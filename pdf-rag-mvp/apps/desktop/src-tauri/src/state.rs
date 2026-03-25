use nabla_pdf_rag_contracts::LibraryId;
use nabla_pdf_rag_retrieval::LanceStore;
use nabla_pdf_rag_storage::{run_migrations, SqliteRepository};
use rusqlite::Connection;
use tokio::sync::Mutex;

const DB_PATH: &str = "nabla.db";
const LANCE_PATH: &str = "nabla.lance";

/// Embedding dimensions — must match the embedder in use.
/// HashEmbedder = 384, GLM embedding-3 = 1024, OpenAI = 1536.
pub const DEFAULT_DIM: i32 = 384;

pub const DEFAULT_LIBRARY_ID: &str = "lib-default";

pub struct AppState {
    pub repo: SqliteRepository,
    pub lance: Mutex<Option<LanceStore>>,
}

impl AppState {
    pub fn new() -> anyhow::Result<Self> {
        let conn = Connection::open(DB_PATH)?;
        run_migrations(&conn)?;

        // Ensure default library exists
        let lib = nabla_pdf_rag_contracts::LibraryRecord {
            id: LibraryId::new(DEFAULT_LIBRARY_ID),
            name: "default".into(),
            root_dir: ".".into(),
            created_at: String::new(),
            prompt_template: None,
        };
        let _ = repo_ref(&conn).insert_library_conn(&conn, &lib);

        Ok(Self {
            repo: SqliteRepository::new(conn),
            lance: Mutex::new(None),
        })
    }

    pub async fn lance(&self) -> anyhow::Result<LanceStore> {
        let mut guard = self.lance.lock().await;
        if guard.is_none() {
            *guard = Some(LanceStore::open(LANCE_PATH, DEFAULT_DIM).await?);
        }
        // Open a fresh handle (LanceStore is cheap — just a Connection wrapper)
        LanceStore::open(LANCE_PATH, DEFAULT_DIM).await
    }
}

// Helper to create library with raw connection before SqliteRepository takes ownership
fn repo_ref(_conn: &Connection) -> TempRepo {
    TempRepo
}

struct TempRepo;

impl TempRepo {
    fn insert_library_conn(
        &self,
        conn: &Connection,
        lib: &nabla_pdf_rag_contracts::LibraryRecord,
    ) -> anyhow::Result<()> {
        conn.execute(
            "INSERT OR IGNORE INTO libraries (id, name, root_dir, created_at, prompt_template) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![lib.id.as_str(), lib.name, lib.root_dir, lib.created_at, lib.prompt_template],
        )?;
        Ok(())
    }
}
