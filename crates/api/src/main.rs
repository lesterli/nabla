use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use clap::Parser;
use nabla_adapters::{AgentAdapter, ApiAdapter, LocalCliAdapter};
use nabla_contracts::{ProjectBrief, ScreeningDecision};
use nabla_service::TopicAgentService;
use nabla_sources::{ArxivSource, CompositeCollector, OpenAlexSource, PubMedSource};
use nabla_storage::SqliteStorage;
use serde::Deserialize;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;

type AppState = Arc<ServerConfig>;
type ApiResult<T> = Result<T, (StatusCode, String)>;

#[derive(Debug, Clone)]
struct ServerConfig {
    db: String,
    artifacts_dir: String,
    openalex_limit: usize,
    pubmed_limit: usize,
    arxiv_limit: usize,
    adapter: String,
    api_key: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
}

fn err(e: impl std::fmt::Display) -> (StatusCode, String) {
    let msg = e.to_string();
    tracing::error!("{msg}");
    (StatusCode::INTERNAL_SERVER_ERROR, msg)
}

async fn run_blocking<T, F>(f: F) -> ApiResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(err)?
        .map_err(err)
}

fn build_adapter(config: &ServerConfig) -> Result<Box<dyn AgentAdapter>> {
    match config.adapter.as_str() {
        "codex" => Ok(Box::new(LocalCliAdapter::codex())),
        "claude" => Ok(Box::new(LocalCliAdapter::claude())),
        "test" => Ok(Box::new(nabla_adapters::TestAdapter)),
        "anthropic" => {
            let api_key = config
                .api_key
                .clone()
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                .ok_or_else(|| {
                    anyhow::anyhow!("anthropic adapter requires --api-key or ANTHROPIC_API_KEY env")
                })?;
            Ok(Box::new(ApiAdapter::anthropic(
                api_key,
                config.model.clone(),
                config.base_url.clone(),
            )))
        }
        "openai" => {
            let api_key = config
                .api_key
                .clone()
                .or_else(|| std::env::var("OPENAI_API_KEY").ok())
                .ok_or_else(|| {
                    anyhow::anyhow!("openai adapter requires --api-key or OPENAI_API_KEY env")
                })?;
            Ok(Box::new(ApiAdapter::openai(
                api_key,
                config.model.clone(),
                config.base_url.clone(),
            )))
        }
        other => anyhow::bail!(
            "unsupported adapter: {other} (options: test, codex, claude, anthropic, openai)"
        ),
    }
}

fn build_service(config: &ServerConfig) -> Result<TopicAgentService> {
    let storage = SqliteStorage::open(&config.db, &config.artifacts_dir)?;
    let collector = Box::new(CompositeCollector::new(vec![
        Box::new(PubMedSource::new(config.pubmed_limit)),
        Box::new(OpenAlexSource::new(config.openalex_limit)),
        Box::new(ArxivSource::new(config.arxiv_limit)),
    ]));
    let adapter = build_adapter(config)?;
    Ok(TopicAgentService::new(storage, collector, adapter))
}

#[derive(Debug, Parser)]
#[command(name = "nabla-server", about = "Topic-agent HTTP server")]
struct Args {
    #[arg(long, default_value_t = 3001)]
    port: u16,

    #[arg(long, default_value = ".nabla/runs.db")]
    db: String,

    #[arg(long, default_value = ".nabla/artifacts")]
    artifacts_dir: String,

    #[arg(long, default_value_t = 5)]
    openalex_limit: usize,

    #[arg(long, default_value_t = 5)]
    pubmed_limit: usize,

    #[arg(long, default_value_t = 5)]
    arxiv_limit: usize,

    #[arg(long, default_value = "test")]
    adapter: String,

    /// API key for anthropic/openai adapters (overrides env vars)
    #[arg(long)]
    api_key: Option<String>,

    /// Model name for anthropic/openai adapters
    #[arg(long)]
    model: Option<String>,

