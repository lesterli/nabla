use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use clap::Parser;
use nabla_adapters::{AgentAdapter, LocalCliAdapter};
use nabla_contracts::{ProjectBrief, ScreeningDecision};
use nabla_service::TopicAgentService;
use nabla_sources::{ArxivSource, CompositeCollector, OpenAlexSource};
use nabla_storage::SqliteStorage;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;

type AppState = Arc<Mutex<TopicAgentService>>;
type ApiResult<T> = Result<T, (StatusCode, String)>;

fn err(e: impl std::fmt::Display) -> (StatusCode, String) {
    let msg = e.to_string();
    tracing::error!("{msg}");
    (StatusCode::INTERNAL_SERVER_ERROR, msg)
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

    #[arg(long, default_value_t = 10)]
    openalex_limit: usize,

    #[arg(long, default_value_t = 10)]
    arxiv_limit: usize,

    #[arg(long, default_value = "test")]
    adapter: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nabla_api=info,tower_http=info".parse().unwrap()),
        )
        .init();

    let args = Args::parse();

    let storage = SqliteStorage::open(&args.db, &args.artifacts_dir)?;
    let collector = Box::new(CompositeCollector::new(vec![
        Box::new(OpenAlexSource::new(args.openalex_limit)),
        Box::new(ArxivSource::new(args.arxiv_limit)),
    ]));
    let adapter: Box<dyn AgentAdapter> = match args.adapter.as_str() {
        "codex" => Box::new(LocalCliAdapter::codex()),
        "claude" => Box::new(LocalCliAdapter::claude()),
        "test" => Box::new(nabla_adapters::TestAdapter),
        other => anyhow::bail!("unsupported adapter: {other}"),
    };

    info!(adapter = args.adapter, db = args.db, "starting nabla-server");
    let service = Arc::new(Mutex::new(TopicAgentService::new(storage, collector, adapter)));

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
        .with_state(service);

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
    State(svc): State<AppState>,
    Json(brief): Json<ProjectBrief>,
) -> ApiResult<Json<serde_json::Value>> {
    info!(project_id = brief.id, "POST /api/runs — creating run");
    let output = tokio::task::spawn_blocking(move || {
        let svc = svc.lock().unwrap();
        svc.create_run(&brief)
    })
    .await
    .map_err(err)?
    .map_err(err)?;

    info!(
        run_id = output.run_manifest.run_id,
        topics = output.topics.len(),
        screening = output.screening.len(),
        "run completed"
    );
    Ok(Json(serde_json::to_value(output).unwrap()))
}

async fn get_run(
    State(svc): State<AppState>,
    Path(run_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let svc = svc.lock().unwrap();
    let manifest = svc.get_run(&run_id).map_err(err)?;
    match manifest {
        Some(m) => Ok(Json(serde_json::to_value(m).unwrap())),
        None => Err((StatusCode::NOT_FOUND, format!("run {run_id} not found"))),
    }
}

async fn list_runs(
    State(svc): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let svc = svc.lock().unwrap();
    let runs = svc.list_runs(&id).map_err(err)?;
    info!(project_id = id, count = runs.len(), "list_runs");
    Ok(Json(serde_json::to_value(runs).unwrap()))
}

async fn list_papers(
    State(svc): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let svc = svc.lock().unwrap();
    let papers = svc.list_project_papers(&id).map_err(err)?;
    info!(project_id = id, count = papers.len(), "list_papers");
    Ok(Json(serde_json::to_value(papers).unwrap()))
}

async fn list_screening(
    State(svc): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let svc = svc.lock().unwrap();
    let decisions = svc.list_project_screening(&id).map_err(err)?;
    info!(project_id = id, count = decisions.len(), "list_screening");
    Ok(Json(serde_json::to_value(decisions).unwrap()))
}

#[derive(Deserialize)]
struct ScreeningUpdate {
    decisions: Vec<ScreeningDecision>,
}

async fn update_screening(
    State(svc): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ScreeningUpdate>,
) -> ApiResult<StatusCode> {
    info!(project_id = id, count = body.decisions.len(), "update_screening");
    let svc = svc.lock().unwrap();
    for decision in body.decisions {
        svc.update_screening_decision(decision).map_err(err)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn list_topics(
    State(svc): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let svc = svc.lock().unwrap();
    let topics = svc.list_project_topics(&id).map_err(err)?;
    info!(project_id = id, count = topics.len(), "list_topics");
    Ok(Json(serde_json::to_value(topics).unwrap()))
}

async fn rerun_propose(
    State(svc): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    info!(project_id = id, "POST rerun_propose");
    let output = tokio::task::spawn_blocking(move || {
        let svc = svc.lock().unwrap();
        svc.rerun_propose(&id)
    })
    .await
    .map_err(err)?
    .map_err(err)?;

    info!(topics = output.topics.len(), "rerun completed");
    Ok(Json(serde_json::to_value(output).unwrap()))
}
