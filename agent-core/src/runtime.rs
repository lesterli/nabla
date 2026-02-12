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
        self.sync_next_event_index(store);

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
            Op::Resume { checkpoint_id, .. } => {
                self.push_event(
                    &submission_id,
                    EventKind::TurnResumed { checkpoint_id },
                    &mut emitted,
                    store,
                );

                self.push_event(
                    &submission_id,
                    EventKind::TurnStopped {
                        reason: StopReason::Interrupted,
                    },
                    &mut emitted,
                    store,
                );

                TurnResult {
                    stop_reason: StopReason::Interrupted,
                    events: emitted,
                }
            }
            Op::HumanApprovalResponse {
                request_id,
                approved,
                reason,
                ..
            } => {
                self.push_event(
                    &submission_id,
                    EventKind::HumanApprovalResolved {
                        request_id,
                        approved,
                        reason,
                    },
                    &mut emitted,
                    store,
                );

                self.push_event(
                    &submission_id,
                    EventKind::TurnStopped {
                        reason: StopReason::Interrupted,
                    },
                    &mut emitted,
                    store,
                );

                TurnResult {
                    stop_reason: StopReason::Interrupted,
                    events: emitted,
                }
            }
        }
    }

    fn sync_next_event_index(&mut self, store: &dyn EventStore) {
        let Some(last_index) = store.last_event_index() else {
            return;
        };

        let next_after_store = last_index.saturating_add(1);
        if self.next_event_index < next_after_store {
            self.next_event_index = next_after_store;
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
        policy::{AllowAllPolicy, DenyAllPolicy},
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

    #[test]
    fn resume_op_emits_turn_resumed_and_stops_interrupted() {
        let mut runtime = AgentRuntime::default();
        let mut store = InMemoryEventStore::default();
        let tools = ToolRegistry::default();
        let llm = StaticGateway {
            text: "unused".to_string(),
            tool_calls: Vec::new(),
        };

        let result = runtime.run_turn(
            Op::Resume {
                submission_id: "sub-resume".to_string(),
                checkpoint_id: Some("ckpt-1".to_string()),
            },
            &llm,
            &DenyAllPolicy::new("unused"),
            &tools,
            &mut store,
        );

        assert_eq!(result.stop_reason, StopReason::Interrupted);
        assert!(store.events().iter().any(|event| {
            matches!(
                event.kind,
                EventKind::TurnResumed {
                    checkpoint_id: Some(ref checkpoint_id),
                } if checkpoint_id == "ckpt-1"
            )
        }));
    }

    #[test]
    fn human_approval_response_op_emits_resolution_and_stops_interrupted() {
        let mut runtime = AgentRuntime::default();
        let mut store = InMemoryEventStore::default();
        let tools = ToolRegistry::default();
        let llm = StaticGateway {
            text: "unused".to_string(),
            tool_calls: Vec::new(),
        };

        let result = runtime.run_turn(
            Op::HumanApprovalResponse {
                submission_id: "sub-approval".to_string(),
                request_id: "approval-1".to_string(),
                approved: true,
                reason: Some("approved in test".to_string()),
            },
            &llm,
            &DenyAllPolicy::new("unused"),
            &tools,
            &mut store,
        );

        assert_eq!(result.stop_reason, StopReason::Interrupted);
        assert!(store.events().iter().any(|event| {
            matches!(
                event.kind,
                EventKind::HumanApprovalResolved {
                    ref request_id,
                    approved: true,
                    reason: Some(ref reason),
                } if request_id == "approval-1" && reason == "approved in test"
            )
        }));
    }

    #[test]
    fn event_index_keeps_increasing_after_runtime_restart() {
        let mut store = InMemoryEventStore::default();
        let mut tools = ToolRegistry::default();
        tools.register(EchoTool);

        let llm = StaticGateway {
            text: "ok".to_string(),
            tool_calls: vec![ToolCall {
                name: "echo".to_string(),
                args: json!({ "text": "hello" }),
            }],
        };

        let mut runtime_first = AgentRuntime::default();
        let first_result = runtime_first.run_turn(
            Op::UserInput {
                submission_id: "sub-1".to_string(),
                input: "first".to_string(),
            },
            &llm,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );
        let first_last_index = first_result
            .events
            .last()
            .expect("first run should emit events")
            .index;

        let mut runtime_second = AgentRuntime::default();
        let second_result = runtime_second.run_turn(
            Op::UserInput {
                submission_id: "sub-2".to_string(),
                input: "second".to_string(),
            },
            &llm,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );

        let second_first_index = second_result
            .events
            .first()
            .expect("second run should emit events")
            .index;
        assert_eq!(second_first_index, first_last_index + 1);

        assert!(store.events().windows(2).all(|pair| pair[0].index < pair[1].index));
    }

    #[test]
    fn empty_store_starts_event_index_at_zero() {
        let mut store = InMemoryEventStore::default();
        let mut tools = ToolRegistry::default();
        tools.register(EchoTool);

        let llm = StaticGateway {
            text: "ok".to_string(),
            tool_calls: Vec::new(),
        };

        let mut runtime = AgentRuntime::default();
        let result = runtime.run_turn(
            Op::UserInput {
                submission_id: "sub-empty".to_string(),
                input: "first".to_string(),
            },
            &llm,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );

        let first_index = result
            .events
            .first()
            .expect("run should emit events")
            .index;
        assert_eq!(first_index, 0);
    }
}
