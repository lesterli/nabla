use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

// ─── Newtype IDs ───────────────────────────────────────────────────────────

macro_rules! define_id {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }
    };
}

define_id!(LibraryId, "Unique identifier for a local knowledge library.");
define_id!(BatchId, "Unique identifier for an import batch.");
define_id!(DocumentId, "Unique identifier for a PDF document.");
define_id!(ChunkId, "Unique identifier for a retrieval chunk.");
define_id!(SummaryNodeId, "Unique identifier for a summary tree node.");

// ─── Enum Display/FromStr ──────────────────────────────────────────────────

macro_rules! string_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    $(Self::$variant => write!(f, stringify!($variant)),)+
                }
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $(stringify!($variant) => Ok(Self::$variant),)+
                    other => Err(format!("unknown {} variant: {}", stringify!($name), other)),
                }
            }
        }
    };
}

// ─── Enums ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
}
string_enum!(JobStatus { Pending, Running, Completed, Failed });

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DocumentState {
    Queued,
    Extracting,
    Chunking,
    Summarizing,
    Embedding,
    Ready,
    Failed,
}
string_enum!(DocumentState { Queued, Extracting, Chunking, Summarizing, Embedding, Ready, Failed });

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SummaryNodeKind {
    Section,
    Cluster,
    Document,
}
string_enum!(SummaryNodeKind { Section, Cluster, Document });

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EmbeddingState {
    Pending,
    Indexed,
    Failed,
}
string_enum!(EmbeddingState { Pending, Indexed, Failed });

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RetrievalMode {
    Fast,
    Balanced,
    Deep,
}

// ─── Value Objects ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PageSpan {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Citation {
    pub document_id: DocumentId,
    pub file_name: String,
    pub page_span: Option<PageSpan>,
    pub quote: Option<String>,
}

// ─── Records ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LibraryRecord {
    pub id: LibraryId,
    pub name: String,
    pub root_dir: String,
    pub created_at: String,
    /// Path to a prompt template file for this library's scenario (e.g., "prompts/hr.md").
    pub prompt_template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImportBatchRequest {
    pub library_id: LibraryId,
    pub paths: Vec<String>,
    pub recursive: bool,
    pub copy_files: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImportBatchRecord {
    pub id: BatchId,
    pub library_id: LibraryId,
    pub requested_paths: Vec<String>,
    pub status: JobStatus,
    pub total_files: usize,
    pub imported_files: usize,
    pub failed_files: usize,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocumentRecord {
    pub id: DocumentId,
    pub library_id: LibraryId,
    pub batch_id: Option<BatchId>,
    pub file_name: String,
    pub source_path: String,
    pub checksum_sha256: String,
    pub page_count: Option<u32>,
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub state: DocumentState,
    pub created_at: String,
    pub updated_at: String,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SummaryNode {
    pub id: SummaryNodeId,
    pub document_id: DocumentId,
    pub parent_id: Option<SummaryNodeId>,
    pub kind: SummaryNodeKind,
    pub depth: u8,
    pub ordinal: u32,
    pub title: String,
    pub page_span: Option<PageSpan>,
    pub summary: String,
    pub child_ids: Vec<SummaryNodeId>,
    pub source_chunk_ids: Vec<ChunkId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkRecord {
    pub id: ChunkId,
    pub document_id: DocumentId,
    pub summary_node_id: Option<SummaryNodeId>,
    pub ordinal: u32,
    pub heading_path: Vec<String>,
    pub page_span: Option<PageSpan>,
    pub text: String,
    pub token_count: u32,
    pub embedding_state: EmbeddingState,
}

// ─── Retrieval ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetrievalQuery {
    pub library_id: LibraryId,
    pub prompt: String,
    pub max_chunks: usize,
    pub max_summaries: usize,
    pub mode: RetrievalMode,
}

/// Describes which recall channel produced a hit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RecallSource {
    /// Dense vector (ANN) recall.
    Vector,
    /// Lexical / BM25 recall.
    Lexical,
    /// Summary-tree traversal.
    SummaryTree,
}

/// Type-safe wrapper for the node that was hit — either a chunk or a summary node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum HitNodeId {
    Chunk(ChunkId),
    Summary(SummaryNodeId),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetrievalHit {
    pub document_id: DocumentId,
    pub node_id: HitNodeId,
    pub score: f32,
    /// Which recall channel(s) produced this hit.
    pub sources: Vec<RecallSource>,
    pub summary: String,
    pub citation: Citation,
}

// ─── Answer ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnswerDraft {
    pub answer: String,
    pub citations: Vec<Citation>,
    pub follow_up_questions: Vec<String>,
    pub confidence: Option<f32>,
    /// Optional structured output for template-driven extraction.
    pub structured_fields: Option<serde_json::Value>,
}
