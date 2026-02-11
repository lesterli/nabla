use crate::{
    memory::EventStore,
    policy::PolicyEngine,
    protocol::{Event, EventKind, Op, PolicyDecision, StopReason, ToolCall},
    tools::ToolRegistry,
};

pub trait LlmGateway {
    fn complete(&self, prompt: &str, recent_events: &[Event]) -> Result<LlmOutput, String>;
}

#[derive(Debug, Clone)]
pub struct LlmOutput {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone)]
pub struct TurnResult {
    pub stop_reason: StopReason,
    pub events: Vec<Event>,
}

#[derive(Debug, Default)]
pub struct AgentRuntime {
    next_event_index: u64,
}

impl AgentRuntime {
    pub fn run_turn(
        &mut self,
        op: Op,
        llm: &dyn LlmGateway,
        policy: &dyn PolicyEngine,
        tools: &ToolRegistry,
        store: &mut dyn EventStore,
    ) -> TurnResult {
        let submission_id = op.submission_id().to_string();
        let mut emitted = Vec::new();

        match op {
            Op::UserInput { input, .. } => {
                self.push_event(
                    &submission_id,
                    EventKind::UserInput {
                        input: input.clone(),
                    },
                    &mut emitted,
                    store,
                );

                self.push_event(
                    &submission_id,
                    EventKind::ContextBuilt {
                        recent_events: store.events().len(),
                    },
                    &mut emitted,
                    store,
                );

                let llm_output = match llm.complete(&input, store.events()) {
                    Ok(output) => output,
                    Err(err) => {
                        self.push_event(
                            &submission_id,
                            EventKind::LlmError { message: err },
                            &mut emitted,
                            store,
                        );

                        self.push_event(
                            &submission_id,
                            EventKind::TurnStopped {
                                reason: StopReason::Error,
                            },
                            &mut emitted,
                            store,
                        );

                        return TurnResult {
                            stop_reason: StopReason::Error,
                            events: emitted,
                        };
                    }
                };

                self.push_event(
                    &submission_id,
                    EventKind::LlmText {
                        text: llm_output.text,
                    },
                    &mut emitted,
                    store,
                );

                for call in llm_output.tool_calls {
                    self.push_event(
                        &submission_id,
                        EventKind::ToolCallProposed { call: call.clone() },
                        &mut emitted,
                        store,
                    );

                    let decision = policy.decide(&call);
                    self.push_event(
                        &submission_id,
                        EventKind::PolicyEvaluated {
                            call: call.clone(),
                            decision: decision.clone(),
                        },
                        &mut emitted,
                        store,
                    );

                    match decision {
                        PolicyDecision::Allow => {
                            let result = tools.execute(&call);
                            self.push_event(
                                &submission_id,
                                EventKind::ToolExecuted { result },
                                &mut emitted,
                                store,
                            );
                        }
                        PolicyDecision::Deny { .. } => {
                            self.push_event(
                                &submission_id,
                                EventKind::TurnStopped {
                                    reason: StopReason::PolicyDenied,
                                },
                                &mut emitted,
                                store,
                            );

                            return TurnResult {
                                stop_reason: StopReason::PolicyDenied,
                                events: emitted,
                            };
                        }
                        PolicyDecision::AskHuman { .. } => {
                            self.push_event(
                                &submission_id,
                                EventKind::TurnStopped {
                                    reason: StopReason::HumanApprovalRequired,
                                },
                                &mut emitted,
                                store,
                            );

                            return TurnResult {
                                stop_reason: StopReason::HumanApprovalRequired,
                                events: emitted,
                            };
                        }
                    }
                }

                self.push_event(
                    &submission_id,
                    EventKind::TurnStopped {
                        reason: StopReason::Done,
                    },
                    &mut emitted,
                    store,
                );

                TurnResult {
                    stop_reason: StopReason::Done,
                    events: emitted,
                }
            }
        }
    }

    fn push_event(
        &mut self,
        submission_id: &str,
        kind: EventKind,
        emitted: &mut Vec<Event>,
        store: &mut dyn EventStore,
    ) {
        let event = Event::new(submission_id.to_string(), self.next_event_index, kind);
        self.next_event_index += 1;
        store.append(event.clone());
        emitted.push(event);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AgentRuntime, LlmGateway, LlmOutput};
    use crate::{
        memory::{EventStore, InMemoryEventStore},
        policy::DenyAllPolicy,
        protocol::{Event, EventKind, Op, StopReason, ToolCall},
        tools::{EchoTool, ToolRegistry},
    };

    struct StaticGateway {
        text: String,
        tool_calls: Vec<ToolCall>,
    }

    impl LlmGateway for StaticGateway {
        fn complete(&self, _prompt: &str, _recent_events: &[Event]) -> Result<LlmOutput, String> {
            Ok(LlmOutput {
                text: self.text.clone(),
                tool_calls: self.tool_calls.clone(),
            })
        }
    }

    #[test]
    fn policy_denial_stops_before_tool_execution() {
        let mut runtime = AgentRuntime::default();
        let mut store = InMemoryEventStore::default();
        let mut tools = ToolRegistry::default();
        tools.register(EchoTool);

        let llm = StaticGateway {
            text: "call tool".to_string(),
            tool_calls: vec![ToolCall {
                name: "echo".to_string(),
                args: json!({ "text": "hello" }),
            }],
        };

        let result = runtime.run_turn(
            Op::UserInput {
                submission_id: "sub-1".to_string(),
                input: "hi".to_string(),
            },
            &llm,
            &DenyAllPolicy::new("blocked"),
            &tools,
            &mut store,
        );

        assert_eq!(result.stop_reason, StopReason::PolicyDenied);
        assert!(
            store
                .events()
                .iter()
                .all(|event| event.submission_id == "sub-1")
        );

        let executed = store
            .events()
            .iter()
            .any(|event| matches!(event.kind, EventKind::ToolExecuted { .. }));
        assert!(!executed);
    }
}
