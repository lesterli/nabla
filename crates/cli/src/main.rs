use anyhow::{Context, Result};
use clap::Parser;
use nabla_adapters::{AgentAdapter, LocalCliAdapter};
use nabla_contracts::ProjectBrief;
use nabla_service::TopicAgentService;
use nabla_sources::{ArxivSource, CompositeCollector, OpenAlexSource};
use nabla_storage::SqliteStorage;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(name = "nabla", about = "Topic-agent CLI")]
struct Args {
    #[arg(long)]
    brief: PathBuf,

    #[arg(long, default_value = ".nabla/runs.db")]
    db: PathBuf,

    #[arg(long, default_value = ".nabla/artifacts")]
    artifacts_dir: PathBuf,

    #[arg(long, default_value_t = 10)]
    openalex_limit: usize,

    #[arg(long, default_value_t = 10)]
    arxiv_limit: usize,

    #[arg(long, default_value = "codex")]
    adapter: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let brief = read_brief(&args.brief)?;

    let storage = SqliteStorage::open(&args.db, &args.artifacts_dir)?;
    let collector = Box::new(CompositeCollector::new(vec![
        Box::new(OpenAlexSource::new(args.openalex_limit)),
        Box::new(ArxivSource::new(args.arxiv_limit)),
    ]));
    let adapter: Box<dyn AgentAdapter> = match args.adapter.as_str() {
        "codex" => Box::new(LocalCliAdapter::codex()),
        "claude" => Box::new(LocalCliAdapter::claude()),
        other => anyhow::bail!("unsupported adapter: {other}"),
    };

    let service = TopicAgentService::new(storage, collector, adapter);
    let output = service.create_run(&brief)?;

    println!("Run completed");
    println!("project_id: {}", brief.id);
    println!("run_id: {}", output.run_manifest.run_id);
    println!("artifacts: {}", output.artifact_dir.display());
    println!("screening_count: {}", output.screening.len());
    println!("topic_count: {}", output.topics.len());
    for topic in output.topics.iter().take(3) {
        println!("- {}", topic.title);
    }

    Ok(())
}

fn read_brief(path: &Path) -> Result<ProjectBrief> {
    let text =
        fs::read_to_string(path).with_context(|| format!("read brief file {}", path.display()))?;
    let brief = serde_json::from_str(&text)
        .with_context(|| format!("parse brief json {}", path.display()))?;
    Ok(brief)
}
