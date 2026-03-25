use std::sync::RwLock;

use nabla_pdf_rag_contracts::LibraryId;
use nabla_pdf_rag_core::{Embedder, LlmClient};
use nabla_pdf_rag_embedder::{ApiEmbedder, HashEmbedder};
use nabla_pdf_rag_llm::{ApiLlmClient, ApiProvider, LocalCliLlmClient};
use nabla_pdf_rag_retrieval::LanceStore;
use nabla_pdf_rag_storage::{run_migrations, SqliteRepository};
use rusqlite::Connection;
use tokio::sync::Mutex;

use crate::config::{load_config, AppConfig};

const DB_PATH: &str = "nabla.db";
const LANCE_PATH: &str = "nabla.lance";

pub const DEFAULT_LIBRARY_ID: &str = "lib-default";

pub struct AppState {
    pub repo: SqliteRepository,
    pub lance: Mutex<Option<LanceStore>>,
    pub config: RwLock<AppConfig>,
}

impl AppState {
    pub fn new() -> anyhow::Result<Self> {
        let conn = Connection::open(DB_PATH)?;
        run_migrations(&conn)?;

        let lib = nabla_pdf_rag_contracts::LibraryRecord {
            id: LibraryId::new(DEFAULT_LIBRARY_ID),
            name: "default".into(),
            root_dir: ".".into(),
            created_at: String::new(),
            prompt_template: None,
        };
        conn.execute(
            "INSERT OR IGNORE INTO libraries (id, name, root_dir, created_at, prompt_template) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![lib.id.as_str(), lib.name, lib.root_dir, lib.created_at, lib.prompt_template],
        )?;

        let config = load_config();

        Ok(Self {
            repo: SqliteRepository::new(conn),
            lance: Mutex::new(None),
            config: RwLock::new(config),
        })
    }

    pub fn embedding_dim(&self) -> i32 {
        let config = self.config.read().unwrap();
        config.embedding.dimensions.unwrap_or(384) as i32
    }

    pub async fn lance(&self) -> anyhow::Result<LanceStore> {
        let dim = self.embedding_dim();
        let mut guard = self.lance.lock().await;
        if guard.is_none() {
            *guard = Some(LanceStore::open(LANCE_PATH, dim).await?);
        }
        LanceStore::open(LANCE_PATH, dim).await
    }

    pub fn build_llm(&self) -> Box<dyn LlmClient> {
        let config = self.config.read().unwrap();
        match config.llm.provider.as_str() {
            "openai" => {
                let key = config.llm.api_key.clone().unwrap_or_default();
                Box::new(ApiLlmClient::new(
                    ApiProvider::OpenAi,
                    key,
                    config.llm.model.clone(),
                    config.llm.base_url.clone(),
                    None,
                ))
            }
            "anthropic" => {
                let key = config.llm.api_key.clone().unwrap_or_default();
                Box::new(ApiLlmClient::new(
                    ApiProvider::Anthropic,
                    key,
                    config.llm.model.clone(),
                    config.llm.base_url.clone(),
                    None,
                ))
            }
            // Default: claude CLI
            _ => Box::new(LocalCliLlmClient::new(
                nabla_pdf_rag_llm::local_cli::LocalCliTool::Claude,
                None,
            )),
        }
    }

    pub fn build_embedder(&self) -> Box<dyn Embedder> {
        let config = self.config.read().unwrap();
        match config.embedding.provider.as_str() {
            "api" => {
                let key = config.embedding.api_key.clone().unwrap_or_default();
                Box::new(ApiEmbedder::new(
                    key,
                    config.embedding.base_url.clone(),
                    config.embedding.model.clone(),
                    config.embedding.dimensions,
                ))
            }
            // Default: hash embedder (offline)
            _ => Box::new(HashEmbedder {
                dimensions: config.embedding.dimensions.unwrap_or(384),
            }),
        }
    }
}
