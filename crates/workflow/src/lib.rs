use anyhow::{Context, Result};
use nabla_adapters::AgentAdapter;
use nabla_contracts::{Phase, ProjectBrief, RunManifest, RunStatus, ScreeningDecision, TopicCandidate};
use nabla_sources::PaperCollector;
use nabla_storage::SqliteStorage;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct TopicWorkflow<'a> {
    collector: &'a dyn PaperCollector,
    adapter: &'a dyn AgentAdapter,
    storage: &'a SqliteStorage,
}

pub struct WorkflowOutput {
    pub run_manifest: RunManifest,
    pub artifact_dir: std::path::PathBuf,
    pub screening: Vec<ScreeningDecision>,
    pub topics: Vec<TopicCandidate>,
}

impl<'a> TopicWorkflow<'a> {
    pub fn new(
        collector: &'a dyn PaperCollector,
        adapter: &'a dyn AgentAdapter,
        storage: &'a SqliteStorage,
    ) -> Self {
        Self {
            collector,
            adapter,
            storage,
        }
    }

    pub fn run(&self, brief: &ProjectBrief) -> Result<WorkflowOutput> {
        let run_id = generate_run_id();
        let mut manifest = RunManifest {
            run_id: run_id.clone(),
            project_id: brief.id.clone(),
            phase: Phase::Frame,
            created_at: current_timestamp(),
            status: RunStatus::Running,
        };

        self.storage.upsert_project(brief)?;
        self.storage.upsert_run_manifest(&manifest)?;

        self.run_phase(&mut manifest, Phase::Frame, || {
            self.storage.write_json_artifact(&run_id, "project_brief.json", brief)?;
            Ok(())
        })?;

        let papers = self.run_phase(&mut manifest, Phase::Collect, || {
            let papers = self.collector.collect(brief).context("collect papers")?;
            self.storage.persist_papers(&brief.id, &papers)?;
            self.storage.write_json_artifact(&run_id, "paper_set.json", &papers)?;
            Ok(papers)
        })?;

        let screening = self.run_phase(&mut manifest, Phase::Screen, || {
            let screening = self
                .adapter
                .screen(brief, &papers)
                .with_context(|| format!("screen papers with adapter {}", self.adapter.name()))?;
            self.storage.persist_screening_decisions(&screening)?;
            self.storage.write_json_artifact(&run_id, "screening.json", &screening)?;
            Ok(screening)
        })?;

        let topics = self.run_phase(&mut manifest, Phase::Propose, || {
            let topics = self
                .adapter
                .propose(brief, &papers, &screening)
                .with_context(|| format!("propose topics with adapter {}", self.adapter.name()))?;
            self.storage.persist_topic_candidates(&topics)?;
            self.storage.write_json_artifact(&run_id, "topic_brief.json", &topics)?;
            Ok(topics)
        })?;

        manifest.phase = Phase::Done;
        manifest.status = RunStatus::Completed;
        self.storage.upsert_run_manifest(&manifest)?;
        self.storage
            .write_json_artifact(&run_id, "run_manifest.json", &manifest)?;

        Ok(WorkflowOutput {
            run_manifest: manifest,
            artifact_dir: self.storage.artifact_dir(&run_id),
            screening,
            topics,
        })
    }

    fn run_phase<T, F>(&self, manifest: &mut RunManifest, phase: Phase, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T>,
    {
        manifest.phase = phase;
        manifest.status = RunStatus::Running;
        self.storage.upsert_run_manifest(manifest)?;
        match f() {
            Ok(value) => {
                manifest.status = RunStatus::Completed;
                self.storage.upsert_run_manifest(manifest)?;
                Ok(value)
            }
            Err(error) => {
                manifest.status = RunStatus::Failed;
                self.storage.upsert_run_manifest(manifest)?;
                let _ = self.storage.write_text_artifact(
                    &manifest.run_id,
                    "error.txt",
                    &format!("phase={phase}\nerror={error:#}"),
                );
                let _ = self
                    .storage
                    .write_json_artifact(&manifest.run_id, "run_manifest.json", manifest);
                Err(error)
            }
        }
    }
}

fn current_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs()
        .to_string()
}

fn generate_run_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    format!("run-{nanos}")
}

#[cfg(test)]
mod tests {
    use super::TopicWorkflow;
    use anyhow::Result;
    use nabla_adapters::AgentAdapter;
    use nabla_contracts::{
        PaperId, PaperRecord, ProjectBrief, ScreeningDecision, ScreeningLabel, TopicCandidate,
    };
    use nabla_sources::StaticCollector;
    use nabla_storage::SqliteStorage;
    use tempfile::TempDir;

    struct TestAdapter;

    impl AgentAdapter for TestAdapter {
        fn name(&self) -> &'static str {
            "test"
        }

        fn screen(
            &self,
            brief: &ProjectBrief,
            papers: &[PaperRecord],
        ) -> Result<Vec<ScreeningDecision>> {
            Ok(papers
                .iter()
                .map(|paper| ScreeningDecision {
                    project_id: brief.id.clone(),
                    paper_id: paper.paper_id.clone(),
                    label: ScreeningLabel::Include,
                    rationale: "Included by test adapter".into(),
                    tags: vec!["test".into()],
                    confidence: Some(1.0),
                })
                .collect())
        }

        fn propose(
            &self,
            brief: &ProjectBrief,
            papers: &[PaperRecord],
            _decisions: &[ScreeningDecision],
        ) -> Result<Vec<TopicCandidate>> {
            Ok(vec![TopicCandidate {
                id: "topic-1".into(),
                project_id: brief.id.clone(),
                title: "Test topic".into(),
                why_now: "Test rationale".into(),
                scope: "Test scope".into(),
                representative_paper_ids: papers.iter().take(1).map(|paper| paper.paper_id.clone()).collect(),
                entry_risk: "Test risk".into(),
                fallback_scope: "Test fallback scope".into(),
            }])
        }
    }

    #[test]
    fn runs_end_to_end_with_static_collector() {
        let temp = TempDir::new().unwrap();
        let storage = SqliteStorage::open(temp.path().join("runs.db"), temp.path().join("artifacts")).unwrap();
        let collector = StaticCollector::new(vec![PaperRecord {
            paper_id: PaperId::DerivedHash("p1".into()),
            title: "Neural operator methods for PDE discovery".into(),
            authors: vec!["Alice".into()],
            year: Some(2024),
            abstract_text: Some("Neural operator benchmark for PDE systems".into()),
            source_url: None,
            source_name: "fixture".into(),
        }]);
        let adapter = TestAdapter;
        let brief = ProjectBrief {
            id: "proj-1".into(),
            goal: "neural operator topic selection".into(),
            constraints: vec!["recent papers".into()],
            keywords: vec!["neural operator".into(), "pde".into()],
            date_range: None,
        };
        let workflow = TopicWorkflow::new(&collector, &adapter, &storage);
        let output = workflow.run(&brief).unwrap();
        assert!(!output.screening.is_empty());
        assert!(!output.topics.is_empty());
        assert!(output.artifact_dir.join("topic_brief.json").exists());
    }
}
