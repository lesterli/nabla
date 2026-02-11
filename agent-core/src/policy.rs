use crate::protocol::{PolicyDecision, ToolCall};

pub trait PolicyEngine {
    fn decide(&self, call: &ToolCall) -> PolicyDecision;
}

#[derive(Debug, Default)]
pub struct AllowAllPolicy;

impl PolicyEngine for AllowAllPolicy {
    fn decide(&self, _call: &ToolCall) -> PolicyDecision {
        PolicyDecision::Allow
    }
}

#[derive(Debug)]
pub struct DenyAllPolicy {
    reason: String,
}

impl DenyAllPolicy {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl PolicyEngine for DenyAllPolicy {
    fn decide(&self, _call: &ToolCall) -> PolicyDecision {
        PolicyDecision::Deny {
            reason: self.reason.clone(),
        }
    }
}
