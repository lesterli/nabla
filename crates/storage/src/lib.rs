use anyhow::{Context, Result};
use nabla_contracts::{
    PaperId, PaperRecord, Phase, ProjectBrief, RunManifest, RunStatus, ScreeningDecision,
    ScreeningLabel, TopicCandidate,
};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct SqliteStorage {
    conn: Connection,
    artifact_root: PathBuf,
}

impl SqliteStorage {
    pub fn open(db_path: impl AsRef<Path>, artifact_root: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = db_path.as_ref().parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create db dir {}", parent.display()))?;
        }
        fs::create_dir_all(artifact_root.as_ref())
            .with_context(|| format!("create artifact dir {}", artifact_root.as_ref().display()))?;
        let conn = Connection::open(db_path.as_ref())
            .with_context(|| format!("open sqlite db {}", db_path.as_ref().display()))?;
        conn.busy_timeout(Duration::from_secs(5))
            .context("set sqlite busy_timeout")?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let storage = Self {
            conn,
            artifact_root: artifact_root.as_ref().to_path_buf(),
        };
        storage.migrate()?;
        Ok(storage)
    }

    pub fn artifact_dir(&self, run_id: &str) -> PathBuf {
        self.artifact_root.join(run_id)
    }

    pub fn write_json_artifact<T: Serialize>(
        &self,
        run_id: &str,
        name: &str,
        value: &T,
    ) -> Result<PathBuf> {
        let path = self.artifact_dir(run_id).join(name);
        fs::create_dir_all(path.parent().expect("artifact file has parent"))
            .with_context(|| format!("create artifact parent for {}", path.display()))?;
        let bytes = serde_json::to_vec_pretty(value).context("serialize artifact json")?;
        fs::write(&path, bytes).with_context(|| format!("write artifact {}", path.display()))?;
        Ok(path)
    }

    pub fn write_text_artifact(&self, run_id: &str, name: &str, text: &str) -> Result<PathBuf> {
        let path = self.artifact_dir(run_id).join(name);
        fs::create_dir_all(path.parent().expect("artifact file has parent"))
            .with_context(|| format!("create artifact parent for {}", path.display()))?;
        fs::write(&path, text).with_context(|| format!("write artifact {}", path.display()))?;
        Ok(path)
    }

    pub fn upsert_project(&self, project: &ProjectBrief) -> Result<()> {
        self.conn.execute(
            "INSERT INTO projects (id, goal, constraints_json, keywords_json, date_range_json)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
               goal=excluded.goal,
               constraints_json=excluded.constraints_json,
               keywords_json=excluded.keywords_json,
               date_range_json=excluded.date_range_json",
            params![
                project.id,
                project.goal,
                serde_json::to_string(&project.constraints)?,
                serde_json::to_string(&project.keywords)?,
                serde_json::to_string(&project.date_range)?,
            ],
        )?;
        Ok(())
    }

    pub fn persist_papers(&self, project_id: &str, papers: &[PaperRecord]) -> Result<()> {
        for paper in papers {
            self.conn.execute(
                "INSERT INTO papers
                 (project_id, paper_id, paper_id_json, title, authors_json, year, abstract_text, source_url, source_name)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(project_id, paper_id) DO UPDATE SET
                   title=excluded.title,
                   authors_json=excluded.authors_json,
                   year=excluded.year,
                   abstract_text=excluded.abstract_text,
                   source_url=excluded.source_url,
                   source_name=excluded.source_name",
                params![
                    project_id,
                    paper.paper_id.as_key(),
                    serde_json::to_string(&paper.paper_id)?,
                    paper.title,
                    serde_json::to_string(&paper.authors)?,
                    paper.year.map(i64::from),
                    paper.abstract_text,
                    paper.source_url,
                    paper.source_name,
                ],
            )?;
        }
        Ok(())
    }

    pub fn persist_screening_decisions(&self, decisions: &[ScreeningDecision]) -> Result<()> {
        for decision in decisions {
            self.conn.execute(
                "INSERT INTO screening_decisions
                 (project_id, paper_id, label, rationale, tags_json, confidence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(project_id, paper_id) DO UPDATE SET
                   label=excluded.label,
                   rationale=excluded.rationale,
                   tags_json=excluded.tags_json,
                   confidence=excluded.confidence",
                params![
                    decision.project_id,
                    decision.paper_id.as_key(),
                    format!("{:?}", decision.label),
                    decision.rationale,
                    serde_json::to_string(&decision.tags)?,
                    decision.confidence,
                ],
            )?;
        }
        Ok(())
    }

    pub fn persist_topic_candidates(&self, candidates: &[TopicCandidate]) -> Result<()> {
        for candidate in candidates {
            self.conn.execute(
                "INSERT INTO topic_candidates
                 (id, project_id, title, why_now, scope, representative_paper_ids_json, entry_risk, fallback_scope)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(project_id, id) DO UPDATE SET
                   title=excluded.title,
                   why_now=excluded.why_now,
                   scope=excluded.scope,
                   representative_paper_ids_json=excluded.representative_paper_ids_json,
                   entry_risk=excluded.entry_risk,
                   fallback_scope=excluded.fallback_scope",
                params![
                    candidate.id,
                    candidate.project_id,
                    candidate.title,
                    candidate.why_now,
                    candidate.scope,
                    serde_json::to_string(&candidate.representative_paper_ids)?,
                    candidate.entry_risk,
                    candidate.fallback_scope,
                ],
            )?;
        }
        Ok(())
    }

    pub fn upsert_run_manifest(&self, manifest: &RunManifest) -> Result<()> {
        self.conn.execute(
            "INSERT INTO run_manifests (run_id, project_id, phase, created_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(run_id) DO UPDATE SET
               project_id=excluded.project_id,
               phase=excluded.phase,
               created_at=excluded.created_at,
               status=excluded.status",
            params![
                manifest.run_id,
                manifest.project_id,
                manifest.phase.as_str(),
                manifest.created_at,
                manifest.status.as_str(),
            ],
        )?;
        Ok(())
    }

    // ── query methods ──────────────────────────────────────────────

    pub fn get_run_manifest(&self, run_id: &str) -> Result<Option<RunManifest>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, project_id, phase, created_at, status
             FROM run_manifests WHERE run_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        match rows.next() {
            Some(Ok((run_id, project_id, phase, created_at, status))) => Ok(Some(RunManifest {
                run_id,
                project_id,
                phase: parse_phase(&phase)?,
                created_at,
                status: parse_run_status(&status)?,
            })),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn list_run_manifests(&self, project_id: &str) -> Result<Vec<RunManifest>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, project_id, phase, created_at, status
             FROM run_manifests WHERE project_id = ?1
             ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (run_id, project_id, phase, created_at, status) = row?;
            Ok(RunManifest {
                run_id,
                project_id,
                phase: parse_phase(&phase)?,
                created_at,
                status: parse_run_status(&status)?,
            })
        })
        .collect()
    }

    pub fn list_papers(&self, project_id: &str) -> Result<Vec<PaperRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT paper_id_json, title, authors_json, year, abstract_text, source_url, source_name
             FROM papers WHERE project_id = ?1",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        rows.map(|row| {
            let (paper_id_json, title, authors_json, year, abstract_text, source_url, source_name) =
                row?;
            Ok(PaperRecord {
                paper_id: serde_json::from_str(&paper_id_json)
                    .context("deserialize paper_id_json")?,
                title,
                authors: serde_json::from_str(&authors_json).context("deserialize authors_json")?,
                year: year.map(|y| y as u16),
                abstract_text,
                source_url,
                source_name,
            })
        })
        .collect()
    }

    pub fn list_screening_decisions(&self, project_id: &str) -> Result<Vec<ScreeningDecision>> {
        let mut stmt = self.conn.prepare(
            "SELECT project_id, paper_id, label, rationale, tags_json, confidence
             FROM screening_decisions WHERE project_id = ?1",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<f64>>(5)?,
            ))
        })?;
        rows.map(|row| {
            let (project_id, paper_id_key, label_str, rationale, tags_json, confidence) = row?;
            Ok(ScreeningDecision {
                project_id,
                paper_id: self.resolve_paper_id(&paper_id_key)?,
                label: parse_screening_label(&label_str)?,
                rationale,
                tags: serde_json::from_str(&tags_json).context("deserialize tags_json")?,
                confidence: confidence.map(|c| c as f32),
            })
        })
        .collect()
    }

    pub fn list_topic_candidates(&self, project_id: &str) -> Result<Vec<TopicCandidate>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, title, why_now, scope, representative_paper_ids_json, entry_risk, fallback_scope
             FROM topic_candidates WHERE project_id = ?1",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;
        rows.map(|row| {
            let (id, project_id, title, why_now, scope, rep_ids_json, entry_risk, fallback_scope) =
                row?;
            Ok(TopicCandidate {
                id,
                project_id,
                title,
                why_now,
                scope,
                representative_paper_ids: serde_json::from_str(&rep_ids_json)
                    .context("deserialize representative_paper_ids_json")?,
                entry_risk,
                fallback_scope,
            })
        })
        .collect()
    }

    /// Look up the full PaperId JSON for a paper_id key stored in screening/topic tables.
    fn resolve_paper_id(&self, paper_id_key: &str) -> Result<PaperId> {
        let mut stmt = self
            .conn
            .prepare("SELECT paper_id_json FROM papers WHERE paper_id = ?1 LIMIT 1")?;
        let mut rows = stmt.query_map(params![paper_id_key], |row| row.get::<_, String>(0))?;
        match rows.next() {
            Some(Ok(json)) => {
                serde_json::from_str(&json).context("deserialize paper_id from papers table")
            }
            _ => parse_paper_id_from_key(paper_id_key),
        }
    }

    pub fn get_project(&self, project_id: &str) -> Result<Option<ProjectBrief>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, goal, constraints_json, keywords_json, date_range_json
             FROM projects WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        match rows.next() {
            Some(Ok((id, goal, constraints_json, keywords_json, date_range_json))) => {
                Ok(Some(ProjectBrief {
                    id,
                    goal,
                    constraints: serde_json::from_str(&constraints_json)
                        .context("deserialize constraints_json")?,
                    keywords: serde_json::from_str(&keywords_json)
                        .context("deserialize keywords_json")?,
                    date_range: date_range_json.and_then(|json| serde_json::from_str(&json).ok()),
                }))
            }
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn delete_topic_candidates(&self, project_id: &str) -> Result<usize> {
        let count = self.conn.execute(
            "DELETE FROM topic_candidates WHERE project_id = ?1",
            params![project_id],
        )?;
        Ok(count)
    }

    pub fn artifact_root(&self) -> &Path {
        &self.artifact_root
    }

    fn migrate(&self) -> Result<()> {
        // Migrate topic_candidates from old schema (id PRIMARY KEY) to
        // composite key (project_id, id). Safe to run repeatedly because
        // the old table won't exist after first migration.
        let has_old_pk: bool = self
            .conn
            .prepare("SELECT sql FROM sqlite_master WHERE type='table' AND name='topic_candidates'")
            .ok()
            .and_then(|mut stmt| stmt.query_row([], |row| row.get::<_, String>(0)).ok())
            .map(|sql| sql.contains("id TEXT PRIMARY KEY"))
            .unwrap_or(false);
        if has_old_pk {
            self.conn.execute_batch(
                "ALTER TABLE topic_candidates RENAME TO _topic_candidates_old;
                 CREATE TABLE topic_candidates (
                     id TEXT NOT NULL,
                     project_id TEXT NOT NULL,
                     title TEXT NOT NULL,
                     why_now TEXT NOT NULL,
                     scope TEXT NOT NULL,
                     representative_paper_ids_json TEXT NOT NULL,
                     entry_risk TEXT NOT NULL,
                     fallback_scope TEXT NOT NULL,
                     PRIMARY KEY (project_id, id)
                 );
                 INSERT INTO topic_candidates SELECT * FROM _topic_candidates_old;
                 DROP TABLE _topic_candidates_old;",
            )?;
        }

        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY,
                goal TEXT NOT NULL,
                constraints_json TEXT NOT NULL,
                keywords_json TEXT NOT NULL,
                date_range_json TEXT
            );

            CREATE TABLE IF NOT EXISTS papers (
                project_id TEXT NOT NULL,
                paper_id TEXT NOT NULL,
                paper_id_json TEXT NOT NULL,
                title TEXT NOT NULL,
                authors_json TEXT NOT NULL,
                year INTEGER,
                abstract_text TEXT,
                source_url TEXT,
                source_name TEXT NOT NULL,
                PRIMARY KEY (project_id, paper_id)
            );

            CREATE TABLE IF NOT EXISTS screening_decisions (
                project_id TEXT NOT NULL,
                paper_id TEXT NOT NULL,
                label TEXT NOT NULL,
                rationale TEXT NOT NULL,
                tags_json TEXT NOT NULL,
                confidence REAL,
                PRIMARY KEY (project_id, paper_id)
            );

            CREATE TABLE IF NOT EXISTS topic_candidates (
                id TEXT NOT NULL,
                project_id TEXT NOT NULL,
                title TEXT NOT NULL,
                why_now TEXT NOT NULL,
                scope TEXT NOT NULL,
                representative_paper_ids_json TEXT NOT NULL,
                entry_risk TEXT NOT NULL,
                fallback_scope TEXT NOT NULL,
                PRIMARY KEY (project_id, id)
            );

            CREATE TABLE IF NOT EXISTS run_manifests (
                run_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                phase TEXT NOT NULL,
                created_at TEXT NOT NULL,
                status TEXT NOT NULL
            );
            ",
        )?;
        Ok(())
    }
}

