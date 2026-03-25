use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use nabla_pdf_rag_contracts::*;
use nabla_pdf_rag_core::*;
use nabla_pdf_rag_embedder::HashEmbedder;
use nabla_pdf_rag_hierarchy::RaptorLiteBuilder;
use nabla_pdf_rag_llm::{ApiLlmClient, ApiProvider, LocalCliLlmClient};
use nabla_pdf_rag_parser::PdfExtractParser;
use nabla_pdf_rag_storage::{run_migrations, SqliteRepository};
use rusqlite::Connection;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(name = "nabla-pdf", about = "PDF RAG MVP")]
struct Args {
    /// Path to SQLite database
    #[arg(long, default_value = "nabla.db")]
    db: String,

    /// LLM provider
    #[arg(long, default_value = "mock")]
    llm: LlmProvider,

    /// API key (or set NABLA_API_KEY / OPENAI_API_KEY / ANTHROPIC_API_KEY env var)
    #[arg(long)]
    api_key: Option<String>,

    /// API base URL (overrides provider default)
    #[arg(long)]
    base_url: Option<String>,

    /// Model name (overrides provider default)
    #[arg(long)]
    model: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, ValueEnum)]
enum LlmProvider {
    /// No real LLM — placeholder summaries
    Mock,
    /// OpenAI-compatible API (also Kimi, MiniMax, DashScope)
    Openai,
    /// Anthropic Claude API
    Anthropic,
    /// Local `claude` CLI
    Claude,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show architecture blueprint
    Blueprint,

    /// Import PDF files into a library
    Import {
        /// Library name (created if not exists)
        #[arg(long, default_value = "default")]
        library: String,
        /// PDF file paths
        paths: Vec<PathBuf>,
    },

