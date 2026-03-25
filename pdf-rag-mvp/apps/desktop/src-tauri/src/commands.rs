use std::path::PathBuf;

use nabla_pdf_rag_contracts::*;
use nabla_pdf_rag_core::*;
use nabla_pdf_rag_hierarchy::RaptorLiteBuilder;
use nabla_pdf_rag_parser::PdfExtractParser;
use nabla_pdf_rag_retrieval::{ChunkEmbedding, HybridSearcher};
use serde::Serialize;
use tauri::{Emitter, State};
use uuid::Uuid;

use crate::state::{AppState, DEFAULT_LIBRARY_ID};

// ─── Response types ────────────────────────────────────────────────────────

#[derive(Serialize, Clone)]
pub struct DocumentInfo {
    pub id: String,
    pub file_name: String,
    pub state: String,
    pub page_count: Option<u32>,
    pub title: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct SearchResult {
    pub chunk_id: String,
    pub document_id: String,
    pub text: String,
    pub score: f32,
}

#[derive(Serialize, Clone)]
pub struct AskResponse {
    pub evidence: Vec<SearchResult>,
    pub doc_summaries: Vec<String>,
    pub answer: String,
}

#[derive(Serialize, Clone)]
pub struct ImportProgress {
    pub file_name: String,
    pub stage: String,
    pub current: u64,
    pub total: u64,
    pub message: String,
}

// ─── Commands ──────────────────────────────────────────────────────────────

#[tauri::command]
pub fn list_documents(state: State<AppState>) -> Result<Vec<DocumentInfo>, String> {
    let library_id = LibraryId::new(DEFAULT_LIBRARY_ID);
    let docs = state
        .repo
        .list_documents(&library_id)
        .map_err(|e| e.to_string())?;

    Ok(docs
        .into_iter()
        .map(|d| DocumentInfo {
            id: d.id.to_string(),
            file_name: d.file_name,
            state: d.state.to_string(),
            page_count: d.page_count,
            title: d.title,
        })
        .collect())
}

/// Import PDF files — runs the full pipeline: parse → chunk → summarize → embed → LanceDB.
/// Emits "import-progress" events to the frontend.
#[tauri::command]
pub async fn import_files(
    paths: Vec<String>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let lance = state.lance().await.map_err(|e| e.to_string())?;
    let library_id = LibraryId::new(DEFAULT_LIBRARY_ID);

    let parser = PdfExtractParser;
    let builder = RaptorLiteBuilder::default();
    let embedder = state.build_embedder();
    let llm = state.build_llm();

    let mut imported = 0;
    let mut failed = 0;

    for path_str in &paths {
        let path = PathBuf::from(path_str);
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown.pdf".into());

        let emit_progress = |stage: &str, msg: &str| {
            let _ = app.emit(
                "import-progress",
                ImportProgress {
                    file_name: file_name.clone(),
                    stage: stage.into(),
                    current: 0,
                    total: 1,
                    message: msg.into(),
                },
            );
        };

        emit_progress("parse", &format!("Reading {file_name}"));

        let doc_id = DocumentId::new(Uuid::new_v4().to_string());
        let checksum = format!("{:x}", hash_path(&path));

        let doc = DocumentRecord {
            id: doc_id.clone(),
            library_id: library_id.clone(),
            batch_id: None,
            file_name: file_name.clone(),
            source_path: path_str.clone(),
            checksum_sha256: checksum,
            page_count: None,
            title: None,
            authors: vec![],
            state: DocumentState::Queued,
            created_at: String::new(),
            updated_at: String::new(),
            error_message: None,
        };

        if state.repo.insert_document(&doc).is_err() {
            emit_progress("skip", "Duplicate, skipping");
            continue;
        }

        // Parse
        let _ = state.repo.update_document_state(&doc_id, &DocumentState::Extracting, None);
        let extracted = match parser.extract_text(&doc, &NullProgress) {
            Ok(e) => e,
            Err(e) => {
                let msg = format!("Parse failed: {e}");
                emit_progress("error", &msg);
                let _ = state.repo.update_document_state(&doc_id, &DocumentState::Failed, Some(&msg));
                failed += 1;
                continue;
            }
        };
        emit_progress("parse", &format!("Parsed {} pages", extracted.pages.len()));

        // Build hierarchy
        let _ = state.repo.update_document_state(&doc_id, &DocumentState::Chunking, None);
        emit_progress("chunk", "Building hierarchy");
        let hierarchy = match builder.build(&extracted, llm.as_ref(), &NullProgress) {
            Ok(h) => h,
            Err(e) => {
                let msg = format!("Hierarchy failed: {e}");
                emit_progress("error", &msg);
                let _ = state.repo.update_document_state(&doc_id, &DocumentState::Failed, Some(&msg));
                failed += 1;
                continue;
            }
        };

        // Persist to SQLite
        for chunk in &hierarchy.chunks {
            let _ = state.repo.insert_chunk(chunk);
        }
        for node in &hierarchy.summary_nodes {
            let _ = state.repo.insert_summary_node(node);
        }

        // Embed + persist to LanceDB
        let _ = state.repo.update_document_state(&doc_id, &DocumentState::Embedding, None);
        emit_progress("embed", "Embedding chunks");
        let embed_result = match embedder.embed_chunks(&hierarchy.chunks, &NullProgress) {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("Embed failed: {e}");
                emit_progress("error", &msg);
                let _ = state.repo.update_document_state(&doc_id, &DocumentState::Failed, Some(&msg));
                failed += 1;
                continue;
            }
        };

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

        if let Err(e) = lance.upsert_embeddings(&items).await {
            emit_progress("error", &format!("LanceDB write failed: {e}"));
            let _ = state.repo.update_document_state(&doc_id, &DocumentState::Failed, Some(&e.to_string()));
            failed += 1;
            continue;
        }

        let _ = state.repo.update_document_state(&doc_id, &DocumentState::Ready, None);
        emit_progress("done", &format!("{file_name} ready"));
        imported += 1;
    }