fn parse_phase(s: &str) -> Result<Phase> {
    match s {
        "frame" => Ok(Phase::Frame),
        "collect" => Ok(Phase::Collect),
        "screen" => Ok(Phase::Screen),
        "propose" => Ok(Phase::Propose),
        "done" => Ok(Phase::Done),
        other => Err(anyhow::anyhow!("unknown phase: {other}")),
    }
}

fn parse_run_status(s: &str) -> Result<RunStatus> {
    match s {
        "pending" => Ok(RunStatus::Pending),
        "running" => Ok(RunStatus::Running),
        "completed" => Ok(RunStatus::Completed),
        "failed" => Ok(RunStatus::Failed),
        other => Err(anyhow::anyhow!("unknown run status: {other}")),
    }
}

fn parse_screening_label(s: &str) -> Result<ScreeningLabel> {
    match s {
        "Include" => Ok(ScreeningLabel::Include),
        "Maybe" => Ok(ScreeningLabel::Maybe),
        "Exclude" => Ok(ScreeningLabel::Exclude),
        other => Err(anyhow::anyhow!("unknown screening label: {other}")),
    }
}

fn parse_paper_id_from_key(key: &str) -> Result<PaperId> {
    if let Some(value) = key.strip_prefix("doi:") {
        Ok(PaperId::Doi(value.to_string()))
    } else if let Some(value) = key.strip_prefix("arxiv:") {
        Ok(PaperId::Arxiv(value.to_string()))
    } else if let Some(value) = key.strip_prefix("openalex:") {
        Ok(PaperId::OpenAlex(value.to_string()))
    } else if let Some(value) = key.strip_prefix("derived:") {
        Ok(PaperId::DerivedHash(value.to_string()))
    } else {
        Err(anyhow::anyhow!("unrecognized paper_id key format: {key}"))
    }
}

