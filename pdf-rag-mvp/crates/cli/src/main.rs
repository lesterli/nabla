use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use nabla_pdf_rag_contracts::*;
use nabla_pdf_rag_core::*;
use nabla_pdf_rag_embedder::{ApiEmbedder, HashEmbedder};
use nabla_pdf_rag_hierarchy::RaptorLiteBuilder;
use nabla_pdf_rag_llm::{ApiLlmClient, ApiProvider, LocalCliLlmClient};
use nabla_pdf_rag_parser::PdfExtractParser;
use nabla_pdf_rag_retrieval::{ChunkEmbedding, HybridSearcher, LanceStore};
use nabla_pdf_rag_storage::{run_migrations, SqliteRepository};
use rusqlite::Connection;
use uuid::Uuid;

const LANCE_DIR: &str = "nabla.lance";

#[derive(Debug, Parser)]
#[command(name = "nabla-pdf", about = "PDF RAG MVP")]
struct Args {
    /// Path to SQLite database
    #[arg(long, default_value = "nabla.db")]
    db: String,

    /// Path to LanceDB directory
    #[arg(long, default_value = LANCE_DIR)]
    lance: String,

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

    /// Embedding model (default: text-embedding-3-small for API, hash for mock)
    #[arg(long)]
    embed_model: Option<String>,

    /// Embedding API base URL (defaults to same as --base-url or OpenAI)
    #[arg(long)]
    embed_base_url: Option<String>,

    /// Embedding API key (defaults to same as --api-key)
    #[arg(long)]
    embed_api_key: Option<String>,

    /// Embedding dimensions (default: 1536 for API, 384 for hash)
    #[arg(long)]
    embed_dim: Option<usize>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, ValueEnum)]
enum LlmProvider {
    Mock,
    Openai,
    Anthropic,
    Claude,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show architecture blueprint
    Blueprint,

    /// Import PDF files into a library
    Import {
        #[arg(long, default_value = "default")]
        library: String,
        paths: Vec<PathBuf>,
    },

