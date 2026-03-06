use std::{collections::BTreeMap, fs, path::PathBuf, time::Instant};

use nabla::{
    protocol::{Event, EventKind, StopFacts, StopReason, ToolCall, PROTOCOL_SCHEMA_VERSION},
    runtime::{LlmGateway, LlmUsageSnapshot},
};
use serde::{Deserialize, Serialize};

use crate::{
    cli::{CliCommand, VERSION},
    commands::execute_run,
    extensions::host::ExtensionHost,
    tooling::ToolingSelection,
};

pub const EVAL_RESULT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvalTokenUsage {
    pub estimated_input_tokens: u64,
    pub estimated_output_tokens: u64,
    pub estimation_method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EvalToolCallStats {
    pub total_proposed: u64,
    pub total_executed: u64,
    pub total_errors: u64,
    pub by_tool: Vec<EvalToolCallStat>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvalToolCallStat {
    pub name: String,
    pub proposed: u64,
    pub executed: u64,
    pub errors: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvalRunMetadata {
    pub submission_id: String,
    pub provider: String,
    pub model: String,
    pub seed: Option<String>,
    pub protocol_schema_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvalResult {
    pub schema_version: u32,
    pub task_id: String,
    pub outcome_or_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_facts: Option<StopFacts>,
    pub enabled_tools: Vec<String>,
    pub tool_calls: Vec<ToolCall>,
    pub tool_call_stats: EvalToolCallStats,
    pub steps: u64,
    pub latency_ms: u64,
    pub token_usage: EvalTokenUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    pub version: String,
    pub submission_id: String,
    pub run_metadata: EvalRunMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvalResultSet {
    pub schema_version: u32,
    pub version: String,
    pub results: Vec<EvalResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvalTaskInput {
    pub task_id: String,
    pub prompt: String,
    #[serde(default)]
    pub submission_id: Option<String>,
}

pub fn execute_eval_command(
    command: CliCommand,
    llm: &dyn LlmGateway,
    tooling: &ToolingSelection,
) -> Result<EvalResultSet, String> {
    match command {
        CliCommand::EvalSingle {
            task_id,
            submission_id,
            prompt,
            store_file,
            tooling: _,
        } => {
            let result = execute_eval_task(
                task_id,
                submission_id,
                prompt,
                store_file,
                llm,
                tooling,
            );
            Ok(EvalResultSet {
                schema_version: EVAL_RESULT_SCHEMA_VERSION,
                version: VERSION.to_string(),
                results: vec![result],
            })
        }
        CliCommand::EvalBatch {
            tasks_file,
            store_dir,
            tooling: _,
        } => {
            let tasks = load_eval_tasks(&tasks_file)?;
            if let Some(dir) = &store_dir {
                fs::create_dir_all(dir).map_err(|err| {
                    format!("failed to create eval store dir `{}`: {err}", dir.display())
                })?;
            }

            let mut results = Vec::with_capacity(tasks.len());
            for (index, task) in tasks.into_iter().enumerate() {
                let submission_id = task.submission_id.unwrap_or_else(|| {
                    format!(
                        "eval-{}-{}",
                        sanitize_for_filename(&task.task_id),
                        index.saturating_add(1)
                    )
                });
                let task_store_file = store_dir.as_ref().map(|dir| {
                    dir.join(format!("{}.jsonl", sanitize_for_filename(&submission_id)))
                });
                let result = execute_eval_task(
                    task.task_id,
                    submission_id,
                    task.prompt,
                    task_store_file,
                    llm,
                    tooling,
                );
                results.push(result);
            }

            Ok(EvalResultSet {
                schema_version: EVAL_RESULT_SCHEMA_VERSION,
                version: VERSION.to_string(),
                results,
            })
        }
        _ => Err("not an eval command".to_string()),
    }
}

fn execute_eval_task(
    task_id: String,
    submission_id: String,
    prompt: String,
    store_file: Option<PathBuf>,
    llm: &dyn LlmGateway,
    tooling: &ToolingSelection,
) -> EvalResult {
    let started = Instant::now();
    let run_metadata = build_eval_run_metadata(submission_id.clone());
    let enabled_tools = tooling
        .enabled_tool_names()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let usage_before = llm.usage_snapshot();
    let mut host = ExtensionHost::default();
    let execution = execute_run(
        submission_id.clone(),
        prompt.clone(),
        store_file,
        llm,
        &mut host,
        tooling,
    );
    let usage_after = llm.usage_snapshot();
    let usage_delta = diff_usage_snapshots(usage_before, usage_after);
    let latency_ms = started.elapsed().as_millis() as u64;

    match execution {
        Ok(execution) => build_eval_result_from_events(
            task_id,
            submission_id,
            prompt,
            execution.events,
            execution.diagnostics,
            latency_ms,
            run_metadata,
            usage_delta,
            enabled_tools,
        ),
        Err(err) => EvalResult {
            schema_version: EVAL_RESULT_SCHEMA_VERSION,
            task_id,
            outcome_or_status: "error".to_string(),
            stop_facts: None,
            enabled_tools,
            tool_calls: Vec::new(),
            tool_call_stats: EvalToolCallStats::default(),
            steps: 0,
            latency_ms,
            token_usage: token_usage_for_error(&prompt, usage_delta),
            error_type: Some(format!("command_error:{err}")),
            version: VERSION.to_string(),
            submission_id,
            run_metadata,
        },
    }
}

fn build_eval_result_from_events(
    task_id: String,
    submission_id: String,
    prompt: String,
    events: Vec<Event>,
    diagnostics: Vec<String>,
    latency_ms: u64,
    run_metadata: EvalRunMetadata,
    usage_delta: Option<LlmUsageSnapshot>,
    enabled_tools: Vec<String>,
) -> EvalResult {
    let steps = events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::ContextBuilt { .. }))
        .count() as u64;
    let tool_calls = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolCallProposed { call } => Some(call.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let output_tokens = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::LlmText { text } => Some(estimate_cli_tokens(text)),
            _ => None,
        })
        .sum::<u64>();

    let llm_turns = steps.max(1);
    let input_tokens = estimate_cli_tokens(&prompt).saturating_mul(llm_turns);
    let heuristic_usage = EvalTokenUsage {
        estimated_input_tokens: input_tokens,
        estimated_output_tokens: output_tokens,
        estimation_method: "heuristic_word_count".to_string(),
    };
    let (stop_reason, stop_facts) =
        final_stop_from_events(&events).unwrap_or((StopReason::Interrupted, None));
    let tool_call_stats = build_tool_call_stats(&events);
    let mut error_type = derive_error_type(&stop_reason, stop_facts.as_ref(), &events);
    if error_type.is_none() && !diagnostics.is_empty() {
        error_type = Some("extension_diagnostic".to_string());
    }

    EvalResult {
        schema_version: EVAL_RESULT_SCHEMA_VERSION,
        task_id,
        outcome_or_status: stop_reason_to_status(&stop_reason).to_string(),
        stop_facts,
        enabled_tools,
        tool_calls,
        tool_call_stats,
        steps,
        latency_ms,
        token_usage: token_usage_for_eval(usage_delta, heuristic_usage),
        error_type,
        version: VERSION.to_string(),
        submission_id,
        run_metadata,
    }
}

pub fn diff_usage_snapshots(
    before: Option<LlmUsageSnapshot>,
    after: Option<LlmUsageSnapshot>,
) -> Option<LlmUsageSnapshot> {
    let (Some(before), Some(after)) = (before, after) else {
        return None;
    };

    Some(LlmUsageSnapshot {
        total_input_tokens: after
            .total_input_tokens
            .saturating_sub(before.total_input_tokens),
        total_output_tokens: after
            .total_output_tokens
            .saturating_sub(before.total_output_tokens),
        native_usage_calls: after
            .native_usage_calls
            .saturating_sub(before.native_usage_calls),
        heuristic_usage_calls: after
            .heuristic_usage_calls
            .saturating_sub(before.heuristic_usage_calls),
    })
}

pub fn token_usage_for_eval(
    usage_delta: Option<LlmUsageSnapshot>,
    heuristic_usage: EvalTokenUsage,
) -> EvalTokenUsage {
    let Some(delta) = usage_delta else {
        return heuristic_usage;
    };

    let total_calls = delta
        .native_usage_calls
        .saturating_add(delta.heuristic_usage_calls);
    if total_calls == 0 {
        return heuristic_usage;
    }

    let estimation_method = if delta.native_usage_calls > 0 && delta.heuristic_usage_calls == 0 {
        "provider_native"
    } else if delta.native_usage_calls > 0 && delta.heuristic_usage_calls > 0 {
        "mixed_provider_native_and_heuristic"
    } else {
        "heuristic_word_count"
    };

    EvalTokenUsage {
        estimated_input_tokens: delta.total_input_tokens,
        estimated_output_tokens: delta.total_output_tokens,
        estimation_method: estimation_method.to_string(),
    }
}

fn token_usage_for_error(prompt: &str, usage_delta: Option<LlmUsageSnapshot>) -> EvalTokenUsage {
    let heuristic_usage = EvalTokenUsage {
        estimated_input_tokens: estimate_cli_tokens(prompt),
        estimated_output_tokens: 0,
        estimation_method: "heuristic_word_count".to_string(),
    };
    token_usage_for_eval(usage_delta, heuristic_usage)
}

fn build_tool_call_stats(events: &[Event]) -> EvalToolCallStats {
    #[derive(Default)]
    struct Acc {
        proposed: u64,
        executed: u64,
        errors: u64,
    }

    let mut by_tool: BTreeMap<String, Acc> = BTreeMap::new();
    let mut total_proposed = 0u64;
    let mut total_executed = 0u64;
    let mut total_errors = 0u64;

    for event in events {
        match &event.kind {
            EventKind::ToolCallProposed { call } => {
                total_proposed = total_proposed.saturating_add(1);
                let entry = by_tool.entry(call.name.clone()).or_default();
                entry.proposed = entry.proposed.saturating_add(1);
            }
            EventKind::ToolExecuted { result } => {
                total_executed = total_executed.saturating_add(1);
                let entry = by_tool.entry(result.call_name.clone()).or_default();
                entry.executed = entry.executed.saturating_add(1);
                if result.is_error {
                    total_errors = total_errors.saturating_add(1);
                    entry.errors = entry.errors.saturating_add(1);
                }
            }
            _ => {}
        }
    }

    EvalToolCallStats {
        total_proposed,
        total_executed,
        total_errors,
        by_tool: by_tool
            .into_iter()
            .map(|(name, acc)| EvalToolCallStat {
                name,
                proposed: acc.proposed,
                executed: acc.executed,
                errors: acc.errors,
            })
            .collect(),
    }
}

fn final_stop_from_events(events: &[Event]) -> Option<(StopReason, Option<StopFacts>)> {
    events.iter().rev().find_map(|event| match &event.kind {
        EventKind::TurnStopped { reason, facts } => Some((reason.clone(), facts.clone())),
        _ => None,
    })
}

fn derive_error_type(
    stop_reason: &StopReason,
    stop_facts: Option<&StopFacts>,
    events: &[Event],
) -> Option<String> {
    match stop_reason {
        StopReason::Done => None,
        StopReason::Error => {
            if events
                .iter()
                .any(|event| matches!(event.kind, EventKind::LlmError { .. }))
            {
                Some("llm_error".to_string())
            } else if stop_facts
                .map(|facts| facts.tool_error_count > 0)
                .unwrap_or(false)
            {
                Some("tool_error".to_string())
            } else {
                Some("runtime_error".to_string())
            }
        }
        StopReason::BudgetExceeded => Some("budget_exceeded".to_string()),
        StopReason::PolicyDenied => Some("policy_denied".to_string()),
        StopReason::HumanApprovalRequired => Some("human_approval_required".to_string()),
        StopReason::Interrupted => Some("interrupted".to_string()),
    }
}

fn stop_reason_to_status(stop_reason: &StopReason) -> &'static str {
    match stop_reason {
        StopReason::Done => "done",
        StopReason::Interrupted => "interrupted",
        StopReason::Error => "error",
        StopReason::BudgetExceeded => "budget_exceeded",
        StopReason::PolicyDenied => "policy_denied",
        StopReason::HumanApprovalRequired => "human_approval_required",
    }
}

fn estimate_cli_tokens(text: &str) -> u64 {
    text.split_whitespace().count() as u64
}

fn build_eval_run_metadata(submission_id: String) -> EvalRunMetadata {
    let provider = std::env::var("AGENT_LLM_PROVIDER").unwrap_or_else(|_| "mock".to_string());
    let model = std::env::var("AGENT_LLM_MODEL").unwrap_or_else(|_| {
        if provider == "mock" {
            "mock-static".to_string()
        } else {
            "gpt-4o-mini".to_string()
        }
    });
    let seed = std::env::var("AGENT_EVAL_SEED")
        .ok()
        .filter(|value| !value.trim().is_empty());

    EvalRunMetadata {
        submission_id,
        provider,
        model,
        seed,
        protocol_schema_version: PROTOCOL_SCHEMA_VERSION,
    }
}

pub fn load_eval_tasks(path: &PathBuf) -> Result<Vec<EvalTaskInput>, String> {
    let raw = fs::read_to_string(path)
        .map_err(|err| format!("failed to read tasks file `{}`: {err}", path.display()))?;

    if let Ok(tasks) = serde_json::from_str::<Vec<EvalTaskInput>>(&raw) {
        return validate_eval_tasks(tasks, path);
    }

    let mut tasks = Vec::new();
    for (line_number, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let task = serde_json::from_str::<EvalTaskInput>(trimmed).map_err(|err| {
            format!(
                "invalid tasks file `{}` at line {}: {err}",
                path.display(),
                line_number + 1
            )
        })?;
        tasks.push(task);
    }
    validate_eval_tasks(tasks, path)
}

fn validate_eval_tasks(
    tasks: Vec<EvalTaskInput>,
    path: &PathBuf,
) -> Result<Vec<EvalTaskInput>, String> {
    if tasks.is_empty() {
        return Err(format!("tasks file `{}` contains no tasks", path.display()));
    }

    for task in &tasks {
        if task.task_id.trim().is_empty() {
            return Err(format!(
                "tasks file `{}` contains a task with empty `task_id`",
                path.display()
            ));
        }
        if task.prompt.trim().is_empty() {
            return Err(format!(
                "tasks file `{}` contains empty prompt for task `{}`",
                path.display(),
                task.task_id
            ));
        }
    }
    Ok(tasks)
}

pub fn sanitize_for_filename(input: &str) -> String {
    let mut sanitized = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();

    if sanitized.trim_matches('_').is_empty() {
        sanitized = "submission".to_string();
    }
    sanitized
}