#[cfg(test)]
mod tests {
    use super::SqliteStorage;
    use nabla_contracts::{
        PaperId, PaperRecord, Phase, ProjectBrief, RunManifest, RunStatus, ScreeningDecision,
        ScreeningLabel, TopicCandidate,
    };
    use tempfile::TempDir;

    #[test]
    fn writes_artifacts_and_sqlite_rows() {
        let temp = TempDir::new().unwrap();
        let db = temp.path().join("runs.db");
        let artifacts = temp.path().join("artifacts");
        let storage = SqliteStorage::open(&db, &artifacts).unwrap();
        let project = ProjectBrief {
            id: "p1".into(),
            goal: "test".into(),
            constraints: vec![],
            keywords: vec!["ml".into()],
            date_range: None,
        };
        storage.upsert_project(&project).unwrap();
        storage
            .persist_papers(
                &project.id,
                &[PaperRecord {
                    paper_id: PaperId::DerivedHash("a".into()),
                    title: "Paper".into(),
                    authors: vec!["A".into()],
                    year: Some(2024),
                    abstract_text: None,
                    source_url: None,
                    source_name: "test".into(),
                }],
            )
            .unwrap();
        storage
            .upsert_run_manifest(&RunManifest {
                run_id: "r1".into(),
                project_id: project.id.clone(),
                phase: Phase::Collect,
                created_at: "1".into(),
                status: RunStatus::Running,
            })
            .unwrap();
        let path = storage
            .write_text_artifact("r1", "notes.txt", "ok")
            .unwrap();
        assert!(path.exists());
    }

