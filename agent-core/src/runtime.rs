use std::collections::{HashMap, HashSet};

use crate::{
    memory::EventStore,
    policy::PolicyEngine,
    protocol::{
        BudgetExceededFact, BudgetKind, Event, EventKind, Op, PolicyDecision, StopFacts,
        StopReason, ToolCall, ToolResult,
    },
    tools::{ToolRegistry, idempotency_key_for_call},
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
    pub stop_facts: StopFacts,
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

                let mut idempotency_results = build_idempotency_result_cache(store, &submission_id);
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
                    &mut idempotency_results,
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

                let Some(prompt) = latest_user_input_for_submission(store, &submission_id) else {
                    self.push_event(
                        &submission_id,
                        EventKind::LlmError {
                            message: "cannot resume: missing prior user input".to_string(),
                        },
                        &mut emitted,
                        store,
                    );
                    return self.finalize_turn(
                        &submission_id,
                        StopReason::Error,
                        &mut emitted,
                        store,
                    );
                };

                let mut idempotency_results = build_idempotency_result_cache(store, &submission_id);
                self.run_control_loop(
                    &submission_id,
                    &prompt,
                    llm,
                    policy,
                    tools,
                    &mut emitted,
                    store,
                    0,
                    0,
                    &mut idempotency_results,
                )
            }
            Op::HumanApprovalResponse {
                request_id,
                approved,
                reason,
                ..
            } => {
                let mut idempotency_results = build_idempotency_result_cache(store, &submission_id);
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
                    return self.finalize_turn(
                        &submission_id,
                        StopReason::PolicyDenied,
                        &mut emitted,
                        store,
                    );
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
                    return self.finalize_turn(
                        &submission_id,
                        StopReason::Error,
                        &mut emitted,
                        store,
                    );
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

                let execution = if let Some(idempotency_key) =
                    idempotency_key_for_call(&pending_call)
                {
                    if let Some(cached) = idempotency_results.get(idempotency_key) {
                        cached.clone()
                    } else {
                        let executed = tools.execute(&pending_call);
                        idempotency_results.insert(idempotency_key.to_string(), executed.clone());
                        executed
                    }
                } else {
                    tools.execute(&pending_call)
                };
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
                    return self.finalize_turn(
                        &submission_id,
                        StopReason::Error,
                        &mut emitted,
                        store,
                    );
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
                    return self.finalize_turn(
                        &submission_id,
                        StopReason::Error,
                        &mut emitted,
                        store,
                    );
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
                    &mut idempotency_results,
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
        idempotency_results: &mut HashMap<String, ToolResult>,
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

                    return self.finalize_turn(submission_id, StopReason::Error, emitted, store);
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
                return self.finalize_turn(submission_id, StopReason::Done, emitted, store);
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
                        let result = if let Some(idempotency_key) = idempotency_key_for_call(&call)
                        {
                            if let Some(cached) = idempotency_results.get(idempotency_key) {
                                cached.clone()
                            } else {
                                let executed = tools.execute(&call);
                                idempotency_results
                                    .insert(idempotency_key.to_string(), executed.clone());
                                executed
                            }
                        } else {
                            tools.execute(&call)
                        };
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
                            return self.finalize_turn(
                                submission_id,
                                StopReason::Error,
                                emitted,
                                store,
                            );
                        }
                    }
                    PolicyDecision::Deny { .. } => {
                        return self.finalize_turn(
                            submission_id,
                            StopReason::PolicyDenied,
                            emitted,
                            store,
                        );
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

                        return self.finalize_turn(
                            submission_id,
                            StopReason::HumanApprovalRequired,
                            emitted,
                            store,
                        );
                    }
                }
            }
        }

        self.finalize_turn(submission_id, StopReason::Interrupted, emitted, store)
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
        self.finalize_turn(submission_id, StopReason::BudgetExceeded, emitted, store)
    }

    fn finalize_turn(
        &mut self,
        submission_id: &str,
        stop_reason: StopReason,
        emitted: &mut Vec<Event>,
        store: &mut dyn EventStore,
    ) -> TurnResult {
        let stop_facts = build_stop_facts(stop_reason.clone(), emitted, store, submission_id);
        self.push_event(
            submission_id,
            EventKind::TurnStopped {
                reason: stop_reason.clone(),
                facts: Some(stop_facts.clone()),
            },
            emitted,
            store,
        );
        TurnResult {
            stop_reason,
            stop_facts,
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

fn build_idempotency_result_cache(
    store: &dyn EventStore,
    submission_id: &str,
) -> HashMap<String, ToolResult> {
    let mut cache = HashMap::new();
    let mut pending_idempotency_key: Option<String> = None;

    for event in store.events_for_submission(submission_id) {
        match event.kind {
            EventKind::ToolCallProposed { call } => {
                pending_idempotency_key = idempotency_key_for_call(&call).map(str::to_string);
            }
            EventKind::ToolExecuted { result } => {
                if let Some(idempotency_key) = pending_idempotency_key.take() {
                    cache.insert(idempotency_key, result);
                }
            }
            _ => {}
        }
    }

    cache
}

fn estimate_text_tokens(text: &str) -> u64 {
    text.split_whitespace().count() as u64
}

fn build_stop_facts(
    stop_reason: StopReason,
    emitted: &[Event],
    store: &dyn EventStore,
    submission_id: &str,
) -> StopFacts {
    let mut last_tool_calls = emitted
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolCallProposed { call } => Some(call.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    const MAX_STOP_FACTS_TOOL_CALLS: usize = 8;
    if last_tool_calls.len() > MAX_STOP_FACTS_TOOL_CALLS {
        let start = last_tool_calls.len() - MAX_STOP_FACTS_TOOL_CALLS;
        last_tool_calls = last_tool_calls.split_off(start);
    }

    let tool_error_count = emitted
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                EventKind::ToolExecuted {
                    result: ToolResult { is_error: true, .. }
                }
            )
        })
        .count() as u64;

    StopFacts {
        stop_reason,
        budget_exceeded: extract_budget_exceeded_fact(emitted),
        tool_error_count,
        last_tool_calls,
        has_pending_approval: has_pending_human_approval(store, submission_id),
    }
}

