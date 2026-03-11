use anyhow::{Context, Result};
use nabla_contracts::{PaperRecord, ProjectBrief, RunManifest, ScreeningDecision, TopicCandidate};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

pub struct SqliteStorage {
    conn: Connection,
    artifact_root: PathBuf,
}

impl SqliteStorage {
    pub fn open(db_path: impl AsRef<Path>, artifact_root: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = db_path.as_ref().parent() {
            fs::create_dir_all(parent).with_context(|| format!("create db dir {}", parent.display()))?;
        }
        fs::create_dir_all(artifact_root.as_ref())
            .with_context(|| format!("create artifact dir {}", artifact_root.as_ref().display()))?;
        let conn = Connection::open(db_path.as_ref())
            .with_context(|| format!("open sqlite db {}", db_path.as_ref().display()))?;
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

    pub fn write_json_artifact<T: Serialize>(&self, run_id: &str, name: &str, value: &T) -> Result<PathBuf> {
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
                 ON CONFLICT(id) DO UPDATE SET
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

    fn migrate(&self) -> Result<()> {
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
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                title TEXT NOT NULL,
                why_now TEXT NOT NULL,
                scope TEXT NOT NULL,
                representative_paper_ids_json TEXT NOT NULL,
                entry_risk TEXT NOT NULL,
                fallback_scope TEXT NOT NULL
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

#[cfg(test)]
mod tests {
    use super::SqliteStorage;
    use nabla_contracts::{PaperId, PaperRecord, Phase, ProjectBrief, RunManifest, RunStatus};
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
        let path = storage.write_text_artifact("r1", "notes.txt", "ok").unwrap();
        assert!(path.exists());
    }
}