    #[test]
    fn query_round_trips() {
        let temp = TempDir::new().unwrap();
        let storage =
            SqliteStorage::open(temp.path().join("runs.db"), temp.path().join("artifacts"))
                .unwrap();
        let project = ProjectBrief {
            id: "p1".into(),
            goal: "test".into(),
            constraints: vec![],
            keywords: vec!["ml".into()],
            date_range: None,
        };
        storage.upsert_project(&project).unwrap();

        let paper = PaperRecord {
            paper_id: PaperId::Arxiv("2401.00001".into()),
            title: "Test Paper".into(),
            authors: vec!["Alice".into()],
            year: Some(2024),
            abstract_text: Some("Abstract".into()),
            source_url: None,
            source_name: "arxiv".into(),
        };
        storage.persist_papers("p1", &[paper.clone()]).unwrap();

        let decision = ScreeningDecision {
            project_id: "p1".into(),
            paper_id: PaperId::Arxiv("2401.00001".into()),
            label: ScreeningLabel::Include,
            rationale: "Relevant".into(),
            tags: vec!["ml".into()],
            confidence: Some(0.9),
        };
        storage.persist_screening_decisions(&[decision]).unwrap();

        let topic = TopicCandidate {
            id: "topic-1".into(),
            project_id: "p1".into(),
            title: "Test Topic".into(),
            why_now: "Trending".into(),
            scope: "Narrow".into(),
            representative_paper_ids: vec![PaperId::Arxiv("2401.00001".into())],
            entry_risk: "Low".into(),
            fallback_scope: "Broader".into(),
        };
        storage.persist_topic_candidates(&[topic]).unwrap();

        let manifest = RunManifest {
            run_id: "run-1".into(),
            project_id: "p1".into(),
            phase: Phase::Done,
            created_at: "100".into(),
            status: RunStatus::Completed,
        };
        storage.upsert_run_manifest(&manifest).unwrap();

        // query back
        let papers = storage.list_papers("p1").unwrap();
        assert_eq!(papers.len(), 1);
        assert_eq!(papers[0].title, "Test Paper");

        let decisions = storage.list_screening_decisions("p1").unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].label, ScreeningLabel::Include);

        let topics = storage.list_topic_candidates("p1").unwrap();
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].title, "Test Topic");

        let runs = storage.list_run_manifests("p1").unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Completed);

        let run = storage.get_run_manifest("run-1").unwrap();
        assert!(run.is_some());
        assert_eq!(run.unwrap().phase, Phase::Done);

        assert!(storage.get_run_manifest("nonexistent").unwrap().is_none());
    }

    #[test]
    fn get_project_round_trip() {
        let temp = TempDir::new().unwrap();
        let storage =
            SqliteStorage::open(temp.path().join("runs.db"), temp.path().join("artifacts"))
                .unwrap();

        assert!(storage.get_project("nonexistent").unwrap().is_none());

        let project = ProjectBrief {
            id: "p1".into(),
            goal: "test goal".into(),
            constraints: vec!["recent".into()],
            keywords: vec!["ml".into(), "nlp".into()],
            date_range: None,
        };
        storage.upsert_project(&project).unwrap();

        let loaded = storage.get_project("p1").unwrap().unwrap();
        assert_eq!(loaded, project);
    }

    #[test]
    fn delete_topic_candidates_clears_project() {
        let temp = TempDir::new().unwrap();
        let storage =
            SqliteStorage::open(temp.path().join("runs.db"), temp.path().join("artifacts"))
                .unwrap();

        let project = ProjectBrief {
            id: "p1".into(),
            goal: "test".into(),
            constraints: vec![],
            keywords: vec![],
            date_range: None,
        };
        storage.upsert_project(&project).unwrap();
        storage
            .persist_topic_candidates(&[
                TopicCandidate {
                    id: "t1".into(),
                    project_id: "p1".into(),
                    title: "Topic 1".into(),
                    why_now: "w".into(),
                    scope: "s".into(),
                    representative_paper_ids: vec![],
                    entry_risk: "r".into(),
                    fallback_scope: "f".into(),
                },
                TopicCandidate {
                    id: "t2".into(),
                    project_id: "p1".into(),
                    title: "Topic 2".into(),
                    why_now: "w".into(),
                    scope: "s".into(),
                    representative_paper_ids: vec![],
                    entry_risk: "r".into(),
                    fallback_scope: "f".into(),
                },
            ])
            .unwrap();

        assert_eq!(storage.list_topic_candidates("p1").unwrap().len(), 2);
        let deleted = storage.delete_topic_candidates("p1").unwrap();
        assert_eq!(deleted, 2);
        assert_eq!(storage.list_topic_candidates("p1").unwrap().len(), 0);
    }
}