    // Rebuild FTS index
    let _ = lance.rebuild_fts_index().await;

    Ok(format!("{imported} imported, {failed} failed"))
}

/// Ask a question — hybrid search + document summaries + LLM answer.
#[tauri::command]
pub async fn ask_question(
    prompt: String,
    doc_ids: Option<Vec<String>>,
    state: State<'_, AppState>,
) -> Result<AskResponse, String> {
    let lance = state.lance().await.map_err(|e| e.to_string())?;

    // Build searcher with optional doc filter
    let mut searcher = HybridSearcher::new(&lance);
    if let Some(ref ids) = doc_ids {
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        searcher = searcher.with_doc_ids(&id_refs);
    }

    // Embed query for vector search
    let embedder = state.build_embedder();
    let query_vec = embed_query_text(embedder.as_ref(), &prompt);

    // Hybrid search with FTS fallback
    let hits = match searcher
        .hybrid_search(&prompt, &query_vec, 5)
        .await
    {
        Ok(h) => h,
        Err(_) => searcher
            .fts_search(&prompt, 5)
            .await
            .map_err(|e| e.to_string())?,
    };

    let evidence: Vec<SearchResult> = hits
        .iter()
        .map(|h| SearchResult {
            chunk_id: h.chunk_id.clone(),
            document_id: h.document_id.clone(),
            text: h.text.clone(),
            score: h.score,
        })
        .collect();

    // Collect document summaries
    let mut seen = std::collections::HashSet::new();
    let mut doc_summaries = Vec::new();
    for hit in &hits {
        if seen.insert(hit.document_id.clone()) {
            let doc_id = DocumentId::new(&hit.document_id);
            if let Ok(nodes) = state.repo.list_summary_nodes(&doc_id) {
                if let Some(doc_node) = nodes.iter().find(|n| n.kind == SummaryNodeKind::Document) {
                    doc_summaries.push(doc_node.summary.clone());
                }
            }
        }
    }

    // Generate answer via mock LLM (real LLM integration via config in next iteration)
    let evidence_text: String = hits
        .iter()
        .enumerate()
        .map(|(i, h)| format!("[{}] {}", i + 1, h.text))
        .collect::<Vec<_>>()
        .join("\n\n");

    let summary_context = if doc_summaries.is_empty() {
        String::new()
    } else {
        format!(
            "Document summaries:\n{}\n\n",
            doc_summaries.join("\n")
        )
    };

    let answer_prompt = format!(
        "Based on the following document summaries and evidence chunks, answer the user's question. \
         Cite evidence using [N] notation.\n\n\
         {summary_context}\
         Evidence:\n{evidence_text}\n\n\
         Question: {prompt}\n\nAnswer:"
    );

    let llm = state.build_llm();
    let answer = llm
        .complete(&answer_prompt, 500)
        .unwrap_or_else(|e| format!("LLM error: {e}"));

    Ok(AskResponse {
        evidence,
        doc_summaries,
        answer,
    })
}

#[tauri::command]
pub fn get_document_summaries(
    doc_ids: Vec<String>,
    state: State<AppState>,
) -> Result<Vec<String>, String> {
    let mut summaries = Vec::new();
    for id in doc_ids {
        let doc_id = DocumentId::new(id);
        if let Ok(nodes) = state.repo.list_summary_nodes(&doc_id) {
            if let Some(doc_node) = nodes.iter().find(|n| n.kind == SummaryNodeKind::Document) {
                summaries.push(doc_node.summary.clone());
            }
        }
    }
    Ok(summaries)
}

/// Delete a document and all its chunks/summaries from SQLite.
/// LanceDB data is left as-is (orphaned rows are harmless and cleaned on next rebuild).
#[tauri::command]
pub fn delete_document(doc_id: String, state: State<AppState>) -> Result<(), String> {
    let id = DocumentId::new(doc_id);
    state.repo.delete_document(&id).map_err(|e| e.to_string())
}

/// Get current app configuration.
#[tauri::command]
pub fn get_config(state: State<AppState>) -> Result<crate::config::AppConfig, String> {
    let config = state.config.read().map_err(|e| e.to_string())?;
    Ok(config.clone())
}

/// Save app configuration.
#[tauri::command]
pub fn save_config(
    config: crate::config::AppConfig,
    state: State<AppState>,
) -> Result<(), String> {
    crate::config::save_config(&config).map_err(|e| e.to_string())?;
    let mut current = state.config.write().map_err(|e| e.to_string())?;
    *current = config;
    // Reset LanceDB connection so it picks up new embedding dimensions
    let lance = state.lance.try_lock();
    if let Ok(mut guard) = lance {
        *guard = None;
    }
    Ok(())
}

// ─── Helpers ───────────────────────────────────────────────────────────────

fn embed_query_text(embedder: &dyn Embedder, text: &str) -> Vec<f32> {
    let chunk = ChunkRecord {
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
        .embed_chunks(&[chunk], &NullProgress)
        .expect("query embedding failed");
    result.indexed[0].vector.clone()
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
