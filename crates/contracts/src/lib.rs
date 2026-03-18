use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DateRange {
    pub start: Option<String>,
    pub end: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectBrief {
    pub id: String,
    pub goal: String,
    pub constraints: Vec<String>,
    pub keywords: Vec<String>,
    pub date_range: Option<DateRange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", content = "value")]
pub enum PaperId {
    Doi(String),
    Arxiv(String),
    OpenAlex(String),
    PubMed(String),
    DerivedHash(String),
}

impl PaperId {
    pub fn as_key(&self) -> String {
        match self {
            PaperId::Doi(value) => format!("doi:{value}"),
            PaperId::Arxiv(value) => format!("arxiv:{value}"),
            PaperId::OpenAlex(value) => format!("openalex:{value}"),
            PaperId::PubMed(value) => format!("pubmed:{value}"),
            PaperId::DerivedHash(value) => format!("derived:{value}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaperRecord {
    pub paper_id: PaperId,
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<u16>,
    pub abstract_text: Option<String>,
    pub source_url: Option<String>,
    pub source_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ScreeningLabel {
    Include,
    Maybe,
    Exclude,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreeningDecision {
    pub project_id: String,
    pub paper_id: PaperId,
    pub label: ScreeningLabel,
    pub rationale: String,
    pub tags: Vec<String>,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopicCandidate {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub why_now: String,
    pub scope: String,
    pub representative_paper_ids: Vec<PaperId>,
    pub entry_risk: String,
    pub fallback_scope: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Frame,
    Collect,
    Screen,
    Propose,
    Done,
}

impl Phase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Phase::Frame => "frame",
            Phase::Collect => "collect",
            Phase::Screen => "screen",
            Phase::Propose => "propose",
            Phase::Done => "done",
        }
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Pending => "pending",
            RunStatus::Running => "running",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
        }
    }
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunManifest {
    pub run_id: String,
    pub project_id: String,
    pub phase: Phase,
    pub created_at: String,
    pub status: RunStatus,
}
