use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_SCHEMA_VERSION: u32 = 1;

pub type SubmissionId = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    UserInput {
        submission_id: SubmissionId,
        input: String,
    },
}

impl Op {
    pub fn submission_id(&self) -> &str {
        match self {
            Self::UserInput { submission_id, .. } => submission_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_name: String,
    pub output: Value,
    pub is_error: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PolicyDecision {
    Allow,
    Deny { reason: String },
    AskHuman { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Done,
    Interrupted,
    Error,
    BudgetExceeded,
    PolicyDenied,
    HumanApprovalRequired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    UserInput {
        input: String,
    },
    ContextBuilt {
        recent_events: usize,
    },
    LlmText {
        text: String,
    },
    LlmError {
        message: String,
    },
    ToolCallProposed {
        call: ToolCall,
    },
    PolicyEvaluated {
        call: ToolCall,
        decision: PolicyDecision,
    },
    ToolExecuted {
        result: ToolResult,
    },
    TurnStopped {
        reason: StopReason,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub schema_version: u32,
    pub submission_id: SubmissionId,
    pub index: u64,
    pub kind: EventKind,
}

impl Event {
    pub fn new(submission_id: SubmissionId, index: u64, kind: EventKind) -> Self {
        Self {
            schema_version: PROTOCOL_SCHEMA_VERSION,
            submission_id,
            index,
            kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Event, EventKind, StopReason};

    #[test]
    fn protocol_schema_json_shape_is_stable() {
        let event = Event::new(
            "submission-42".to_string(),
            7,
            EventKind::TurnStopped {
                reason: StopReason::Done,
            },
        );

        let actual = serde_json::to_string_pretty(&event).expect("serialize event");
        let expected = r#"{
  "schema_version": 1,
  "submission_id": "submission-42",
  "index": 7,
  "kind": {
    "kind": "turn_stopped",
    "reason": "done"
  }
}"#;

        assert_eq!(actual, expected);
    }
}
