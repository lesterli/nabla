use anyhow::Result;
use clap::{Parser, Subcommand};
use nabla_pdf_rag_contracts::{LibraryId, RetrievalMode, RetrievalQuery};
use nabla_pdf_rag_core::{DEFAULT_DECISIONS, DEFAULT_PIPELINE, REPO_LAYOUT};

#[derive(Debug, Parser)]
#[command(name = "nabla-pdf", about = "PDF RAG MVP architecture helper")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Blueprint,
    Roadmap,
    QueryExample {
        #[arg(long)]
        library_id: String,
        #[arg(long)]
        prompt: String,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Blueprint => print_blueprint(),
        Command::Roadmap => print_roadmap(),
        Command::QueryExample { library_id, prompt } => print_query_example(library_id, prompt)?,
    }
    Ok(())
}

fn print_blueprint() {
    println!("MVP decisions");
    for decision in DEFAULT_DECISIONS {
        println!("- area: {}", decision.area);
        println!("  mvp: {}", decision.mvp_choice);
        println!("  later: {}", decision.later_choice);
        println!("  reason: {}", decision.reason);
    }

    println!();
    println!("Pipeline");
    for step in DEFAULT_PIPELINE {
        println!("- {}: {}", step.name, step.goal);
        println!("  output: {}", step.output);
    }
}

fn print_roadmap() {
    println!("Recommended repo layout");
    for module in REPO_LAYOUT {
        println!("- {}: {}", module.path, module.responsibility);
    }
}

fn print_query_example(library_id: String, prompt: String) -> Result<()> {
    let query = RetrievalQuery {
        library_id: LibraryId::new(library_id),
        prompt,
        max_chunks: 12,
        max_summaries: 6,
        mode: RetrievalMode::Balanced,
    };
    println!("{}", serde_json::to_string_pretty(&query)?);
    Ok(())
}
