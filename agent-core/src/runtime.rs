use crate::{
    memory::EventStore,
    policy::PolicyEngine,
    protocol::{BudgetKind, Event, EventKind, Op, PolicyDecision, StopReason, ToolCall},
    tools::ToolRegistry,
};

pub trait LlmGateway {
    fn complete(&self, prompt: &str, recent_events: &[Event]) -> Result<LlmOutput, String>;
}

const DEFAULT_MAX_CONTROL_LOOP_ITERATIONS: usize = 16;

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub max_tool_calls: Option<u64>,
    pub max_events: Option<u64>,
    pub max_tokens: Option<u64>,
    pub max_control_loop_iterations: usize,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_tool_calls: None,
            max_events: None,
            max_tokens: None,
            max_control_loop_iterations: DEFAULT_MAX_CONTROL_LOOP_ITERATIONS,
        }
    }
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
    config: RuntimeConfig,
}

impl AgentRuntime {
    pub fn with_config(config: RuntimeConfig) -> Self {
        Self {
            next_event_index: 0,
            config,
        }
    }

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
                if let Some(result) = self.try_push_event(
                    &submission_id,
                    EventKind::UserInput {
                        input: input.clone(),
                    },
                    &mut emitted,
                    store,
                ) {
                    return result;
                }

