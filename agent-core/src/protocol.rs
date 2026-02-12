use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_SCHEMA_VERSION: u32 = 1;

pub type SubmissionId = String;
pub type CheckpointId = String;
pub type ApprovalRequestId = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    UserInput {
        submission_id: SubmissionId,
        input: String,
    },
    Resume {
        submission_id: SubmissionId,
        checkpoint_id: Option<CheckpointId>,
    },
    HumanApprovalResponse {
        submission_id: SubmissionId,
        request_id: ApprovalRequestId,
        approved: bool,
        reason: Option<String>,
    },
}

impl Op {
    pub fn submission_id(&self) -> &str {
        match self {
            Self::UserInput { submission_id, .. } => submission_id,
            Self::Resume { submission_id, .. } => submission_id,
            Self::HumanApprovalResponse { submission_id, .. } => submission_id,
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
    CheckpointSaved {
        checkpoint_id: CheckpointId,
    },
    TurnResumed {
        checkpoint_id: Option<CheckpointId>,
    },
    HumanApprovalRequested {
        request_id: ApprovalRequestId,
        call: ToolCall,
        reason: String,
    },
    HumanApprovalResolved {
        request_id: ApprovalRequestId,
        approved: bool,
        reason: Option<String>,
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
    use serde_json::json;

    use super::{Event, EventKind, Op, StopReason, ToolCall};

    fn assert_stable_json<T: serde::Serialize>(actual: T, expected: &str) {
        let actual =
            serde_json::to_string_pretty(&actual).expect("serialize snapshot test value to json");
        assert_eq!(actual, expected);
    }

    #[test]
    fn protocol_schema_turn_stopped_json_shape_is_stable() {
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

    #[test]
    fn protocol_schema_resume_op_json_shape_is_stable() {
        let op = Op::Resume {
            submission_id: "submission-42".to_string(),
            checkpoint_id: Some("ckpt-1".to_string()),
        };
        let expected = r#"{
  "op": "resume",
  "submission_id": "submission-42",
  "checkpoint_id": "ckpt-1"
}"#;

        assert_stable_json(op, expected);
    }

    #[test]
    fn protocol_schema_human_approval_response_op_json_shape_is_stable() {
        let op = Op::HumanApprovalResponse {
            submission_id: "submission-42".to_string(),
            request_id: "approval-7".to_string(),
            approved: true,
            reason: Some("approved for test".to_string()),
        };
        let expected = r#"{
  "op": "human_approval_response",
  "submission_id": "submission-42",
  "request_id": "approval-7",
  "approved": true,
  "reason": "approved for test"
}"#;

        assert_stable_json(op, expected);
    }

    #[test]
    fn protocol_schema_checkpoint_saved_event_json_shape_is_stable() {
        let event = Event::new(
            "submission-42".to_string(),
            8,
            EventKind::CheckpointSaved {
                checkpoint_id: "ckpt-1".to_string(),
            },
        );
        let expected = r#"{
  "schema_version": 1,
  "submission_id": "submission-42",
  "index": 8,
  "kind": {
    "kind": "checkpoint_saved",
    "checkpoint_id": "ckpt-1"
  }
}"#;

        assert_stable_json(event, expected);
    }

    #[test]
    fn protocol_schema_turn_resumed_event_json_shape_is_stable() {
        let event = Event::new(
            "submission-42".to_string(),
            9,
            EventKind::TurnResumed {
                checkpoint_id: Some("ckpt-1".to_string()),
            },
        );
        let expected = r#"{
  "schema_version": 1,
  "submission_id": "submission-42",
  "index": 9,
  "kind": {
    "kind": "turn_resumed",
    "checkpoint_id": "ckpt-1"
  }
}"#;

        assert_stable_json(event, expected);
    }

    #[test]
    fn protocol_schema_human_approval_requested_event_json_shape_is_stable() {
        let event = Event::new(
            "submission-42".to_string(),
            10,
            EventKind::HumanApprovalRequested {
                request_id: "approval-7".to_string(),
                call: ToolCall {
                    name: "echo".to_string(),
                    args: json!({ "text": "hello" }),
                },
                reason: "manual approval required".to_string(),
            },
        );
        let expected = r#"{
  "schema_version": 1,
  "submission_id": "submission-42",
  "index": 10,
  "kind": {
    "kind": "human_approval_requested",
    "request_id": "approval-7",
    "call": {
      "name": "echo",
      "args": {
        "text": "hello"
      }
    },
    "reason": "manual approval required"
  }
}"#;

        assert_stable_json(event, expected);
    }

    #[test]
    fn protocol_schema_human_approval_resolved_event_json_shape_is_stable() {
        let event = Event::new(
            "submission-42".to_string(),
            11,
            EventKind::HumanApprovalResolved {
                request_id: "approval-7".to_string(),
                approved: false,
                reason: Some("rejected in test".to_string()),
            },
        );
        let expected = r#"{
  "schema_version": 1,
  "submission_id": "submission-42",
  "index": 11,
  "kind": {
    "kind": "human_approval_resolved",
    "request_id": "approval-7",
    "approved": false,
    "reason": "rejected in test"
  }
}"#;

        assert_stable_json(event, expected);
    }

    #[test]
    fn unknown_op_variant_is_rejected() {
        let err = serde_json::from_str::<Op>(
            r#"{"op":"unsupported_op","submission_id":"submission-42"}"#,
        )
        .expect_err("unknown op should fail");

        assert!(err.to_string().contains("unknown variant"));
    }

    #[test]
    fn unknown_event_kind_variant_is_rejected() {
        let err = serde_json::from_str::<Event>(
            r#"{
                "schema_version": 1,
                "submission_id": "submission-42",
                "index": 1,
                "kind": {
                    "kind": "unsupported_event_kind"
                }
            }"#,
        )
        .expect_err("unknown event kind should fail");

        assert!(err.to_string().contains("unknown variant"));
    }
}