    /// Ask a question against the library
    Ask {
        /// Library name
        #[arg(long, default_value = "default")]
        library: String,
        /// Your question
        prompt: String,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    let llm = build_llm(&args)?;

    match args.command {
        Command::Blueprint => {
            print_blueprint();
            Ok(())
        }
        Command::Import { library, paths } => {
            let repo = open_db(&args.db)?;
            cmd_import(&repo, &library, &paths, llm.as_ref())
        }
        Command::Ask { library, prompt } => {
            let repo = open_db(&args.db)?;
            cmd_ask(&repo, &library, &prompt, llm.as_ref())
        }
    }
}

fn build_llm(args: &Args) -> Result<Box<dyn LlmClient>> {
    match args.llm {
        LlmProvider::Mock => Ok(Box::new(MockLlm)),
        LlmProvider::Openai => {
            let key = resolve_api_key(args, &["NABLA_API_KEY", "OPENAI_API_KEY"])?;
            Ok(Box::new(ApiLlmClient::new(
                ApiProvider::OpenAi,
                key,
                args.model.clone(),
                args.base_url.clone(),
                None,
            )))
        }
        LlmProvider::Anthropic => {
            let key = resolve_api_key(args, &["NABLA_API_KEY", "ANTHROPIC_API_KEY"])?;
            Ok(Box::new(ApiLlmClient::new(
                ApiProvider::Anthropic,
                key,
                args.model.clone(),
                args.base_url.clone(),
                None,
            )))
        }
        LlmProvider::Claude => Ok(Box::new(LocalCliLlmClient::new(
            nabla_pdf_rag_llm::local_cli::LocalCliTool::Claude,
            None,
        ))),
    }
}

fn resolve_api_key(args: &Args, env_vars: &[&str]) -> Result<String> {
    if let Some(key) = &args.api_key {
        return Ok(key.clone());
    }
    for var in env_vars {
        if let Ok(key) = std::env::var(var) {
            if !key.is_empty() {
                return Ok(key);
            }
        }
    }
    bail!(
        "No API key provided. Use --api-key or set one of: {}",
        env_vars.join(", ")
    )
}

fn open_db(path: &str) -> Result<SqliteRepository> {
    let conn = Connection::open(path).context("Failed to open database")?;
    run_migrations(&conn)?;
    Ok(SqliteRepository::new(conn))
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

// ─── Import ────────────────────────────────────────────────────────────────

fn cmd_import(
    repo: &SqliteRepository,
    library_name: &str,
    paths: &[PathBuf],
    llm: &dyn LlmClient,
) -> Result<()> {
    if paths.is_empty() {
        bail!("No PDF paths provided");
    }

    let progress = StderrProgress;
    let library_id = LibraryId::new(format!("lib-{library_name}"));
    let lib = LibraryRecord {
        id: library_id.clone(),
        name: library_name.into(),
        root_dir: ".".into(),
        created_at: now(),
    };
    let _ = repo.insert_library(&lib);

    let parser = PdfExtractParser;
    let builder = RaptorLiteBuilder::default();
    let embedder = HashEmbedder::default();

    for path in paths {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown.pdf".into());

        println!("\n--- Importing: {file_name} ---");
        let t0 = std::time::Instant::now();

        let doc_id = DocumentId::new(Uuid::new_v4().to_string());
        let checksum = format!("{:x}", hash_path(path));

        let doc = DocumentRecord {
            id: doc_id.clone(),
            library_id: library_id.clone(),
            batch_id: None,
            file_name: file_name.clone(),
            source_path: path.to_string_lossy().into(),
            checksum_sha256: checksum,
            page_count: None,
            title: None,
            authors: vec![],
            state: DocumentState::Queued,
            created_at: now(),
            updated_at: now(),
            error_message: None,
        };

        if let Err(e) = repo.insert_document(&doc) {
            eprintln!("  Skipped (likely duplicate): {e}");
            continue;
        }

        // Parse
        repo.update_document_state(&doc_id, &DocumentState::Extracting, None)?;
        let extracted = match parser.extract_text(&doc, &progress) {
            Ok(e) => e,
            Err(e) => {
                let msg = format!("Parse failed: {e}");
                eprintln!("  {msg}");
                repo.update_document_state(&doc_id, &DocumentState::Failed, Some(&msg))?;
                continue;
            }
        };
        println!("  Parsed {} pages", extracted.pages.len());

        // Build hierarchy
        repo.update_document_state(&doc_id, &DocumentState::Chunking, None)?;
        let hierarchy = builder.build(&extracted, llm, &progress)?;
        println!(
            "  {} chunks, {} summary nodes",
            hierarchy.chunks.len(),
            hierarchy.summary_nodes.len()
        );

        for chunk in &hierarchy.chunks {
            repo.insert_chunk(chunk)?;
        }
        for node in &hierarchy.summary_nodes {
            repo.insert_summary_node(node)?;
        }

        // Embed
        repo.update_document_state(&doc_id, &DocumentState::Embedding, None)?;
        let embed_result = embedder.embed_chunks(&hierarchy.chunks, &progress)?;
        println!(
            "  Embedded {} chunks ({} failed)",
            embed_result.indexed.len(),
            embed_result.failed.len()
        );

        repo.update_document_state(&doc_id, &DocumentState::Ready, None)?;
        println!("  Done: {file_name} → Ready ({:.1}s)", t0.elapsed().as_secs_f64());
    }

    Ok(())
}

// ─── Ask ───────────────────────────────────────────────────────────────────

fn cmd_ask(
    repo: &SqliteRepository,
    library_name: &str,
    prompt: &str,
    llm: &dyn LlmClient,
) -> Result<()> {
    let library_id = LibraryId::new(format!("lib-{library_name}"));
    let docs = repo.list_documents(&library_id)?;

    if docs.is_empty() {
        println!("No documents in library '{library_name}'. Import some PDFs first.");
        return Ok(());
    }

    // Gather all chunks
    let mut all_chunks = Vec::new();
    for doc in &docs {
        all_chunks.extend(repo.list_chunks(&doc.id)?);
    }

    if all_chunks.is_empty() {
        println!("No chunks found. Documents may not have been parsed yet.");
        return Ok(());
    }

    // Keyword recall
    let query_words: Vec<&str> = prompt.split_whitespace().collect();
    let mut scored: Vec<(f32, &ChunkRecord)> = all_chunks
        .iter()
        .map(|c| {
            let text_lower = c.text.to_lowercase();
            let hits = query_words
                .iter()
                .filter(|w| text_lower.contains(&w.to_lowercase()))
                .count();
            (hits as f32 / query_words.len().max(1) as f32, c)
        })
        .filter(|(score, _)| *score > 0.0)
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let top_chunks: Vec<&ChunkRecord> = scored.iter().take(5).map(|(_, c)| *c).collect();

    if top_chunks.is_empty() {
        println!("No relevant chunks found for: \"{prompt}\"");
        return Ok(());
    }

    // Show evidence
    println!("Question: {prompt}\n");
    println!("Evidence ({} chunks):\n", top_chunks.len());

    for (i, chunk) in top_chunks.iter().enumerate() {
        let page_info = chunk
            .page_span
            .as_ref()
            .map(|p| format!("pp.{}-{}", p.start, p.end))
            .unwrap_or_else(|| "?".into());
        let preview: String = chunk.text.chars().take(150).collect();
        println!("  {}. [{}] {preview}...", i + 1, page_info);
    }

    // Generate answer via LLM
    println!("\n--- Answer ---\n");

    let evidence_text: String = top_chunks
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let pages = c
                .page_span
                .as_ref()
                .map(|p| format!("pages {}-{}", p.start, p.end))
                .unwrap_or_default();
            format!("[{}] ({}) {}", i + 1, pages, c.text)
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let answer_prompt = format!(
        "Based on the following evidence chunks from research documents, answer the user's question. \
         Cite evidence using [N] notation. If the evidence is insufficient, say so.\n\n\
         Evidence:\n{evidence_text}\n\n\
         Question: {prompt}\n\n\
         Answer:"
    );

    match llm.complete(&answer_prompt, 500) {
        Ok(answer) => println!("{answer}"),
        Err(e) => println!("(LLM error: {e})"),
    }

    Ok(())
}

// ─── Utilities ─────────────────────────────────────────────────────────────

struct StderrProgress;

impl ProgressSink for StderrProgress {
    fn on_progress(&self, update: &ProgressUpdate) {
        eprintln!(
            "  [{:?}] {}/{}{}",
            update.stage,
            update.current,
            update.total,
            update
                .message
                .as_ref()
                .map(|m| format!(" — {m}"))
                .unwrap_or_default()
        );
    }
}

struct MockLlm;

impl LlmClient for MockLlm {
    fn complete(&self, _prompt: &str, _max_tokens: u32) -> Result<String> {
        Ok("(Mock LLM — use --llm openai/anthropic/claude for real answers)".into())
    }

    fn complete_json(&self, _prompt: &str, _max_tokens: u32) -> Result<serde_json::Value> {
        Ok(serde_json::json!({}))
    }

    fn max_context_tokens(&self) -> u32 {
        4096
    }
}

fn hash_path(path: &PathBuf) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    if let Ok(meta) = std::fs::metadata(path) {
        meta.len().hash(&mut hasher);
    }
    hasher.finish()
}

fn now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}