    /// Base URL for API adapters (overrides default endpoints)
    #[arg(long)]
    base_url: Option<String>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nabla_api=info,tower_http=info".parse().unwrap()),
        )
        .init();

    let args = Args::parse();

    info!(
        adapter = args.adapter,
        db = args.db,
        "starting nabla-server"
    );
    let config = Arc::new(ServerConfig {
        db: args.db,
        artifacts_dir: args.artifacts_dir,
        openalex_limit: args.openalex_limit,
        pubmed_limit: args.pubmed_limit,
        arxiv_limit: args.arxiv_limit,
        adapter: args.adapter,
        api_key: args.api_key,
        model: args.model,
        base_url: args.base_url,
    });
    let _ = build_service(&config)?;

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/api/runs", post(create_run))
        .route("/api/runs/{run_id}", get(get_run))
        .route("/api/projects/{id}/runs", get(list_runs))
        .route("/api/projects/{id}/papers", get(list_papers))
        .route("/api/projects/{id}/screening", get(list_screening))
        .route("/api/projects/{id}/screening", put(update_screening))
        .route("/api/projects/{id}/topics", get(list_topics))
        .route("/api/projects/{id}/rerun", post(rerun_propose))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(config);

    let addr = format!("0.0.0.0:{}", args.port);
    info!("listening on {addr}");

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        axum::serve(listener, app).await?;
        Ok(())
    })
}

async fn create_run(
    State(config): State<AppState>,
    Json(brief): Json<ProjectBrief>,
) -> ApiResult<Json<serde_json::Value>> {
    info!(project_id = brief.id, "POST /api/runs — creating run");
    let submit_config = Arc::clone(&config);
    let submit_brief = brief.clone();
    let manifest = run_blocking(move || {
        let service = build_service(&submit_config)?;
        service.submit_run(&submit_brief)
    })
    .await?;
    let run_id = manifest.run_id.clone();
    let background_config = Arc::clone(&config);
    let background_brief = brief;
    let background_run_id = run_id.clone();

    tokio::task::spawn_blocking(move || match build_service(&background_config) {
        Ok(service) => {
            if let Err(error) = service.execute_submitted_run(&background_brief, &background_run_id)
            {
                tracing::error!(
                    run_id = background_run_id,
                    error = format!("{error:#}"),
                    "background run failed"
                );
            }
        }
        Err(error) => {
            tracing::error!(run_id = background_run_id, error = %error, "failed to build service for background run");
        }
    });

    info!(run_id, "run submitted");
    Ok(Json(serde_json::to_value(manifest).unwrap()))
}

async fn get_run(
    State(config): State<AppState>,
    Path(run_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let run_id_for_query = run_id.clone();
    let manifest = run_blocking(move || {
        let service = build_service(&config)?;
        service.get_run(&run_id_for_query)
    })
    .await?;
    match manifest {
        Some(m) => Ok(Json(serde_json::to_value(m).unwrap())),
        None => Err((StatusCode::NOT_FOUND, format!("run {run_id} not found"))),
    }
}

async fn list_runs(
    State(config): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let project_id_for_query = id.clone();
    let runs = run_blocking(move || {
        let service = build_service(&config)?;
        service.list_runs(&project_id_for_query)
    })
    .await?;
    info!(project_id = id, count = runs.len(), "list_runs");
    Ok(Json(serde_json::to_value(runs).unwrap()))
}

async fn list_papers(
    State(config): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let project_id_for_query = id.clone();
    let papers = run_blocking(move || {
        let service = build_service(&config)?;
        service.list_project_papers(&project_id_for_query)
    })
    .await?;
    info!(project_id = id, count = papers.len(), "list_papers");
    Ok(Json(serde_json::to_value(papers).unwrap()))
}

async fn list_screening(
    State(config): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let project_id_for_query = id.clone();
    let decisions = run_blocking(move || {
        let service = build_service(&config)?;
        service.list_project_screening(&project_id_for_query)
    })
    .await?;
    info!(project_id = id, count = decisions.len(), "list_screening");
    Ok(Json(serde_json::to_value(decisions).unwrap()))
}

#[derive(Deserialize)]
struct ScreeningUpdate {
    decisions: Vec<ScreeningDecision>,
}

async fn update_screening(
    State(config): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ScreeningUpdate>,
) -> ApiResult<StatusCode> {
    info!(
        project_id = id,
        count = body.decisions.len(),
        "update_screening"
    );
    run_blocking(move || {
        let service = build_service(&config)?;
        for decision in body.decisions {
            service.update_screening_decision(decision)?;
        }
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_topics(
    State(config): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let project_id_for_query = id.clone();
    let topics = run_blocking(move || {
        let service = build_service(&config)?;
        service.list_project_topics(&project_id_for_query)
    })
    .await?;
    info!(project_id = id, count = topics.len(), "list_topics");
    Ok(Json(serde_json::to_value(topics).unwrap()))
}

async fn rerun_propose(
    State(config): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    info!(project_id = id, "POST rerun_propose");
    let output = run_blocking(move || {
        let service = build_service(&config)?;
        service.rerun_propose(&id)
    })
    .await?;

    info!(topics = output.topics.len(), "rerun completed");
    Ok(Json(serde_json::to_value(output).unwrap()))
}
