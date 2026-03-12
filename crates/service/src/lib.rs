use anyhow::{Context, Result};
use nabla_adapters::AgentAdapter;
use nabla_contracts::{
    PaperRecord, Phase, ProjectBrief, RunManifest, RunStatus, ScreeningDecision, TopicCandidate,
};
use nabla_sources::PaperCollector;
use nabla_storage::SqliteStorage;
use nabla_workflow::{TopicWorkflow, WorkflowOutput};
use std::path::{Path, PathBuf};

/// Application service for the topic-agent workflow.
///
/// Owns storage, paper collector, and agent adapter. Exposes domain operations
/// that any transport layer (CLI, HTTP, Tauri) can call directly.
pub struct TopicAgentService {
    storage: SqliteStorage,
    collector: Box<dyn PaperCollector>,
    adapter: Box<dyn AgentAdapter>,
}

impl TopicAgentService {
    pub fn new(
        storage: SqliteStorage,
        collector: Box<dyn PaperCollector>,
        adapter: Box<dyn AgentAdapter>,
    ) -> Self {
        Self {
            storage,
            collector,
            adapter,
        }
    }

    /// Run the full topic-agent workflow for a project brief.
    pub fn create_run(&self, brief: &ProjectBrief) -> Result<WorkflowOutput> {
        let workflow = TopicWorkflow::new(
            self.collector.as_ref(),
            self.adapter.as_ref(),
            &self.storage,
        );
        workflow.run(brief)
    }

    /// Fetch a single run manifest by ID.
    pub fn get_run(&self, run_id: &str) -> Result<Option<RunManifest>> {
        self.storage.get_run_manifest(run_id)
    }

    /// List all runs for a project, newest first.
    pub fn list_runs(&self, project_id: &str) -> Result<Vec<RunManifest>> {
        self.storage.list_run_manifests(project_id)
    }

    /// List all collected papers for a project.
    pub fn list_project_papers(&self, project_id: &str) -> Result<Vec<PaperRecord>> {
        self.storage.list_papers(project_id)
    }

    /// List all screening decisions for a project.
    pub fn list_project_screening(&self, project_id: &str) -> Result<Vec<ScreeningDecision>> {
        self.storage.list_screening_decisions(project_id)
    }

    /// List all topic candidates for a project.
    pub fn list_project_topics(&self, project_id: &str) -> Result<Vec<TopicCandidate>> {
        self.storage.list_topic_candidates(project_id)
    }

    /// Path to artifacts for a given run.
    pub fn artifact_dir(&self, run_id: &str) -> PathBuf {
        self.storage.artifact_dir(run_id)
    }

    /// Root path for all artifacts.
    pub fn artifact_root(&self) -> &Path {
        self.storage.artifact_root()
    }

    /// Update a single screening decision (label change from the UI).
    pub fn update_screening_decision(&self, decision: ScreeningDecision) -> Result<()> {
        self.storage.persist_screening_decisions(&[decision])
    }

    /// Re-run the propose phase with current screening decisions.
    /// Deletes old topics, calls adapter.propose(), persists new ones,
    /// and creates a new RunManifest.
    pub fn rerun_propose(&self, project_id: &str) -> Result<WorkflowOutput> {
        let brief = self
            .storage
            .get_project(project_id)?
            .ok_or_else(|| anyhow::anyhow!("project not found: {project_id}"))?;
        let papers = self.storage.list_papers(project_id)?;
        let decisions = self.storage.list_screening_decisions(project_id)?;

        let topics = self
            .adapter
            .propose(&brief, &papers, &decisions)
            .context("rerun propose")?;

        self.storage.delete_topic_candidates(project_id)?;
        self.storage.persist_topic_candidates(&topics)?;

        let run_id = format!(
            "run-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos()
        );
        let manifest = RunManifest {
            run_id,
            project_id: project_id.to_string(),
            phase: Phase::Done,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_secs()
                .to_string(),
            status: RunStatus::Completed,
        };
        self.storage.upsert_run_manifest(&manifest)?;

        Ok(WorkflowOutput {
            run_manifest: manifest,
            artifact_dir: self.storage.artifact_dir(&"rerun"),
            screening: decisions,
            topics,
        })
    }

    /// Access the underlying storage (for advanced use by adapters).
    pub fn storage(&self) -> &SqliteStorage {
        &self.storage
    }
}

