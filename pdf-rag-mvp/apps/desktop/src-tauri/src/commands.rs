use nabla_pdf_rag_contracts::*;
use nabla_pdf_rag_core::DocumentRepository;
use nabla_pdf_rag_retrieval::HybridSearcher;
use serde::Serialize;
use tauri::State;

use crate::state::AppState;

#[derive(Serialize)]
pub struct DocumentInfo {
    pub id: String,
    pub file_name: String,
    pub state: String,
    pub page_count: Option<u32>,
    pub title: Option<String>,
}

#[derive(Serialize)]
pub struct SearchResult {
    pub chunk_id: String,
    pub document_id: String,
    pub text: String,
    pub score: f32,
}

#[derive(Serialize)]
pub struct AskResponse {
    pub evidence: Vec<SearchResult>,
    pub doc_summaries: Vec<String>,
    pub answer: String,
}

/// List all documents in the default library.
#[tauri::command]
pub fn list_documents(state: State<AppState>) -> Result<Vec<DocumentInfo>, String> {
    let library_id = LibraryId::new("lib-default");
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

/// Import PDF files into the default library.
#[tauri::command]
pub async fn import_files(paths: Vec<String>, _state: State<'_, AppState>) -> Result<String, String> {
    // For now, return a placeholder. Full import pipeline will be wired in next iteration.
    Ok(format!("Import queued: {} files", paths.len()))
}

/// Ask a question against the knowledge base.
#[tauri::command]
pub async fn ask_question(
    prompt: String,
    _doc_ids: Option<Vec<String>>,
    state: State<'_, AppState>,
) -> Result<AskResponse, String> {
    let lance = state.lance().await.map_err(|e| e.to_string())?;
    let searcher = HybridSearcher::new(&lance);

    // TODO: embed query with ApiEmbedder, for now use FTS only
    let hits = searcher
        .fts_search(&prompt, 5)
        .await
        .map_err(|e| e.to_string())?;

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

    Ok(AskResponse {
        evidence,
        doc_summaries,
        answer: "(Connect LLM for full answer generation)".into(),
    })
}

/// Get document summaries for given document IDs.
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
