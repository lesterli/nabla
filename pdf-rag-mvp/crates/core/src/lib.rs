use anyhow::Result;
use nabla_pdf_rag_contracts::{
    AnswerDraft, ChunkId, ChunkRecord, DocumentId, DocumentRecord, LibraryId, RetrievalHit,
    RetrievalQuery, SummaryNode,
};
use serde::{Deserialize, Serialize};

mod structured;
pub use structured::*;

// ─── LLM Abstraction ──────────────────────────────────────────────────────

/// Minimal LLM abstraction — vendor-agnostic, testable, shared across pipeline stages.
pub trait LlmClient: Send + Sync {
    /// Plain text completion.
    fn complete(&self, prompt: &str, max_tokens: u32) -> Result<String>;

    /// Structured JSON completion — returns a JSON value that the caller deserializes.
    fn complete_json(&self, prompt: &str, max_tokens: u32) -> Result<serde_json::Value>;

    /// Maximum context window size in tokens.
    /// Used by AnswerEngine to decide how many evidence chunks to include.
    fn max_context_tokens(&self) -> u32;
}

// ─── Progress Reporting ────────────────────────────────────────────────────

/// Which pipeline stage is reporting progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PipelineStage {
    Discover,
    Parse,
    Structure,
    Summarize,
    Embed,
    Retrieve,
    Answer,
}

/// Progress update emitted by long-running pipeline stages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressUpdate {
    pub stage: PipelineStage,
    pub current: u64,
    pub total: u64,
    pub message: Option<String>,
}

/// A callback that receives progress updates from pipeline stages.
/// Implementations can update a UI progress bar, log to file, etc.
pub trait ProgressSink: Send + Sync {
    fn on_progress(&self, update: &ProgressUpdate);
}

/// No-op progress sink for tests or when progress reporting is not needed.
pub struct NullProgress;

impl ProgressSink for NullProgress {
    fn on_progress(&self, _update: &ProgressUpdate) {}
}

// ─── Repository ────────────────────────────────────────────────────────────

pub trait DocumentRepository: Send + Sync {
    fn list_documents(&self, library_id: &LibraryId) -> Result<Vec<DocumentRecord>>;
    fn list_chunks(&self, document_id: &DocumentId) -> Result<Vec<ChunkRecord>>;
    fn list_summary_nodes(&self, document_id: &DocumentId) -> Result<Vec<SummaryNode>>;
}

// ─── Parser ────────────────────────────────────────────────────────────────

/// Legacy page-level extraction — used internally by PdfExtractParser before
/// conversion to StructuredDocument. Kept for backward compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedPage {
    pub page_number: u32,
    pub text: String,
}

/// Legacy flat document — superseded by StructuredDocument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedDocument {
    pub document_id: DocumentId,
    pub inferred_title: Option<String>,
    pub pages: Vec<ExtractedPage>,
}

/// PDF parsing — returns a structure-aware document representation.
///
/// Structure-aware parsers (Docling) populate full element types with headings,
/// tables, and figures. Fallback parsers (pdf-extract) emit all-Paragraph elements.
pub trait DocumentParser: Send + Sync {
    fn parse(
        &self,
        document: &DocumentRecord,
        progress: &dyn ProgressSink,
    ) -> Result<StructuredDocument>;
}

// ─── Hierarchy Builder ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HierarchyBuildOutput {
    pub summary_nodes: Vec<SummaryNode>,
    pub chunks: Vec<ChunkRecord>,
}

/// Splits a structured document into chunks and builds the hierarchical summary tree.
/// LLM is used internally for generating section/cluster/document summaries.
pub trait HierarchyBuilder: Send + Sync {
    fn build(
        &self,
        document: &StructuredDocument,
        llm: &dyn LlmClient,
        progress: &dyn ProgressSink,
    ) -> Result<HierarchyBuildOutput>;
}

// ─── Embedder ──────────────────────────────────────────────────────────────