#[cfg(test)]
mod tests {
    use super::TopicAgentService;
    use anyhow::Result;
    use nabla_adapters::AgentAdapter;
    use nabla_contracts::{
        PaperId, PaperRecord, ProjectBrief, ScreeningDecision, ScreeningLabel, TopicCandidate,
    };
    use nabla_sources::StaticCollector;
    use nabla_storage::SqliteStorage;
    use tempfile::TempDir;

    struct StubAdapter;

    impl AgentAdapter for StubAdapter {
        fn name(&self) -> &'static str {
            "stub"
        }

        fn screen(
            &self,
            brief: &ProjectBrief,
            papers: &[PaperRecord],
        ) -> Result<Vec<ScreeningDecision>> {
            Ok(papers
                .iter()
                .map(|p| ScreeningDecision {
                    project_id: brief.id.clone(),
                    paper_id: p.paper_id.clone(),
                    label: ScreeningLabel::Include,
                    rationale: "stub".into(),
                    tags: vec!["test".into()],
                    confidence: Some(1.0),
                })
                .collect())
        }

        fn propose(
            &self,
            brief: &ProjectBrief,
            _papers: &[PaperRecord],
            _decisions: &[ScreeningDecision],
        ) -> Result<Vec<TopicCandidate>> {
            Ok(vec![TopicCandidate {
                id: "topic-1".into(),
                project_id: brief.id.clone(),
                title: "Stub Topic".into(),
                why_now: "reason".into(),
                scope: "scope".into(),
                representative_paper_ids: vec![],
                entry_risk: "low".into(),
                fallback_scope: "wider".into(),
            }])
        }
    }

    fn make_service(temp: &TempDir) -> TopicAgentService {
        let storage = SqliteStorage::open(
            temp.path().join("runs.db"),
            temp.path().join("artifacts"),
        )
        .unwrap();
        let collector = Box::new(StaticCollector::new(vec![PaperRecord {
            paper_id: PaperId::DerivedHash("p1".into()),
            title: "Test Paper".into(),
            authors: vec!["Alice".into()],
            year: Some(2024),
            abstract_text: Some("Abstract".into()),
            source_url: None,
            source_name: "test".into(),
        }]));
        let adapter: Box<dyn AgentAdapter> = Box::new(StubAdapter);
        TopicAgentService::new(storage, collector, adapter)
    }

    #[test]
    fn create_run_then_query_back() {
        let temp = TempDir::new().unwrap();
        let svc = make_service(&temp);
        let brief = ProjectBrief {
            id: "proj-1".into(),
            goal: "test goal".into(),
            constraints: vec![],
            keywords: vec!["ml".into()],
            date_range: None,
        };

        let output = svc.create_run(&brief).unwrap();
        assert_eq!(output.topics.len(), 1);

        // query back through the service
        let run = svc.get_run(&output.run_manifest.run_id).unwrap();
        assert!(run.is_some());

        let runs = svc.list_runs("proj-1").unwrap();
        assert_eq!(runs.len(), 1);

        let papers = svc.list_project_papers("proj-1").unwrap();
        assert_eq!(papers.len(), 1);

        let screening = svc.list_project_screening("proj-1").unwrap();
        assert_eq!(screening.len(), 1);

        let topics = svc.list_project_topics("proj-1").unwrap();
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].title, "Stub Topic");
    }

    #[test]
    fn update_screening_and_rerun_propose() {
        let temp = TempDir::new().unwrap();
        let svc = make_service(&temp);
        let brief = ProjectBrief {
            id: "proj-2".into(),
            goal: "test goal".into(),
            constraints: vec![],
            keywords: vec!["ml".into()],
            date_range: None,
        };

        let output = svc.create_run(&brief).unwrap();
        assert_eq!(output.topics.len(), 1);

        // update a screening decision
        let mut decision = svc.list_project_screening("proj-2").unwrap().remove(0);
        decision.label = ScreeningLabel::Exclude;
        svc.update_screening_decision(decision).unwrap();

        let updated = svc.list_project_screening("proj-2").unwrap();
        assert_eq!(updated[0].label, ScreeningLabel::Exclude);

        // rerun propose
        let rerun_output = svc.rerun_propose("proj-2").unwrap();
        assert_eq!(rerun_output.topics.len(), 1);
        assert_eq!(rerun_output.topics[0].title, "Stub Topic");
    }
}