                self.run_control_loop(
                    &submission_id,
                    &input,
                    llm,
                    policy,
                    tools,
                    &mut emitted,
                    store,
                    0,
                    0,
                )
            }
            Op::Resume { checkpoint_id, .. } => {
                if let Some(result) = self.try_push_event(
                    &submission_id,
                    EventKind::TurnResumed { checkpoint_id },
                    &mut emitted,
                    store,
                ) {
                    return result;
                }

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
                let request_id_for_lookup = request_id.clone();
                let pending_call =
                    find_pending_human_approval_call(store, &submission_id, &request_id_for_lookup);

                if let Some(result) = self.try_push_event(
                    &submission_id,
                    EventKind::HumanApprovalResolved {
                        request_id,
                        approved,
                        reason,
                    },
                    &mut emitted,
                    store,
                ) {
                    return result;
                }

                if !approved {
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

                let Some(pending_call) = pending_call else {
                    self.push_event(
                        &submission_id,
                        EventKind::LlmError {
                            message: format!(
                                "no pending human approval request: {request_id_for_lookup}"
                            ),
                        },
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
                };

                if let Some(result) = self.try_push_event(
                    &submission_id,
                    EventKind::TurnResumed {
                        checkpoint_id: None,
                    },
                    &mut emitted,
                    store,
                ) {
                    return result;
                }

                let total_tool_calls =
                    match self.consume_tool_call_budget(&submission_id, 0, &mut emitted, store) {
                        Ok(next) => next,
                        Err(result) => return result,
                    };

                let execution = tools.execute(&pending_call);
                let is_error = execution.is_error;
                if let Some(result) = self.try_push_event(
                    &submission_id,
                    EventKind::ToolExecuted { result: execution },
                    &mut emitted,
                    store,
                ) {
                    return result;
                }
                if is_error {
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

                let Some(prompt) = latest_user_input_for_submission(store, &submission_id) else {
                    self.push_event(
                        &submission_id,
                        EventKind::LlmError {
                            message: "cannot resume: missing prior user input".to_string(),
                        },
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
                };

                self.run_control_loop(
                    &submission_id,
                    &prompt,
                    llm,
                    policy,
                    tools,
                    &mut emitted,
                    store,
                    total_tool_calls,
                    0,
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_control_loop(
        &mut self,
        submission_id: &str,
        prompt: &str,
        llm: &dyn LlmGateway,
        policy: &dyn PolicyEngine,
        tools: &ToolRegistry,
        emitted: &mut Vec<Event>,
        store: &mut dyn EventStore,
        mut total_tool_calls: u64,
        mut total_tokens: u64,
    ) -> TurnResult {
        for _ in 0..self.config.max_control_loop_iterations {
            if let Some(result) = self.try_push_event(
                submission_id,
                EventKind::ContextBuilt {
                    recent_events: store.events().len(),
                },
                emitted,
                store,
            ) {
                return result;
            }

            let llm_output = match llm.complete(prompt, store.events()) {
                Ok(output) => output,
                Err(err) => {
                    if let Some(result) = self.try_push_event(
                        submission_id,
                        EventKind::LlmError { message: err },
                        emitted,
                        store,
                    ) {
                        return result;
                    }

                    self.push_event(
                        submission_id,
                        EventKind::TurnStopped {
                            reason: StopReason::Error,
                        },
                        emitted,
                        store,
                    );

                    return TurnResult {
                        stop_reason: StopReason::Error,
                        events: emitted.clone(),
                    };
                }
            };

            if let Some(result) = self.try_push_event(
                submission_id,
                EventKind::LlmText {
                    text: llm_output.text.clone(),
                },
                emitted,
                store,
            ) {
                return result;
            }

            total_tokens = total_tokens.saturating_add(estimate_text_tokens(&llm_output.text));
            if let Some(max_tokens) = self.config.max_tokens {
                if total_tokens > max_tokens {
                    return self.stop_for_budget(
                        submission_id,
                        BudgetKind::Tokens,
                        max_tokens,
                        total_tokens,
                        emitted,
                        store,
                    );
                }
            }

            if llm_output.tool_calls.is_empty() {
                self.push_event(
                    submission_id,
                    EventKind::TurnStopped {
                        reason: StopReason::Done,
                    },
                    emitted,
                    store,
                );

                return TurnResult {
                    stop_reason: StopReason::Done,
                    events: emitted.clone(),
                };
            }

            for call in llm_output.tool_calls {
                total_tool_calls = match self.consume_tool_call_budget(
                    submission_id,
                    total_tool_calls,
                    emitted,
                    store,
                ) {
                    Ok(next) => next,
                    Err(result) => return result,
                };

                if let Some(result) = self.try_push_event(
                    submission_id,
                    EventKind::ToolCallProposed { call: call.clone() },
                    emitted,
                    store,
                ) {
                    return result;
                }

                let decision = policy.decide(&call);
                if let Some(result) = self.try_push_event(
                    submission_id,
                    EventKind::PolicyEvaluated {
                        call: call.clone(),
                        decision: decision.clone(),
                    },
                    emitted,
                    store,
                ) {
                    return result;
                }

                match decision {
                    PolicyDecision::Allow => {
                        let result = tools.execute(&call);
                        let is_error = result.is_error;
                        if let Some(stop) = self.try_push_event(
                            submission_id,
                            EventKind::ToolExecuted { result },
                            emitted,
                            store,
                        ) {
                            return stop;
                        }

                        if is_error {
                            self.push_event(
                                submission_id,
                                EventKind::TurnStopped {
                                    reason: StopReason::Error,
                                },
                                emitted,
                                store,
                            );

                            return TurnResult {
                                stop_reason: StopReason::Error,
                                events: emitted.clone(),
                            };
                        }
                    }
                    PolicyDecision::Deny { .. } => {
                        self.push_event(
                            submission_id,
                            EventKind::TurnStopped {
                                reason: StopReason::PolicyDenied,
                            },
                            emitted,
                            store,
                        );

                        return TurnResult {
                            stop_reason: StopReason::PolicyDenied,
                            events: emitted.clone(),
                        };
                    }
                    PolicyDecision::AskHuman { reason } => {
                        let request_id = format!("approval-{}", self.next_event_index);
                        if let Some(result) = self.try_push_event(
                            submission_id,
                            EventKind::HumanApprovalRequested {
                                request_id,
                                call: call.clone(),
                                reason,
                            },
                            emitted,
                            store,
                        ) {
                            return result;
                        }

                        self.push_event(
                            submission_id,
                            EventKind::TurnStopped {
                                reason: StopReason::HumanApprovalRequired,
                            },
                            emitted,
                            store,
                        );

                        return TurnResult {
                            stop_reason: StopReason::HumanApprovalRequired,
                            events: emitted.clone(),
                        };
                    }
                }
            }
        }

        self.push_event(
            submission_id,
            EventKind::TurnStopped {
                reason: StopReason::Interrupted,
            },
            emitted,
            store,
        );

        TurnResult {
            stop_reason: StopReason::Interrupted,
            events: emitted.clone(),
        }
    }

    fn consume_tool_call_budget(
        &mut self,
        submission_id: &str,
        current_tool_calls: u64,
        emitted: &mut Vec<Event>,
        store: &mut dyn EventStore,
    ) -> Result<u64, TurnResult> {
        let next_tool_calls = current_tool_calls.saturating_add(1);
        if let Some(max_tool_calls) = self.config.max_tool_calls {
            if next_tool_calls > max_tool_calls {
                return Err(self.stop_for_budget(
                    submission_id,
                    BudgetKind::ToolCalls,
                    max_tool_calls,
                    next_tool_calls,
                    emitted,
                    store,
                ));
            }
        }
        Ok(next_tool_calls)
    }

    fn try_push_event(
        &mut self,
        submission_id: &str,
        kind: EventKind,
        emitted: &mut Vec<Event>,
        store: &mut dyn EventStore,
    ) -> Option<TurnResult> {
        if let Some(max_events) = self.config.max_events {
            let next_events = emitted.len() as u64 + 1;
            if next_events > max_events {
                return Some(self.stop_for_budget(
                    submission_id,
                    BudgetKind::Events,
                    max_events,
                    next_events,
                    emitted,
                    store,
                ));
            }
        }

        self.push_event(submission_id, kind, emitted, store);
        None
    }

    fn stop_for_budget(
        &mut self,
        submission_id: &str,
        budget: BudgetKind,
        limit: u64,
        observed: u64,
        emitted: &mut Vec<Event>,
        store: &mut dyn EventStore,
    ) -> TurnResult {
        self.push_event(
            submission_id,
            EventKind::BudgetExceeded {
                budget,
                limit,
                observed,
            },
            emitted,
            store,
        );
        self.push_event(
            submission_id,
            EventKind::TurnStopped {
                reason: StopReason::BudgetExceeded,
            },
            emitted,
            store,
        );

        TurnResult {
            stop_reason: StopReason::BudgetExceeded,
            events: emitted.clone(),
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

fn find_pending_human_approval_call(
    store: &dyn EventStore,
    submission_id: &str,
    request_id: &str,
) -> Option<ToolCall> {
    let mut requested_call = None;
    let mut resolved = false;
    for event in store.events_for_submission(submission_id) {
        match event.kind {
            EventKind::HumanApprovalRequested {
                request_id: event_request_id,
                call,
                ..
            } if event_request_id == request_id => {
                requested_call = Some(call);
                resolved = false;
            }
            EventKind::HumanApprovalResolved {
                request_id: event_request_id,
                ..
            } if event_request_id == request_id => {
                resolved = true;
            }
            _ => {}
        }
    }

    if resolved { None } else { requested_call }
}

fn latest_user_input_for_submission(store: &dyn EventStore, submission_id: &str) -> Option<String> {
    store
        .events_for_submission(submission_id)
        .into_iter()
        .rev()
        .find_map(|event| match event.kind {
            EventKind::UserInput { input } => Some(input),
            _ => None,
        })
}

fn estimate_text_tokens(text: &str) -> u64 {
    text.split_whitespace().count() as u64
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::{AgentRuntime, LlmGateway, LlmOutput, RuntimeConfig};
    use crate::{
        memory::{EventStore, InMemoryEventStore},
        policy::{AllowAllPolicy, DenyAllPolicy, PolicyEngine},
        protocol::{BudgetKind, Event, EventKind, Op, PolicyDecision, StopReason, ToolCall},
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

    struct SequenceGateway {
        outputs: Vec<LlmOutput>,
        next: Mutex<usize>,
    }

    impl SequenceGateway {
        fn new(outputs: Vec<LlmOutput>) -> Self {
            Self {
                outputs,
                next: Mutex::new(0),
            }
        }
    }

    impl LlmGateway for SequenceGateway {
        fn complete(&self, _prompt: &str, _recent_events: &[Event]) -> Result<LlmOutput, String> {
            let mut idx = self.next.lock().expect("sequence gateway mutex poisoned");
            let output = self.outputs.get(*idx).cloned().unwrap_or(LlmOutput {
                text: "done".to_string(),
                tool_calls: Vec::new(),
            });
            *idx += 1;
            Ok(output)
        }
    }

    struct AskHumanPolicy {
        reason: String,
    }

    impl AskHumanPolicy {
        fn new(reason: impl Into<String>) -> Self {
            Self {
                reason: reason.into(),
            }
        }
    }

    impl PolicyEngine for AskHumanPolicy {
        fn decide(&self, _call: &ToolCall) -> PolicyDecision {
            PolicyDecision::AskHuman {
                reason: self.reason.clone(),
            }
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
    fn human_approval_approved_resumes_and_executes_pending_call() {
        let mut runtime = AgentRuntime::default();
        let mut store = InMemoryEventStore::default();
        let mut tools = ToolRegistry::default();
        tools.register(EchoTool);
        let llm = SequenceGateway::new(vec![
            LlmOutput {
                text: "need approval".to_string(),
                tool_calls: vec![ToolCall {
                    name: "echo".to_string(),
                    args: json!({ "text": "approved" }),
                }],
            },
            LlmOutput {
                text: "done".to_string(),
                tool_calls: Vec::new(),
            },
        ]);

        let first = runtime.run_turn(
            Op::UserInput {
                submission_id: "sub-approval".to_string(),
                input: "needs approval".to_string(),
            },
            &llm,
            &AskHumanPolicy::new("needs human"),
            &tools,
            &mut store,
        );
        assert_eq!(first.stop_reason, StopReason::HumanApprovalRequired);

        let expected_request_id = first
            .events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::HumanApprovalRequested { request_id, .. } => Some(request_id.clone()),
                _ => None,
            })
            .expect("expected approval request event");

        let mut resumed_runtime = AgentRuntime::default();
        let second = resumed_runtime.run_turn(
            Op::HumanApprovalResponse {
                submission_id: "sub-approval".to_string(),
                request_id: expected_request_id.clone(),
                approved: true,
                reason: Some("approved in test".to_string()),
            },
            &llm,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );

        assert_eq!(second.stop_reason, StopReason::Done);
        assert!(store.events().iter().any(|event| {
            matches!(
                event.kind,
                EventKind::HumanApprovalResolved {
                    ref request_id,
                    approved: true,
                    reason: Some(ref reason),
                } if request_id == &expected_request_id && reason == "approved in test"
            )
        }));
        assert!(store.events().iter().any(|event| {
            matches!(
                event.kind,
                EventKind::ToolExecuted { ref result }
                    if result.call_name == "echo" && !result.is_error
            )
        }));
    }

    #[test]
    fn human_approval_denied_stops_without_executing_pending_call() {
        let mut runtime = AgentRuntime::default();
        let mut store = InMemoryEventStore::default();
        let mut tools = ToolRegistry::default();
        tools.register(EchoTool);
        let llm = SequenceGateway::new(vec![LlmOutput {
            text: "need approval".to_string(),
            tool_calls: vec![ToolCall {
                name: "echo".to_string(),
                args: json!({ "text": "should-not-run" }),
            }],
        }]);

        let first = runtime.run_turn(
            Op::UserInput {
                submission_id: "sub-approval-deny".to_string(),
                input: "needs approval".to_string(),
            },
            &llm,
            &AskHumanPolicy::new("needs human"),
            &tools,
            &mut store,
        );
        assert_eq!(first.stop_reason, StopReason::HumanApprovalRequired);

        let request_id = first
            .events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::HumanApprovalRequested { request_id, .. } => Some(request_id.clone()),
                _ => None,
            })
            .expect("expected approval request event");

        let second = runtime.run_turn(
            Op::HumanApprovalResponse {
                submission_id: "sub-approval-deny".to_string(),
                request_id,
                approved: false,
                reason: Some("denied in test".to_string()),
            },
            &llm,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );

        assert_eq!(second.stop_reason, StopReason::PolicyDenied);
        let executed = store
            .events()
            .iter()
            .any(|event| matches!(event.kind, EventKind::ToolExecuted { .. }));
        assert!(!executed);
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

        assert!(
            store
                .events()
                .windows(2)
                .all(|pair| pair[0].index < pair[1].index)
        );
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

        let first_index = result.events.first().expect("run should emit events").index;
        assert_eq!(first_index, 0);
    }

    #[test]
    fn multi_step_loop_executes_tools_across_iterations() {
        let mut store = InMemoryEventStore::default();
        let mut tools = ToolRegistry::default();
        tools.register(EchoTool);

        let llm = SequenceGateway::new(vec![
            LlmOutput {
                text: "step 1".to_string(),
                tool_calls: vec![ToolCall {
                    name: "echo".to_string(),
                    args: json!({ "text": "first" }),
                }],
            },
            LlmOutput {
                text: "step 2".to_string(),
                tool_calls: vec![ToolCall {
                    name: "echo".to_string(),
                    args: json!({ "text": "second" }),
                }],
            },
            LlmOutput {
                text: "final".to_string(),
                tool_calls: Vec::new(),
            },
        ]);

        let mut runtime = AgentRuntime::default();
        let result = runtime.run_turn(
            Op::UserInput {
                submission_id: "sub-multi-step".to_string(),
                input: "run multi-step".to_string(),
            },
            &llm,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );

        assert_eq!(result.stop_reason, StopReason::Done);
        let executed_outputs = store
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::ToolExecuted { result } => Some(result.output.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(executed_outputs.len(), 2);
        assert_eq!(executed_outputs[0], json!({ "echo": "first" }));
        assert_eq!(executed_outputs[1], json!({ "echo": "second" }));
    }

    #[test]
    fn tool_error_in_mid_loop_stops_with_error() {
        let mut store = InMemoryEventStore::default();
        let mut tools = ToolRegistry::default();
        tools.register(EchoTool);

        let llm = SequenceGateway::new(vec![
            LlmOutput {
                text: "step 1".to_string(),
                tool_calls: vec![ToolCall {
                    name: "echo".to_string(),
                    args: json!({ "text": "ok" }),
                }],
            },
            LlmOutput {
                text: "step 2".to_string(),
                tool_calls: vec![ToolCall {
                    name: "echo".to_string(),
                    args: json!({}),
                }],
            },
        ]);

        let mut runtime = AgentRuntime::default();
        let result = runtime.run_turn(
            Op::UserInput {
                submission_id: "sub-mid-error".to_string(),
                input: "run until error".to_string(),
            },
            &llm,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );

        assert_eq!(result.stop_reason, StopReason::Error);

        let has_error_tool_result = store.events().iter().any(|event| {
            matches!(
                event.kind,
                EventKind::ToolExecuted {
                    ref result
                } if result.call_name == "echo" && result.is_error
            )
        });
        assert!(has_error_tool_result);

        let last_event = store.events().last().expect("store should have events");
        assert!(matches!(
            last_event.kind,
            EventKind::TurnStopped {
                reason: StopReason::Error
            }
        ));
    }

    #[test]
    fn max_tool_calls_budget_stops_with_budget_exceeded() {
        let mut store = InMemoryEventStore::default();
        let mut tools = ToolRegistry::default();
        tools.register(EchoTool);

        let llm = SequenceGateway::new(vec![
            LlmOutput {
                text: "step 1".to_string(),
                tool_calls: vec![ToolCall {
                    name: "echo".to_string(),
                    args: json!({ "text": "one" }),
                }],
            },
            LlmOutput {
                text: "step 2".to_string(),
                tool_calls: vec![ToolCall {
                    name: "echo".to_string(),
                    args: json!({ "text": "two" }),
                }],
            },
        ]);

        let config = RuntimeConfig {
            max_tool_calls: Some(1),
            ..RuntimeConfig::default()
        };
        let mut runtime = AgentRuntime::with_config(config);
        let result = runtime.run_turn(
            Op::UserInput {
                submission_id: "sub-tool-budget".to_string(),
                input: "budget".to_string(),
            },
            &llm,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );

        assert_eq!(result.stop_reason, StopReason::BudgetExceeded);
        assert!(store.events().iter().any(|event| {
            matches!(
                event.kind,
                EventKind::BudgetExceeded {
                    budget: BudgetKind::ToolCalls,
                    limit: 1,
                    observed: 2
                }
            )
        }));
    }

    #[test]
    fn max_events_budget_stops_with_budget_exceeded() {
        let mut store = InMemoryEventStore::default();
        let mut tools = ToolRegistry::default();
        tools.register(EchoTool);

        let llm = StaticGateway {
            text: "hello world".to_string(),
            tool_calls: Vec::new(),
        };

        let config = RuntimeConfig {
            max_events: Some(2),
            ..RuntimeConfig::default()
        };
        let mut runtime = AgentRuntime::with_config(config);
        let result = runtime.run_turn(
            Op::UserInput {
                submission_id: "sub-event-budget".to_string(),
                input: "budget".to_string(),
            },
            &llm,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );

        assert_eq!(result.stop_reason, StopReason::BudgetExceeded);
        assert!(store.events().iter().any(|event| {
            matches!(
                event.kind,
                EventKind::BudgetExceeded {
                    budget: BudgetKind::Events,
                    limit: 2,
                    observed: 3
                }
            )
        }));
    }

    #[test]
    fn max_tokens_budget_stops_with_budget_exceeded() {
        let mut store = InMemoryEventStore::default();
        let mut tools = ToolRegistry::default();
        tools.register(EchoTool);

        let llm = StaticGateway {
            text: "one two three".to_string(),
            tool_calls: Vec::new(),
        };

        let config = RuntimeConfig {
            max_tokens: Some(2),
            ..RuntimeConfig::default()
        };
        let mut runtime = AgentRuntime::with_config(config);
        let result = runtime.run_turn(
            Op::UserInput {
                submission_id: "sub-token-budget".to_string(),
                input: "budget".to_string(),
            },
            &llm,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );

        assert_eq!(result.stop_reason, StopReason::BudgetExceeded);
        assert!(store.events().iter().any(|event| {
            matches!(
                event.kind,
                EventKind::BudgetExceeded {
                    budget: BudgetKind::Tokens,
                    limit: 2,
                    observed: 3
                }
            )
        }));
    }
}