/// Result of an embedding operation — makes storage and indexing explicit.
#[derive(Debug, Clone)]
pub struct EmbeddingRecord {
    pub chunk_id: ChunkId,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct EmbeddingBatchResult {
    pub indexed: Vec<EmbeddingRecord>,
    pub failed: Vec<(ChunkId, String)>,
}

/// Embeds chunks into vectors and returns the results.
/// The caller (pipeline orchestrator) is responsible for persisting them to the index.
pub trait Embedder: Send + Sync {
    fn embed_chunks(
        &self,
        chunks: &[ChunkRecord],
        progress: &dyn ProgressSink,
    ) -> Result<EmbeddingBatchResult>;
}

// ─── Retrieval ─────────────────────────────────────────────────────────────

/// A single recall candidate before fusion/reranking.
/// The recall channel is tracked in `hit.sources`.
#[derive(Debug, Clone)]
pub struct RecallCandidate {
    pub hit: RetrievalHit,
}

/// Dense vector (ANN) recall — backed by LanceDB or similar.
pub trait VectorIndex: Send + Sync {
    fn query_nearest(
        &self,
        query_embedding: &[f32],
        top_k: usize,
        library_id: &LibraryId,
    ) -> Result<Vec<RecallCandidate>>;
}

/// Lexical / BM25 recall — backed by LanceDB FTS or Tantivy.
pub trait LexicalIndex: Send + Sync {
    fn query_bm25(
        &self,
        query_text: &str,
        top_k: usize,
        library_id: &LibraryId,
    ) -> Result<Vec<RecallCandidate>>;
}

/// Merges candidates from multiple recall channels into a single ranked list.
/// Default implementation: Reciprocal Rank Fusion (RRF).
pub trait FusionStrategy: Send + Sync {
    fn fuse(&self, channels: Vec<Vec<RecallCandidate>>) -> Vec<RetrievalHit>;
}

/// Optional reranker — cross-encoder or LLM-based.
/// Applied after fusion to boost precision on the top-N results.
pub trait Reranker: Send + Sync {
    fn rerank(
        &self,
        query: &str,
        hits: Vec<RetrievalHit>,
        top_n: usize,
    ) -> Result<Vec<RetrievalHit>>;
}

/// Orchestrates the full retrieval pipeline:
/// embed query → parallel recall (vector + lexical + summary tree) → fuse → rerank → expand.
pub trait Retriever: Send + Sync {
    fn retrieve(&self, query: &RetrievalQuery) -> Result<Vec<RetrievalHit>>;
}

// ─── Answer Engine ─────────────────────────────────────────────────────────

/// Generates answers with citations from retrieved evidence.
pub trait AnswerEngine: Send + Sync {
    fn answer(
        &self,
        query: &RetrievalQuery,
        evidence: &[RetrievalHit],
        llm: &dyn LlmClient,
    ) -> Result<AnswerDraft>;
}

// ─── Pipeline Metadata ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineStep {
    pub name: &'static str,
    pub goal: &'static str,
    pub output: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MvpDecision {
    pub area: &'static str,
    pub mvp_choice: &'static str,
    pub later_choice: &'static str,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoModule {
    pub path: &'static str,
    pub responsibility: &'static str,
}

pub const DEFAULT_PIPELINE: &[PipelineStep] = &[
    PipelineStep {
        name: "discover",
        goal: "scan files and create import jobs",
        output: "DocumentRecord in Queued state",
    },
    PipelineStep {
        name: "parse",
        goal: "extract structured elements (headings, paragraphs, tables) from PDF",
        output: "StructuredDocument",
    },
    PipelineStep {
        name: "structure",
        goal: "split sections and chunks while preserving page spans",
        output: "SummaryNode + ChunkRecord",
    },
    PipelineStep {
        name: "summarize",
        goal: "build section, cluster, and document summaries",
        output: "hierarchical summary tree",
    },
    PipelineStep {
        name: "embed",
        goal: "prepare semantic retrieval on chunks and summary nodes",
        output: "searchable local index",
    },
    PipelineStep {
        name: "answer",
        goal: "retrieve evidence and respond with citations",
        output: "AnswerDraft",
    },
];

pub const DEFAULT_DECISIONS: &[MvpDecision] = &[
    MvpDecision {
        area: "retrieval backbone",
        mvp_choice: "RAPTOR-lite summary tree over chunks",
        later_choice: "GraphRAG sidecar for explicit multi-hop routing",
        reason: "PDF MVP wins first from stable hierarchy and page-grounded citations.",
    },
    MvpDecision {
        area: "metadata storage",
        mvp_choice: "SQLite for system of record (libraries, tasks, documents, chunks)",
        later_choice: "Postgres if multi-device sync is needed",
        reason: "Desktop MVP should stay operationally simple and offline-first.",
    },
    MvpDecision {
        area: "retrieval index",
        mvp_choice: "LanceDB (embedded, vector + FTS/BM25 + hybrid/RRF in one system)",
        later_choice: "Add Tantivy for deeper lexical/CJK control, or Qdrant for server mode",
        reason: "Single embedded index minimizes operational cost; upgrade path is clear.",
    },
    MvpDecision {
        area: "PDF parsing",
        mvp_choice: "Docling via sidecar process, OCRmyPDF as OCR fallback",
        later_choice: "Native Rust parser if sidecar bundling proves too costly",
        reason: "Docling has best layout understanding; sidecar keeps Rust pipeline clean.",
    },
    MvpDecision {
        area: "embedding model",
        mvp_choice: "Local ONNX (bge-small-zh-v1.5 or bge-m3) via ort crate",
        later_choice: "Remote API (OpenAI text-embedding-3-small) as opt-in for quality",
        reason: "Local-first means offline-capable; CJK-aware model matches research use case.",
    },
    MvpDecision {
        area: "desktop shell",
        mvp_choice: "Tauri with four views: library, tasks, documents, ask",
        later_choice: "annotation, collaboration, graph exploration",
        reason: "The first release should optimize import reliability and evidence tracing.",
    },
];

pub const REPO_LAYOUT: &[RepoModule] = &[
    RepoModule {
        path: "crates/contracts",
        responsibility: "shared domain types for documents, chunks, summaries, and retrieval",
    },
    RepoModule {
        path: "crates/core",
        responsibility: "pipeline boundaries, traits, and architecture decisions",
    },
    RepoModule {
        path: "crates/storage",
        responsibility: "SQLite schema, migrations, and index metadata",
    },
    RepoModule {
        path: "crates/parser",
        responsibility: "PDF text extraction and OCR fallback",
    },
    RepoModule {
        path: "crates/retrieval",
        responsibility: "lexical recall, vector recall, and result fusion",
    },
    RepoModule {
        path: "apps/desktop",
        responsibility: "Tauri shell and local-first desktop UI",
    },
];

#[cfg(test)]
mod tests {
    use super::{DEFAULT_DECISIONS, DEFAULT_PIPELINE};

    #[test]
    fn pipeline_ends_with_answer_stage() {
        assert_eq!(DEFAULT_PIPELINE.last().unwrap().name, "answer");
    }

    #[test]
    fn mvp_starts_with_tree_rag_not_graph_rag() {
        assert!(DEFAULT_DECISIONS[0].mvp_choice.contains("RAPTOR-lite"));
    }
}
