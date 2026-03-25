use std::sync::Arc;

use nabla_pdf_rag_retrieval::LanceStore;
use nabla_pdf_rag_storage::{run_migrations, SqliteRepository};
use rusqlite::Connection;

const DB_PATH: &str = "nabla.db";
const LANCE_PATH: &str = "nabla.lance";
const EMBEDDING_DIM: i32 = 1024;

/// Shared application state managed by Tauri.
pub struct AppState {
    pub repo: SqliteRepository,
    pub lance: Arc<tokio::sync::Mutex<Option<LanceStore>>>,
}

impl AppState {
    pub fn new() -> anyhow::Result<Self> {
        let conn = Connection::open(DB_PATH)?;
        run_migrations(&conn)?;
        Ok(Self {
            repo: SqliteRepository::new(conn),
            lance: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    pub async fn lance(&self) -> anyhow::Result<LanceStore> {
        let mut guard = self.lance.lock().await;
        if guard.is_none() {
            *guard = Some(LanceStore::open(LANCE_PATH, EMBEDDING_DIM).await?);
        }
        // Clone is not available, so we open a new connection each time for now.
        // In production, LanceStore should be shared via Arc.
        LanceStore::open(LANCE_PATH, EMBEDDING_DIM).await
    }
}