fn extract_budget_exceeded_fact(events: &[Event]) -> Option<BudgetExceededFact> {
    events.iter().rev().find_map(|event| match &event.kind {
        EventKind::BudgetExceeded {
            budget,
            limit,
            observed,
        } => Some(BudgetExceededFact {
            budget: budget.clone(),
            limit: *limit,
            observed: *observed,
        }),
        _ => None,
    })
}

fn has_pending_human_approval(store: &dyn EventStore, submission_id: &str) -> bool {
    let mut pending = HashSet::new();
    for event in store.events_for_submission(submission_id) {
        match event.kind {
            EventKind::HumanApprovalRequested { request_id, .. } => {
                pending.insert(request_id);
            }
            EventKind::HumanApprovalResolved { request_id, .. } => {
                pending.remove(&request_id);
            }
            _ => {}
        }
    }

    !pending.is_empty()
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use serde_json::json;

    use super::{AgentRuntime, LlmGateway, LlmOutput, RuntimeConfig};
    use crate::{
        memory::{EventStore, InMemoryEventStore},
        policy::{AllowAllPolicy, DenyAllPolicy, PolicyEngine},
        protocol::{
            BudgetExceededFact, BudgetKind, Event, EventKind, Op, PolicyDecision, StopReason,
            ToolCall,
        },
        tools::{EchoTool, Tool, ToolRegistry},
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
        assert_eq!(result.stop_facts.stop_reason, StopReason::PolicyDenied);
        assert!(!result.stop_facts.has_pending_approval);
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
    fn resume_op_continues_control_loop_with_prior_user_input() {
        let mut runtime = AgentRuntime::default();
        let mut store = InMemoryEventStore::default();
        let tools = ToolRegistry::default();
        let llm = StaticGateway {
            text: "continued".to_string(),
            tool_calls: Vec::new(),
        };

        store.append(Event::new(
            "sub-resume".to_string(),
            0,
            EventKind::UserInput {
                input: "previous prompt".to_string(),
            },
        ));

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

        assert_eq!(result.stop_reason, StopReason::Done);
        assert_eq!(result.stop_facts.stop_reason, StopReason::Done);
        assert!(store.events().iter().any(|event| {
            matches!(
                event.kind,
                EventKind::TurnResumed {
                    checkpoint_id: Some(ref checkpoint_id),
                } if checkpoint_id == "ckpt-1"
            )
        }));
        assert!(store.events().iter().any(|event| {
            matches!(
                event.kind,
                EventKind::LlmText { ref text } if text == "continued"
            )
        }));
    }

    #[test]
    fn resume_without_prior_user_input_stops_with_error() {
        let mut runtime = AgentRuntime::default();
        let mut store = InMemoryEventStore::default();
        let tools = ToolRegistry::default();
        let llm = StaticGateway {
            text: "unused".to_string(),
            tool_calls: Vec::new(),
        };

        let result = runtime.run_turn(
            Op::Resume {
                submission_id: "sub-empty".to_string(),
                checkpoint_id: None,
            },
            &llm,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );

        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(result.stop_facts.stop_reason, StopReason::Error);
        assert!(store.events().iter().any(|event| {
            matches!(
                event.kind,
                EventKind::LlmError { ref message }
                    if message == "cannot resume: missing prior user input"
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
        assert_eq!(
            first.stop_facts.stop_reason,
            StopReason::HumanApprovalRequired
        );
        assert!(first.stop_facts.has_pending_approval);

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
        assert_eq!(second.stop_facts.stop_reason, StopReason::Done);
        assert!(!second.stop_facts.has_pending_approval);
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
        assert_eq!(second.stop_facts.stop_reason, StopReason::PolicyDenied);
        assert!(!second.stop_facts.has_pending_approval);
        let executed = store
            .events()
            .iter()
            .any(|event| matches!(event.kind, EventKind::ToolExecuted { .. }));
        assert!(!executed);
    }

    struct CounterTool {
        runs: Arc<AtomicUsize>,
    }

    impl Tool for CounterTool {
        fn name(&self) -> &str {
            "counter"
        }

        fn run(&self, _args: &serde_json::Value) -> Result<serde_json::Value, String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        }
    }

    #[test]
    fn duplicate_idempotency_key_reuses_previous_result_without_rerun() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut tools = ToolRegistry::default();
        tools.register(CounterTool { runs: runs.clone() });

        let llm_first = SequenceGateway::new(vec![
            LlmOutput {
                text: "first".to_string(),
                tool_calls: vec![ToolCall {
                    name: "counter".to_string(),
                    args: json!({ "value": "x", "_idempotency_key": "counter-key-1" }),
                }],
            },
            LlmOutput {
                text: "done".to_string(),
                tool_calls: Vec::new(),
            },
        ]);

        let mut runtime_first = AgentRuntime::default();
        let mut store = InMemoryEventStore::default();
        let first = runtime_first.run_turn(
            Op::UserInput {
                submission_id: "sub-idempotency".to_string(),
                input: "first".to_string(),
            },
            &llm_first,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );
        assert_eq!(first.stop_reason, StopReason::Done);
        assert_eq!(first.stop_facts.stop_reason, StopReason::Done);
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        let llm_second = SequenceGateway::new(vec![
            LlmOutput {
                text: "second".to_string(),
                tool_calls: vec![ToolCall {
                    name: "counter".to_string(),
                    args: json!({ "value": "x", "_idempotency_key": "counter-key-1" }),
                }],
            },
            LlmOutput {
                text: "done".to_string(),
                tool_calls: Vec::new(),
            },
        ]);

        let mut runtime_second = AgentRuntime::default();
        let second = runtime_second.run_turn(
            Op::UserInput {
                submission_id: "sub-idempotency".to_string(),
                input: "second".to_string(),
            },
            &llm_second,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );
        assert_eq!(second.stop_reason, StopReason::Done);
        assert_eq!(second.stop_facts.stop_reason, StopReason::Done);
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "tool should not re-run for duplicate idempotency key",
        );
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
        assert_eq!(result.stop_facts.stop_reason, StopReason::Done);
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
        assert_eq!(result.stop_facts.stop_reason, StopReason::Error);
        assert_eq!(result.stop_facts.tool_error_count, 1);

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
                reason: StopReason::Error,
                ..
            }
        ));
        if let EventKind::TurnStopped { facts, .. } = &last_event.kind {
            let facts = facts
                .as_ref()
                .expect("turn_stopped should include standardized stop facts");
            assert_eq!(facts.stop_reason, StopReason::Error);
            assert_eq!(facts.tool_error_count, 1);
        } else {
            panic!("expected turn_stopped event");
        }
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
        assert_eq!(result.stop_facts.stop_reason, StopReason::BudgetExceeded);
        assert_eq!(
            result.stop_facts.budget_exceeded,
            Some(BudgetExceededFact {
                budget: BudgetKind::ToolCalls,
                limit: 1,
                observed: 2
            })
        );
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
        assert_eq!(result.stop_facts.stop_reason, StopReason::BudgetExceeded);
        assert_eq!(
            result.stop_facts.budget_exceeded,
            Some(BudgetExceededFact {
                budget: BudgetKind::Events,
                limit: 2,
                observed: 3
            })
        );
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
        assert_eq!(result.stop_facts.stop_reason, StopReason::BudgetExceeded);
        assert_eq!(
            result.stop_facts.budget_exceeded,
            Some(BudgetExceededFact {
                budget: BudgetKind::Tokens,
                limit: 2,
                observed: 3
            })
        );
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
