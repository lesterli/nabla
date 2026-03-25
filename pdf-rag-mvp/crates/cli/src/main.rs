use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use nabla_pdf_rag_contracts::*;
use nabla_pdf_rag_core::*;
use nabla_pdf_rag_embedder::HashEmbedder;
use nabla_pdf_rag_hierarchy::RaptorLiteBuilder;
use nabla_pdf_rag_parser::DoclingParser;
use nabla_pdf_rag_storage::{run_migrations, SqliteRepository};
use rusqlite::Connection;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(name = "nabla-pdf", about = "PDF RAG MVP")]
struct Args {
    /// Path to SQLite database (default: ./nabla.db)
    #[arg(long, default_value = "nabla.db")]
    db: String,

    /// Path to sidecar Python script (overrides auto-detection)
    #[arg(long)]
    sidecar: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
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

    match args.command {
        Command::Blueprint => {
            print_blueprint();
            Ok(())
        }
        Command::Import { library, paths } => {
            let repo = open_db(&args.db)?;
            cmd_import(&repo, &library, &paths, args.sidecar.as_deref())
        }
        Command::Ask { library, prompt } => {
            let repo = open_db(&args.db)?;
            cmd_ask(&repo, &library, &prompt)
        }
    }
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
    sidecar_override: Option<&std::path::Path>,
) -> Result<()> {
    if paths.is_empty() {
        bail!("No PDF paths provided");
    }

    let progress = StderrProgress;

    // Ensure library exists
    let library_id = LibraryId::new(format!("lib-{library_name}"));
    let lib = LibraryRecord {
        id: library_id.clone(),
        name: library_name.into(),
        root_dir: ".".into(),
        created_at: now(),
    };
    // Ignore error if library already exists
    let _ = repo.insert_library(&lib);

    let sidecar_path = match sidecar_override {
        Some(p) => p.to_path_buf(),
        None => find_sidecar()?,
    };
    let parser = DoclingParser::new(sidecar_path);
    let builder = RaptorLiteBuilder::default();
    let embedder = HashEmbedder::default();
    let llm = CliMockLlm;

    for path in paths {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown.pdf".into());

        println!("\n--- Importing: {file_name} ---");

        let doc_id = DocumentId::new(Uuid::new_v4().to_string());
        let checksum = format!("{:x}", md5_path(path));

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
        let hierarchy = builder.build(&extracted, &llm, &progress)?;
        println!(
            "  {} chunks, {} summary nodes",
            hierarchy.chunks.len(),
            hierarchy.summary_nodes.len()
        );

        // Persist chunks and summary nodes
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

        // TODO: Persist embeddings to LanceDB (P0-6b)

        repo.update_document_state(&doc_id, &DocumentState::Ready, None)?;
        println!("  Done: {file_name} → Ready");
    }

    Ok(())
}

// ─── Ask ───────────────────────────────────────────────────────────────────

fn cmd_ask(repo: &SqliteRepository, library_name: &str, prompt: &str) -> Result<()> {
    let library_id = LibraryId::new(format!("lib-{library_name}"));
    let docs = repo.list_documents(&library_id)?;

    if docs.is_empty() {
        println!("No documents in library '{library_name}'. Import some PDFs first.");
        return Ok(());
    }

    // Gather all chunks across all documents
    let mut all_chunks = Vec::new();
    for doc in &docs {
        let chunks = repo.list_chunks(&doc.id)?;
        all_chunks.extend(chunks);
    }

    if all_chunks.is_empty() {
        println!("No chunks found. Documents may not have been parsed yet.");
        return Ok(());
    }

    // Simple keyword matching for MVP (vector search requires LanceDB integration)
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
    let top_k = scored.iter().take(5).collect::<Vec<_>>();

    if top_k.is_empty() {
        println!("No relevant chunks found for: \"{prompt}\"");
        return Ok(());
    }

    println!("Question: {prompt}\n");
    println!("Top {} evidence chunks:\n", top_k.len());

    for (i, (score, chunk)) in top_k.iter().enumerate() {
        let page_info = chunk
            .page_span
            .as_ref()
            .map(|p| format!("pp.{}-{}", p.start, p.end))
            .unwrap_or_else(|| "?".into());

        let preview: String = chunk.text.chars().take(200).collect();
        println!(
            "  {}. [score={:.2}] [{}] [doc:{}]",
            i + 1,
            score,
            page_info,
            chunk.document_id
        );
        println!("     {preview}...\n");
    }

    // TODO: Pass to AnswerEngine with LLM for full answer generation
    println!("(Full LLM-powered answer generation will be available with AnswerEngine integration)");

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

/// Mock LLM for CLI testing — returns placeholder summaries.
struct CliMockLlm;

impl LlmClient for CliMockLlm {
    fn complete(&self, _prompt: &str, _max_tokens: u32) -> Result<String> {
        Ok("(Summary placeholder — connect a real LLM for production use)".into())
    }

    fn complete_json(&self, _prompt: &str, _max_tokens: u32) -> Result<serde_json::Value> {
        Ok(serde_json::json!({}))
    }

    fn max_context_tokens(&self) -> u32 {
        4096
    }
}

/// Simple hash of file path for deduplication (not cryptographic).
fn md5_path(path: &PathBuf) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    // Also hash file size if available
    if let Ok(meta) = std::fs::metadata(path) {
        meta.len().hash(&mut hasher);
    }
    hasher.finish()
}

fn find_sidecar() -> Result<PathBuf> {
    // Look relative to the binary, then in common locations
    let candidates = [
        PathBuf::from("scripts/docling_sidecar.py"),
        PathBuf::from("pdf-rag-mvp/scripts/docling_sidecar.py"),
    ];
    for p in &candidates {
        if p.exists() {
            return Ok(p.clone());
        }
    }
    bail!(
        "Could not find docling_sidecar.py. Looked in: {:?}",
        candidates
    );
}

fn now() -> String {
    // Simple UTC timestamp — no chrono dependency needed for MVP
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}