    /// Ask a question against the library
    Ask {
        #[arg(long, default_value = "default")]
        library: String,

        /// Filter by document name (substring match)
        #[arg(long)]
        doc: Option<String>,

        /// Path to a prompt template file (system instruction for the LLM)
        #[arg(long)]
        prompt_file: Option<PathBuf>,

        /// Your question
        prompt: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let llm = build_llm(&args)?;
    let (embedder, embed_dim) = build_embedder(&args)?;

    match args.command {
        Command::Blueprint => {
            print_blueprint();
            Ok(())
        }
        Command::Import { library, paths } => {
            let repo = open_db(&args.db)?;
            let lance = LanceStore::open(&args.lance, embed_dim as i32).await?;
            cmd_import(&repo, &lance, &library, &paths, llm.as_ref(), embedder.as_ref()).await
        }
        Command::Ask {
            library,
            doc,
            prompt_file,
            prompt,
        } => {
            let repo = open_db(&args.db)?;
            let lance = LanceStore::open(&args.lance, embed_dim as i32).await?;
            cmd_ask(&repo, &lance, &library, &prompt, doc.as_deref(), prompt_file.as_deref(), llm.as_ref(), embedder.as_ref()).await
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

/// Build the embedder. Returns (embedder, dimensions).
/// - Mock mode: HashEmbedder (384-dim, no API needed)
/// - API mode: ApiEmbedder using OpenAI-compatible /embeddings
fn build_embedder(args: &Args) -> Result<(Box<dyn Embedder>, usize)> {
    match args.llm {
        LlmProvider::Mock => {
            let dim = args.embed_dim.unwrap_or(384);
            Ok((
                Box::new(HashEmbedder { dimensions: dim }),
                dim,
            ))
        }
        _ => {
            // For API/Claude modes, use ApiEmbedder
            // Reuse LLM's API key if no separate embed key is set
            let key = args
                .embed_api_key
                .clone()
                .or_else(|| args.api_key.clone())
                .or_else(|| std::env::var("OPENAI_API_KEY").ok())
                .or_else(|| std::env::var("NABLA_API_KEY").ok())
                .ok_or_else(|| anyhow::anyhow!(
                    "No embedding API key. Use --embed-api-key, --api-key, or set OPENAI_API_KEY"
                ))?;

            let embed = ApiEmbedder::new(
                key,
                args.embed_base_url.clone().or_else(|| args.base_url.clone()),
                args.embed_model.clone(),
                args.embed_dim,
            );
            let dim = embed.dimensions();
            Ok((Box::new(embed), dim))
        }
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

async fn cmd_import(
    repo: &SqliteRepository,
    lance: &LanceStore,
    library_name: &str,
    paths: &[PathBuf],
    llm: &dyn LlmClient,
    embedder: &dyn Embedder,
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
        prompt_template: None,
    };
    let _ = repo.insert_library(&lib);

    let parser = PdfExtractParser;
    let builder = RaptorLiteBuilder::default();

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

        // Parse (catch_unwind because pdf-extract can panic on malformed fonts)
        repo.update_document_state(&doc_id, &DocumentState::Extracting, None)?;
        let parse_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            parser.extract_text(&doc, &progress)
        }));
        let extracted = match parse_result {
            Ok(Ok(e)) => e,
            Ok(Err(e)) => {
                let msg = format!("Parse failed: {e}");
                eprintln!("  {msg}");
                repo.update_document_state(&doc_id, &DocumentState::Failed, Some(&msg))?;
                continue;
            }
            Err(_) => {
                let msg = "Parse crashed: PDF has unsupported font encoding";
                eprintln!("  {msg}");
                repo.update_document_state(&doc_id, &DocumentState::Failed, Some(msg))?;
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

        // Embed + persist to LanceDB
        repo.update_document_state(&doc_id, &DocumentState::Embedding, None)?;
        let embed_result = embedder.embed_chunks(&hierarchy.chunks, &progress)?;

        let items: Vec<ChunkEmbedding> = embed_result
            .indexed
            .iter()
            .zip(hierarchy.chunks.iter())
            .map(|(r, c)| ChunkEmbedding {
                record: r,
                document_id: c.document_id.as_str(),
                text: &c.text,
            })
            .collect();

        lance.upsert_embeddings(&items).await?;

        println!(
            "  Embedded {} chunks → LanceDB ({} failed)",
            embed_result.indexed.len(),
            embed_result.failed.len()
        );
        for (chunk_id, reason) in &embed_result.failed {
            eprintln!("    FAIL {chunk_id}: {reason}");
        }

        repo.update_document_state(&doc_id, &DocumentState::Ready, None)?;
        println!(
            "  Done: {file_name} → Ready ({:.1}s)",
            t0.elapsed().as_secs_f64()
        );
    }

    // Rebuild FTS index after all imports
    lance.rebuild_fts_index().await?;
    println!("\nFTS index rebuilt.");

    Ok(())
}

// ─── Ask ───────────────────────────────────────────────────────────────────

async fn cmd_ask(
    repo: &SqliteRepository,
    lance: &LanceStore,
    library_name: &str,
    prompt: &str,
    doc_filter: Option<&str>,
    prompt_file: Option<&std::path::Path>,
    llm: &dyn LlmClient,
    embedder: &dyn Embedder,
) -> Result<()> {
    let library_id = LibraryId::new(format!("lib-{library_name}"));
    let docs = repo.list_documents(&library_id)?;

    if docs.is_empty() {
        println!("No documents in library '{library_name}'. Import some PDFs first.");
        return Ok(());
    }

    // Filter documents by name if --doc is provided
    let filtered_docs: Vec<&DocumentRecord> = if let Some(filter) = doc_filter {
        let filter_lower = filter.to_lowercase();
        let matched: Vec<_> = docs
            .iter()
            .filter(|d| d.file_name.to_lowercase().contains(&filter_lower))
            .collect();
        if matched.is_empty() {
            println!("No documents matching '{filter}' in library '{library_name}'.");
            return Ok(());
        }
        println!("Filtered to {} document(s) matching '{filter}':\n", matched.len());
        for d in &matched {
            println!("  - {}", d.file_name);
        }
        println!();
        matched
    } else {
        docs.iter().collect()
    };

    // Embed the query for vector search
    let query_embedding = embed_query(embedder, prompt);

    // Two-stage doc filter: SQLite matches by file name → LanceDB filters by document ID
    let mut searcher = HybridSearcher::new(lance);
    if doc_filter.is_some() {
        let doc_ids: Vec<&str> = filtered_docs.iter().map(|d| d.id.as_str()).collect();
        searcher = searcher.with_doc_ids(&doc_ids);
    }

    let hits = match searcher.hybrid_search(prompt, &query_embedding, 5).await {
        Ok(h) => h,
        Err(_) => searcher.fts_search(prompt, 5).await?,
    };

    if hits.is_empty() {
        println!("No relevant chunks found for: \"{prompt}\"");
        return Ok(());
    }

    // Collect document summaries for each unique doc in the hits
    let mut seen_docs = std::collections::HashSet::new();
    let mut doc_summaries = Vec::new();
    for hit in &hits {
        if seen_docs.insert(hit.document_id.clone()) {
            let doc_id = DocumentId::new(&hit.document_id);
            if let Ok(nodes) = repo.list_summary_nodes(&doc_id) {
                if let Some(doc_node) = nodes.iter().find(|n| n.kind == SummaryNodeKind::Document) {
                    doc_summaries.push(format!(
                        "[Doc: {}] {}",
                        &hit.document_id[..8.min(hit.document_id.len())],
                        doc_node.summary
                    ));
                }
            }
        }
    }

    // Show evidence
    println!("Question: {prompt}\n");
    if !doc_summaries.is_empty() {
        println!("Document summaries ({}):\n", doc_summaries.len());
        for s in &doc_summaries {
            println!("  {s}\n");
        }
    }
    println!("Evidence ({} chunks via hybrid search):\n", hits.len());

    for (i, hit) in hits.iter().enumerate() {
        let preview: String = hit.text.chars().take(150).collect();
        println!(
            "  {}. [score={:.2}] [doc:{}] {preview}...",
            i + 1,
            hit.score,
            &hit.document_id[..8.min(hit.document_id.len())]
        );
    }

    // Generate answer via LLM — include document summaries for global context
    println!("\n--- Answer ---\n");

    let summary_context = if doc_summaries.is_empty() {
        String::new()
    } else {
        format!(
            "Document summaries (global context):\n{}\n\n",
            doc_summaries.join("\n")
        )
    };

    let evidence_text: String = hits
        .iter()
        .enumerate()
        .map(|(i, h)| format!("[{}] {}", i + 1, h.text))
        .collect::<Vec<_>>()
        .join("\n\n");

    // Load custom system prompt from file, or use default
    let system_instruction = if let Some(path) = prompt_file {
        std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read prompt file: {}", path.display()))?
    } else {
        "Based on the following document summaries and evidence chunks, answer the user's question. \
         Cite evidence using [N] notation. Use document summaries for context (names, overview). \
         If the evidence is insufficient, say so."
            .to_string()
    };

    let answer_prompt = format!(
        "{system_instruction}\n\n\
         {summary_context}\
         Evidence chunks:\n{evidence_text}\n\n\
         Question: {prompt}\n\n\
         Answer:"
    );

    match llm.complete(&answer_prompt, 500) {
        Ok(answer) => println!("{answer}"),
        Err(e) => println!("(LLM error: {e})"),
    }

    Ok(())
}

/// Embed a query string using the same embedder as import.
fn embed_query(embedder: &dyn Embedder, text: &str) -> Vec<f32> {
    let fake_chunk = ChunkRecord {
        id: ChunkId::new("query"),
        document_id: DocumentId::new("query"),
        summary_node_id: None,
        ordinal: 0,
        heading_path: vec![],
        page_span: None,
        text: text.into(),
        token_count: 0,
        embedding_state: EmbeddingState::Pending,
    };
    let result = embedder
        .embed_chunks(&[fake_chunk], &NullProgress)
        .expect("query embedding failed");
    result.indexed[0].vector.clone()
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
