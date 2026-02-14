mod extensions;
mod tooling;

use std::{fs, path::PathBuf, time::Instant};

use agent_core::{
    memory::{EventStore, InMemoryEventStore},
    memory_file::FileEventStore,
    policy::AllowAllPolicy,
    protocol::{Event, EventKind, Op, PROTOCOL_SCHEMA_VERSION, StopFacts, StopReason, ToolCall},
    runtime::{AgentRuntime, LlmGateway, LlmUsageSnapshot},
    tools::ToolRegistry,
};
use agent_llm::{MultiProviderGateway, OpenAiCompatibleProvider, OpenAiToolChoice, StaticProvider};
use extensions::{
    host::ExtensionHost,
    types::{CliExtension, NextAction, TurnContext},
};
use serde::{Deserialize, Serialize};
use tooling::{ToolingCliConfig, ToolingSelection, resolve_tooling_from_cli};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_SUBMISSION_ID: &str = "cli-session-1";
const EVAL_RESULT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
enum CliCommand {
    Help,
    Version,
    Run {
        submission_id: String,
        prompt: String,
        store_file: Option<PathBuf>,
        tooling: ToolingCliConfig,
    },
    Resume {
        submission_id: String,
        checkpoint_id: Option<String>,
        store_file: PathBuf,
        tooling: ToolingCliConfig,
    },
    Approve {
        submission_id: String,
        request_id: String,
        approved: bool,
        reason: Option<String>,
        store_file: PathBuf,
        tooling: ToolingCliConfig,
    },
    Replay {
        submission_id: String,
        store_file: PathBuf,
    },
    EvalSingle {
        task_id: String,
        submission_id: String,
        prompt: String,
        store_file: Option<PathBuf>,
        tooling: ToolingCliConfig,
    },
    EvalBatch {
        tasks_file: PathBuf,
        store_dir: Option<PathBuf>,
        tooling: ToolingCliConfig,
    },
}

impl CliCommand {
    fn tooling_config(&self) -> Option<&ToolingCliConfig> {
        match self {
            CliCommand::Run { tooling, .. }
            | CliCommand::Resume { tooling, .. }
            | CliCommand::Approve { tooling, .. }
            | CliCommand::EvalSingle { tooling, .. }
            | CliCommand::EvalBatch { tooling, .. } => Some(tooling),
            CliCommand::Help | CliCommand::Version | CliCommand::Replay { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
struct CommandExecution {
    events: Vec<Event>,
    exit_code: i32,
    diagnostics: Vec<String>,
}

#[derive(Debug, Clone)]
struct EnvSingleActionExtension {
    name: &'static str,
    priority: i32,
    action: NextAction,
    used: bool,
}

impl EnvSingleActionExtension {
    fn continue_once(input: String, priority: i32) -> Self {
        Self {
            name: "env-continue-once",
            priority,
            action: NextAction::Continue { input },
            used: false,
        }
    }

    fn ask_human_message_once(message: String, priority: i32) -> Self {
        Self {
            name: "env-ask-human-message-once",
            priority,
            action: NextAction::AskHumanMessage { message },
            used: false,
        }
    }

    fn stop_once(priority: i32) -> Self {
        Self {
            name: "env-stop-once",
            priority,
            action: NextAction::Stop,
            used: false,
        }
    }
}

impl CliExtension for EnvSingleActionExtension {
    fn name(&self) -> &'static str {
        self.name
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    fn propose_next_action(
        &mut self,
        _context: &TurnContext,
    ) -> Result<Option<NextAction>, String> {
        if self.used {
            return Ok(None);
        }
        self.used = true;
        Ok(Some(self.action.clone()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct EvalTokenUsage {
    estimated_input_tokens: u64,
    estimated_output_tokens: u64,
    estimation_method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct EvalRunMetadata {
    submission_id: String,
    provider: String,
    model: String,
    seed: Option<String>,
    protocol_schema_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct EvalResult {
    schema_version: u32,
    task_id: String,
    outcome_or_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stop_facts: Option<StopFacts>,
    tool_calls: Vec<ToolCall>,
    steps: u64,
    latency_ms: u64,
    token_usage: EvalTokenUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error_type: Option<String>,
    version: String,
    submission_id: String,
    run_metadata: EvalRunMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct EvalResultSet {
    schema_version: u32,
    version: String,
    results: Vec<EvalResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct EvalTaskInput {
    task_id: String,
    prompt: String,
    #[serde(default)]
    submission_id: Option<String>,
}

fn print_usage(program: &str) {
    println!(
        "Usage:
  {program} [OPTIONS] <prompt>
  {program} run --store-file <path> [--submission-id <id>] <prompt>
  {program} resume --store-file <path> [--submission-id <id>] [--checkpoint-id <id>]
  {program} approve --store-file <path> [--submission-id <id>] --request-id <id> --approved <true|false> [--reason <text>]
  {program} replay --store-file <path> [--submission-id <id>]
  {program} eval --task-id <id> [--submission-id <id>] [--store-file <path>] <prompt>
  {program} eval --tasks-file <path> [--store-dir <dir>]

Options:
  -h, --help     Show this help message
  -V, --version  Show version

Notes:
  - The shorthand `{program} <prompt>` is kept for backward compatibility.
  - Lifecycle subcommands (`run/resume/approve/replay`) require `--store-file` for persistence.
  - `eval` supports single-task and batch-task execution with machine-readable JSON output.

LLM env:
  AGENT_LLM_PROVIDER=mock|openai|openai_compatible (default: mock)
  AGENT_LLM_BASE_URL (default: https://api.openai.com/v1)
  AGENT_LLM_API_KEY (required for openai/openai_compatible)
  AGENT_LLM_MODEL (default: gpt-4o-mini)
  AGENT_LLM_NAME (optional provider display name)
  AGENT_LLM_TOOL_CHOICE (optional: auto|required|none|function:<name>)

Tool options:
  --tools <list>   Enable specific built-in tools (supported: read,write,edit,bash)
  --no-tools       Disable all built-in tools (if combined with --tools, only listed tools are enabled)

Extension env (optional):
  AGENT_EXTENSION_MAX_FOLLOW_UP_TURNS (default: 4)
  AGENT_EXTENSION_CONTINUE_ONCE (inject one continue input)
  AGENT_EXTENSION_ASK_HUMAN_MESSAGE_ONCE (inject one ask-human message)
  AGENT_EXTENSION_STOP_ONCE=true|false (inject one stop action)"
    );
}

fn parse_tool_choice_env(raw: &str) -> Result<OpenAiToolChoice, String> {
    let normalized = raw.trim();
    if normalized.is_empty() {
        return Err("AGENT_LLM_TOOL_CHOICE cannot be empty".to_string());
    }

    match normalized {
        "auto" => Ok(OpenAiToolChoice::Auto),
        "required" => Ok(OpenAiToolChoice::Required),
        "none" => Ok(OpenAiToolChoice::None),
        _ => {
            if let Some(name) = normalized.strip_prefix("function:") {
                if name.trim().is_empty() {
                    return Err(
                        "AGENT_LLM_TOOL_CHOICE function:<name> requires non-empty <name>"
                            .to_string(),
                    );
                }
                return Ok(OpenAiToolChoice::Function {
                    name: name.trim().to_string(),
                });
            }
            Err(format!(
                "unsupported AGENT_LLM_TOOL_CHOICE value `{normalized}` (expected: auto, required, none, function:<name>)"
            ))
        }
    }
}

fn validate_tool_choice(
    choice: &OpenAiToolChoice,
    tooling: &ToolingSelection,
) -> Result<(), String> {
    match choice {
        OpenAiToolChoice::Function { name } => {
            if tooling.contains_tool(name) {
                Ok(())
            } else {
                Err(format!(
                    "AGENT_LLM_TOOL_CHOICE selects tool `{name}`, but enabled tools are: {}",
                    tooling.enabled_tool_names().join(", ")
                ))
            }
        }
        _ => Ok(()),
    }
}

fn build_gateway_from_env(tooling: &ToolingSelection) -> Result<MultiProviderGateway, String> {
    let provider = std::env::var("AGENT_LLM_PROVIDER").unwrap_or_else(|_| "mock".to_string());

    match provider.as_str() {
        "mock" => {
            Ok(MultiProviderGateway::new()
                .with_provider(StaticProvider::new("mock", "assistant> ")))
        }
        "openai" | "openai_compatible" => {
            let base_url = std::env::var("AGENT_LLM_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
            let api_key = std::env::var("AGENT_LLM_API_KEY")
                .map_err(|_| "AGENT_LLM_API_KEY is required for real providers".to_string())?;
            let model =
                std::env::var("AGENT_LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
            let name =
                std::env::var("AGENT_LLM_NAME").unwrap_or_else(|_| "openai-compatible".to_string());

            let mut provider = OpenAiCompatibleProvider::new(name, base_url, api_key, model)?;
            for tool in tooling.provider_tools() {
                provider = provider.with_tool(tool);
            }

            if let Ok(raw_choice) = std::env::var("AGENT_LLM_TOOL_CHOICE") {
                let parsed_choice = parse_tool_choice_env(&raw_choice)?;
                validate_tool_choice(&parsed_choice, tooling)?;
                provider = provider.with_tool_choice(parsed_choice);
            }

            Ok(MultiProviderGateway::new().with_provider(provider))
        }
        other => Err(format!(
            "unsupported AGENT_LLM_PROVIDER value `{other}` (expected: mock, openai, openai_compatible)"
        )),
    }
}

fn parse_cli_command(args: &[String]) -> Result<CliCommand, String> {
    if args.is_empty() {
        return Err("missing command or prompt".to_string());
    }

    if args.len() == 1 {
        match args[0].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "-V" | "--version" => return Ok(CliCommand::Version),
            _ => {}
        }
    }

    match args[0].as_str() {
        "run" => parse_run_subcommand(&args[1..]),
        "resume" => parse_resume_subcommand(&args[1..]),
        "approve" => parse_approve_subcommand(&args[1..]),
        "replay" => parse_replay_subcommand(&args[1..]),
        "eval" => parse_eval_subcommand(&args[1..]),
        _ => parse_shorthand_run(args),
    }
}

fn parse_tools_list_arg(raw: &str, context: &str) -> Result<Vec<String>, String> {
    let parsed = raw
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if parsed.is_empty() {
        return Err(format!(
            "{context} `--tools` requires a non-empty comma-separated list"
        ));
    }
    Ok(parsed)
}

fn parse_shorthand_run(args: &[String]) -> Result<CliCommand, String> {
    let mut tooling = ToolingCliConfig::default();
    let mut prompt_start: Option<usize> = None;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--no-tools" => {
                tooling.no_tools = true;
            }
            "--tools" => {
                i += 1;
                if i >= args.len() {
                    return Err("`--tools` requires a value".to_string());
                }
                tooling.tools = Some(parse_tools_list_arg(&args[i], "shorthand run")?);
            }
            flag if flag.starts_with('-') => return Err(format!("unsupported option `{flag}`")),
            _ => {
                prompt_start = Some(i);
                break;
            }
        }
        i += 1;
    }

    let Some(prompt_start) = prompt_start else {
        return Err("prompt cannot be empty".to_string());
    };

    let prompt = args[prompt_start..].join(" ");
    if prompt.trim().is_empty() {
        return Err("prompt cannot be empty".to_string());
    }
    Ok(CliCommand::Run {
        submission_id: DEFAULT_SUBMISSION_ID.to_string(),
        prompt,
        store_file: None,
        tooling,
    })
}

fn parse_run_subcommand(args: &[String]) -> Result<CliCommand, String> {
    let mut submission_id = DEFAULT_SUBMISSION_ID.to_string();
    let mut store_file: Option<PathBuf> = None;
    let mut tooling = ToolingCliConfig::default();
    let mut prompt_start: Option<usize> = None;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--submission-id" => {
                i += 1;
                if i >= args.len() {
                    return Err("`run --submission-id` requires a value".to_string());
                }
                submission_id = args[i].clone();
            }
            "--store-file" => {
                i += 1;
                if i >= args.len() {
                    return Err("`run --store-file` requires a value".to_string());
                }
                store_file = Some(PathBuf::from(&args[i]));
            }
            "--no-tools" => {
                tooling.no_tools = true;
            }
            "--tools" => {
                i += 1;
                if i >= args.len() {
                    return Err("`run --tools` requires a value".to_string());
                }
                tooling.tools = Some(parse_tools_list_arg(&args[i], "`run`")?);
            }
            unknown if unknown.starts_with("--") => {
                return Err(format!("unknown `run` option `{unknown}`"));
            }
            _ => {
                prompt_start = Some(i);
                break;
            }
        }
        i += 1;
    }

    let Some(prompt_start) = prompt_start else {
        return Err("`run` requires a prompt".to_string());
    };

    let prompt = args[prompt_start..].join(" ");
    if prompt.trim().is_empty() {
        return Err("`run` prompt cannot be empty".to_string());
    }

    if store_file.is_none() {
        return Err("`run` requires --store-file <path>".to_string());
    }

    Ok(CliCommand::Run {
        submission_id,
        prompt,
        store_file,
        tooling,
    })
}

fn parse_resume_subcommand(args: &[String]) -> Result<CliCommand, String> {
    let mut submission_id = DEFAULT_SUBMISSION_ID.to_string();
    let mut checkpoint_id: Option<String> = None;
    let mut store_file: Option<PathBuf> = None;
    let mut tooling = ToolingCliConfig::default();

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--submission-id" => {
                i += 1;
                if i >= args.len() {
                    return Err("`resume --submission-id` requires a value".to_string());
                }
                submission_id = args[i].clone();
            }
            "--checkpoint-id" => {
                i += 1;
                if i >= args.len() {
                    return Err("`resume --checkpoint-id` requires a value".to_string());
                }
                checkpoint_id = Some(args[i].clone());
            }
            "--store-file" => {
                i += 1;
                if i >= args.len() {
                    return Err("`resume --store-file` requires a value".to_string());
                }
                store_file = Some(PathBuf::from(&args[i]));
            }
            "--no-tools" => {
                tooling.no_tools = true;
            }
            "--tools" => {
                i += 1;
                if i >= args.len() {
                    return Err("`resume --tools` requires a value".to_string());
                }
                tooling.tools = Some(parse_tools_list_arg(&args[i], "`resume`")?);
            }
            unknown if unknown.starts_with("--") => {
                return Err(format!("unknown `resume` option `{unknown}`"));
            }
            value => return Err(format!("unexpected argument for `resume`: `{value}`")),
        }
        i += 1;
    }

    let Some(store_file) = store_file else {
        return Err("`resume` requires --store-file <path>".to_string());
    };

    Ok(CliCommand::Resume {
        submission_id,
        checkpoint_id,
        store_file,
        tooling,
    })
}

fn parse_replay_subcommand(args: &[String]) -> Result<CliCommand, String> {
    let mut submission_id = DEFAULT_SUBMISSION_ID.to_string();
    let mut store_file: Option<PathBuf> = None;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--submission-id" => {
                i += 1;
                if i >= args.len() {
                    return Err("`replay --submission-id` requires a value".to_string());
                }
                submission_id = args[i].clone();
            }
            "--store-file" => {
                i += 1;
                if i >= args.len() {
                    return Err("`replay --store-file` requires a value".to_string());
                }
                store_file = Some(PathBuf::from(&args[i]));
            }
            unknown if unknown.starts_with("--") => {
                return Err(format!("unknown `replay` option `{unknown}`"));
            }
            value => return Err(format!("unexpected argument for `replay`: `{value}`")),
        }
        i += 1;
    }

    let Some(store_file) = store_file else {
        return Err("`replay` requires --store-file <path>".to_string());
    };

    Ok(CliCommand::Replay {
        submission_id,
        store_file,
    })
}

fn parse_eval_subcommand(args: &[String]) -> Result<CliCommand, String> {
    let mut task_id: Option<String> = None;
    let mut submission_id: Option<String> = None;
    let mut tasks_file: Option<PathBuf> = None;
    let mut store_file: Option<PathBuf> = None;
    let mut store_dir: Option<PathBuf> = None;
    let mut tooling = ToolingCliConfig::default();
    let mut prompt_start: Option<usize> = None;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--task-id" => {
                i += 1;
                if i >= args.len() {
                    return Err("`eval --task-id` requires a value".to_string());
                }
                task_id = Some(args[i].clone());
            }
            "--submission-id" => {
                i += 1;
                if i >= args.len() {
                    return Err("`eval --submission-id` requires a value".to_string());
                }
                submission_id = Some(args[i].clone());
            }
            "--tasks-file" => {
                i += 1;
                if i >= args.len() {
                    return Err("`eval --tasks-file` requires a value".to_string());
                }
                tasks_file = Some(PathBuf::from(&args[i]));
            }
            "--store-file" => {
                i += 1;
                if i >= args.len() {
                    return Err("`eval --store-file` requires a value".to_string());
                }
                store_file = Some(PathBuf::from(&args[i]));
            }
            "--store-dir" => {
                i += 1;
                if i >= args.len() {
                    return Err("`eval --store-dir` requires a value".to_string());
                }
                store_dir = Some(PathBuf::from(&args[i]));
            }
            "--no-tools" => {
                tooling.no_tools = true;
            }
            "--tools" => {
                i += 1;
                if i >= args.len() {
                    return Err("`eval --tools` requires a value".to_string());
                }
                tooling.tools = Some(parse_tools_list_arg(&args[i], "`eval`")?);
            }
            unknown if unknown.starts_with("--") => {
                return Err(format!("unknown `eval` option `{unknown}`"));
            }
            _ => {
                prompt_start = Some(i);
                break;
            }
        }
        i += 1;
    }

    if let Some(tasks_file) = tasks_file {
        if prompt_start.is_some() {
            return Err("`eval --tasks-file` does not accept a positional prompt".to_string());
        }
        if task_id.is_some() {
            return Err("`eval --tasks-file` cannot be combined with `--task-id`".to_string());
        }
        if store_file.is_some() {
            return Err("`eval --tasks-file` cannot be combined with `--store-file`".to_string());
        }
        if submission_id.is_some() {
            return Err(
                "`eval --tasks-file` cannot be combined with `--submission-id`".to_string(),
            );
        }
        return Ok(CliCommand::EvalBatch {
            tasks_file,
            store_dir,
            tooling,
        });
    }

    let Some(task_id) = task_id else {
        return Err("`eval` requires `--task-id <id>` or `--tasks-file <path>`".to_string());
    };
    let Some(prompt_start) = prompt_start else {
        return Err("`eval` single task requires a prompt".to_string());
    };

    let prompt = args[prompt_start..].join(" ");
    if prompt.trim().is_empty() {
        return Err("`eval` prompt cannot be empty".to_string());
    }
    if store_dir.is_some() {
        return Err("`eval` single task cannot use `--store-dir`".to_string());
    }

    let submission_id = submission_id.unwrap_or_else(|| format!("eval-{task_id}"));
    Ok(CliCommand::EvalSingle {
        task_id,
        submission_id,
        prompt,
        store_file,
        tooling,
    })
}

fn parse_approve_subcommand(args: &[String]) -> Result<CliCommand, String> {
    let mut submission_id = DEFAULT_SUBMISSION_ID.to_string();
    let mut request_id: Option<String> = None;
    let mut approved: Option<bool> = None;
    let mut reason: Option<String> = None;
    let mut store_file: Option<PathBuf> = None;
    let mut tooling = ToolingCliConfig::default();

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--submission-id" => {
                i += 1;
                if i >= args.len() {
                    return Err("`approve --submission-id` requires a value".to_string());
                }
                submission_id = args[i].clone();
            }
            "--request-id" => {
                i += 1;
                if i >= args.len() {
                    return Err("`approve --request-id` requires a value".to_string());
                }
                request_id = Some(args[i].clone());
            }
            "--approved" => {
                i += 1;
                if i >= args.len() {
                    return Err("`approve --approved` requires a value".to_string());
                }
                approved = Some(match args[i].as_str() {
                    "true" => true,
                    "false" => false,
                    other => {
                        return Err(format!(
                            "`approve --approved` expects `true` or `false`, got `{other}`"
                        ));
                    }
                });
            }
            "--reason" => {
                i += 1;
                if i >= args.len() {
                    return Err("`approve --reason` requires a value".to_string());
                }
                reason = Some(args[i].clone());
            }
            "--store-file" => {
                i += 1;
                if i >= args.len() {
                    return Err("`approve --store-file` requires a value".to_string());
                }
                store_file = Some(PathBuf::from(&args[i]));
            }
            "--no-tools" => {
                tooling.no_tools = true;
            }
            "--tools" => {
                i += 1;
                if i >= args.len() {
                    return Err("`approve --tools` requires a value".to_string());
                }
                tooling.tools = Some(parse_tools_list_arg(&args[i], "`approve`")?);
            }
            unknown if unknown.starts_with("--") => {
                return Err(format!("unknown `approve` option `{unknown}`"));
            }
            value => return Err(format!("unexpected argument for `approve`: `{value}`")),
        }
        i += 1;
    }

    let Some(request_id) = request_id else {
        return Err("`approve` requires --request-id <id>".to_string());
    };
    let Some(approved) = approved else {
        return Err("`approve` requires --approved <true|false>".to_string());
    };
    let Some(store_file) = store_file else {
        return Err("`approve` requires --store-file <path>".to_string());
    };

    Ok(CliCommand::Approve {
        submission_id,
        request_id,
        approved,
        reason,
        store_file,
        tooling,
    })
}

fn build_tools(tooling: &ToolingSelection) -> ToolRegistry {
    let mut tools = ToolRegistry::default();
    tooling.register_local_tools(&mut tools);
    tools
}

fn parse_bool_env(var: &str, raw: &str) -> Result<bool, String> {
    match raw.trim() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        other => Err(format!(
            "{var} must be one of: true,false,1,0,yes,no (got `{other}`)"
        )),
    }
}

fn build_extension_host_from_env() -> Result<ExtensionHost, String> {
    let mut host = ExtensionHost::default();

    if let Ok(raw_max_follow_up_turns) = std::env::var("AGENT_EXTENSION_MAX_FOLLOW_UP_TURNS") {
        let parsed = raw_max_follow_up_turns
            .trim()
            .parse::<usize>()
            .map_err(|err| format!("AGENT_EXTENSION_MAX_FOLLOW_UP_TURNS must be usize: {err}"))?;
        host.set_max_follow_up_turns(parsed);
    }

    if let Ok(input) = std::env::var("AGENT_EXTENSION_CONTINUE_ONCE") {
        let trimmed = input.trim();
        if !trimmed.is_empty() {
            host.add_extension(Box::new(EnvSingleActionExtension::continue_once(
                trimmed.to_string(),
                100,
            )));
        }
    }

    if let Ok(message) = std::env::var("AGENT_EXTENSION_ASK_HUMAN_MESSAGE_ONCE") {
        let trimmed = message.trim();
        if !trimmed.is_empty() {
            host.add_extension(Box::new(EnvSingleActionExtension::ask_human_message_once(
                trimmed.to_string(),
                90,
            )));
        }
    }

    if let Ok(raw_stop_once) = std::env::var("AGENT_EXTENSION_STOP_ONCE") {
        let should_stop = parse_bool_env("AGENT_EXTENSION_STOP_ONCE", &raw_stop_once)?;
        if should_stop {
            host.add_extension(Box::new(EnvSingleActionExtension::stop_once(80)));
        }
    }

    Ok(host)
}

fn execute_parsed_command(
    command: CliCommand,
    llm: Option<&dyn LlmGateway>,
) -> Result<CommandExecution, String> {
    execute_parsed_command_with_extensions(command, llm, None)
}

fn execute_parsed_command_with_extensions(
    command: CliCommand,
    llm: Option<&dyn LlmGateway>,
    extension_host: Option<&mut ExtensionHost>,
) -> Result<CommandExecution, String> {
    let tooling = match &command {
        CliCommand::Run { tooling, .. }
        | CliCommand::Resume { tooling, .. }
        | CliCommand::Approve { tooling, .. }
        | CliCommand::EvalSingle { tooling, .. }
        | CliCommand::EvalBatch { tooling, .. } => resolve_tooling_from_cli(tooling)?,
        CliCommand::Help | CliCommand::Version | CliCommand::Replay { .. } => {
            ToolingSelection::empty()
        }
    };
    execute_parsed_command_with_extensions_and_tooling(command, llm, extension_host, &tooling)
}

fn execute_parsed_command_with_extensions_and_tooling(
    command: CliCommand,
    llm: Option<&dyn LlmGateway>,
    extension_host: Option<&mut ExtensionHost>,
    tooling: &ToolingSelection,
) -> Result<CommandExecution, String> {
    let mut default_host = build_extension_host_from_env()?;
    let host = match extension_host {
        Some(host) => host,
        None => &mut default_host,
    };

    match command {
        CliCommand::Run {
            submission_id,
            prompt,
            store_file,
            tooling: _,
        } => {
            let llm = llm.ok_or_else(|| "run requires an LLM gateway".to_string())?;
            execute_run(submission_id, prompt, store_file, llm, host, tooling)
        }
        CliCommand::Resume {
            submission_id,
            checkpoint_id,
            store_file,
            tooling: _,
        } => {
            let llm = llm.ok_or_else(|| "resume requires an LLM gateway".to_string())?;
            execute_resume(submission_id, checkpoint_id, store_file, llm, host, tooling)
        }
        CliCommand::Approve {
            submission_id,
            request_id,
            approved,
            reason,
            store_file,
            tooling: _,
        } => {
            let llm = llm.ok_or_else(|| "approve requires an LLM gateway".to_string())?;
            execute_approve(
                submission_id,
                request_id,
                approved,
                reason,
                store_file,
                llm,
                host,
                tooling,
            )
        }
        CliCommand::Replay {
            submission_id,
            store_file,
        } => execute_replay(submission_id, store_file),
        CliCommand::EvalSingle { .. } | CliCommand::EvalBatch { .. } => {
            Err("use eval execution path for `eval` subcommand".to_string())
        }
        CliCommand::Help | CliCommand::Version => Err("cannot execute help/version".to_string()),
    }
}

fn execute_run(
    submission_id: String,
    prompt: String,
    store_file: Option<PathBuf>,
    llm: &dyn LlmGateway,
    extension_host: &mut ExtensionHost,
    tooling: &ToolingSelection,
) -> Result<CommandExecution, String> {
    match store_file {
        Some(path) => {
            let mut store = FileEventStore::open(&path)
                .map_err(|err| format!("failed to open store file `{}`: {err}", path.display()))?;
            execute_turn_with_extensions(
                Op::UserInput {
                    submission_id,
                    input: prompt,
                },
                llm,
                &mut store,
                extension_host,
                true,
                tooling,
            )
        }
        None => {
            let mut store = InMemoryEventStore::default();
            execute_turn_with_extensions(
                Op::UserInput {
                    submission_id,
                    input: prompt,
                },
                llm,
                &mut store,
                extension_host,
                true,
                tooling,
            )
        }
    }
}

fn execute_turn_with_extensions(
    initial_op: Op,
    llm: &dyn LlmGateway,
    store: &mut dyn EventStore,
    extension_host: &mut ExtensionHost,
    strict_done_exit: bool,
    tooling: &ToolingSelection,
) -> Result<CommandExecution, String> {
    let mut runtime = AgentRuntime::default();
    let policy = AllowAllPolicy;
    let tools = build_tools(tooling);

    let mut current_op = initial_op;
    let mut all_events = Vec::new();
    let mut follow_up_turns_used = 0usize;

    let final_stop_reason = loop {
        let current_submission_id = current_op.submission_id().to_string();
        let turn = runtime.run_turn(current_op, llm, &policy, &tools, store);
        let turn_stop_reason = turn.stop_reason.clone();

        let turn_events = turn.events.clone();
        all_events.extend(turn_events.clone());

        let context = TurnContext {
            submission_id: current_submission_id.clone(),
            stop_reason: turn.stop_reason,
            stop_facts: turn.stop_facts,
            events: turn_events,
        };

        let next_action = extension_host.process_turn(&context);
        let Some(next_action) = next_action else {
            break turn_stop_reason;
        };

        match next_action {
            NextAction::Stop => break turn_stop_reason,
            NextAction::Continue { input } => {
                if follow_up_turns_used >= extension_host.max_follow_up_turns() {
                    extension_host.record_diagnostic(format!(
                        "extension follow-up budget exceeded: max_follow_up_turns={}",
                        extension_host.max_follow_up_turns()
                    ));
                    break turn_stop_reason;
                }
                follow_up_turns_used = follow_up_turns_used.saturating_add(1);
                current_op = Op::UserInput {
                    submission_id: current_submission_id,
                    input,
                };
            }
            NextAction::AskHumanMessage { message } => {
                if follow_up_turns_used >= extension_host.max_follow_up_turns() {
                    extension_host.record_diagnostic(format!(
                        "extension follow-up budget exceeded: max_follow_up_turns={}",
                        extension_host.max_follow_up_turns()
                    ));
                    break turn_stop_reason;
                }
                follow_up_turns_used = follow_up_turns_used.saturating_add(1);
                current_op = Op::UserInput {
                    submission_id: current_submission_id,
                    input: message,
                };
            }
        }
    };
    let exit_code = if strict_done_exit {
        if final_stop_reason == StopReason::Done {
            0
        } else {
            1
        }
    } else {
        match final_stop_reason {
            StopReason::Error | StopReason::PolicyDenied | StopReason::BudgetExceeded => 1,
            _ => 0,
        }
    };

    Ok(CommandExecution {
        events: all_events,
        exit_code,
        diagnostics: extension_host.diagnostics().to_vec(),
    })
}

fn execute_resume(
    submission_id: String,
    checkpoint_id: Option<String>,
    store_file: PathBuf,
    llm: &dyn LlmGateway,
    extension_host: &mut ExtensionHost,
    tooling: &ToolingSelection,
) -> Result<CommandExecution, String> {
    let mut store = FileEventStore::open(&store_file).map_err(|err| {
        format!(
            "failed to open store file `{}`: {err}",
            store_file.display()
        )
    })?;

    execute_turn_with_extensions(
        Op::Resume {
            submission_id,
            checkpoint_id,
        },
        llm,
        &mut store,
        extension_host,
        false,
        tooling,
    )
}

fn execute_replay(submission_id: String, store_file: PathBuf) -> Result<CommandExecution, String> {
    let store = FileEventStore::open(&store_file).map_err(|err| {
        format!(
            "failed to open store file `{}`: {err}",
            store_file.display()
        )
    })?;

    Ok(CommandExecution {
        events: store.events_for_submission(&submission_id),
        exit_code: 0,
        diagnostics: Vec::new(),
    })
}

fn execute_approve(
    submission_id: String,
    request_id: String,
    approved: bool,
    reason: Option<String>,
    store_file: PathBuf,
    llm: &dyn LlmGateway,
    extension_host: &mut ExtensionHost,
    tooling: &ToolingSelection,
) -> Result<CommandExecution, String> {
    let mut store = FileEventStore::open(&store_file).map_err(|err| {
        format!(
            "failed to open store file `{}`: {err}",
            store_file.display()
        )
    })?;

    execute_turn_with_extensions(
        Op::HumanApprovalResponse {
            submission_id,
            request_id,
            approved,
            reason,
        },
        llm,
        &mut store,
        extension_host,
        false,
        tooling,
    )
}

fn execute_eval_command(
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
            let result =
                execute_eval_task(task_id, submission_id, prompt, store_file, llm, tooling);
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
        ),
        Err(err) => EvalResult {
            schema_version: EVAL_RESULT_SCHEMA_VERSION,
            task_id,
            outcome_or_status: "error".to_string(),
            stop_facts: None,
            tool_calls: Vec::new(),
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
    let mut error_type = derive_error_type(&stop_reason, stop_facts.as_ref(), &events);
    if error_type.is_none() && !diagnostics.is_empty() {
        error_type = Some("extension_diagnostic".to_string());
    }

    EvalResult {
        schema_version: EVAL_RESULT_SCHEMA_VERSION,
        task_id,
        outcome_or_status: stop_reason_to_status(&stop_reason).to_string(),
        stop_facts,
        tool_calls,
        steps,
        latency_ms,
        token_usage: token_usage_for_eval(usage_delta, heuristic_usage),
        error_type,
        version: VERSION.to_string(),
        submission_id,
        run_metadata,
    }
}

fn diff_usage_snapshots(
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

fn token_usage_for_eval(
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

fn load_eval_tasks(path: &PathBuf) -> Result<Vec<EvalTaskInput>, String> {
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

fn sanitize_for_filename(input: &str) -> String {
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

fn main() {
    let mut args = std::env::args();
    let program = args.next().unwrap_or_else(|| "agent-cli".to_string());
    let rest: Vec<String> = args.collect();

    let parsed = match parse_cli_command(&rest) {
        Ok(command) => command,
        Err(err) => {
            eprintln!("Argument error: {err}");
            print_usage(&program);
            std::process::exit(2);
        }
    };

    let resolved_tooling = if let Some(tooling_config) = parsed.tooling_config() {
        Some(match resolve_tooling_from_cli(tooling_config) {
            Ok(tooling) => tooling,
            Err(err) => {
                eprintln!("Tooling configuration error: {err}");
                std::process::exit(2);
            }
        })
    } else {
        None
    };

    match parsed {
        CliCommand::Help => {
            print_usage(&program);
            return;
        }
        CliCommand::Version => {
            println!("{VERSION}");
            return;
        }
        command @ (CliCommand::Run { .. }
        | CliCommand::Resume { .. }
        | CliCommand::Approve { .. }) => {
            let tooling = resolved_tooling
                .as_ref()
                .expect("tooling resolved for llm commands");

            let llm = match build_gateway_from_env(&tooling) {
                Ok(gateway) => gateway,
                Err(err) => {
                    eprintln!("LLM configuration error: {err}");
                    std::process::exit(2);
                }
            };

            let execution = match execute_parsed_command_with_extensions_and_tooling(
                command,
                Some(&llm),
                None,
                tooling,
            ) {
                Ok(result) => result,
                Err(err) => {
                    eprintln!("Command failed: {err}");
                    std::process::exit(2);
                }
            };

            for event in execution.events {
                let line = serde_json::to_string(&event).expect("serialize event");
                println!("{line}");
            }
            for diagnostic in execution.diagnostics {
                eprintln!("Extension diagnostic: {diagnostic}");
            }

            if execution.exit_code != 0 {
                std::process::exit(execution.exit_code);
            }
        }
        command @ (CliCommand::EvalSingle { .. } | CliCommand::EvalBatch { .. }) => {
            let tooling = resolved_tooling
                .as_ref()
                .expect("tooling resolved for eval commands");

            let llm = match build_gateway_from_env(&tooling) {
                Ok(gateway) => gateway,
                Err(err) => {
                    eprintln!("LLM configuration error: {err}");
                    std::process::exit(2);
                }
            };

            let result_set = match execute_eval_command(command, &llm, tooling) {
                Ok(result_set) => result_set,
                Err(err) => {
                    eprintln!("Command failed: {err}");
                    std::process::exit(2);
                }
            };

            let line = serde_json::to_string(&result_set).expect("serialize eval result set");
            println!("{line}");
        }
        CliCommand::Replay { .. } => {
            let execution = match execute_parsed_command(parsed, None) {
                Ok(result) => result,
                Err(err) => {
                    eprintln!("Command failed: {err}");
                    std::process::exit(2);
                }
            };

            for event in execution.events {
                let line = serde_json::to_string(&event).expect("serialize event");
                println!("{line}");
            }
            for diagnostic in execution.diagnostics {
                eprintln!("Extension diagnostic: {diagnostic}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Write,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        CliCommand, EvalResult, EvalResultSet, EvalRunMetadata, EvalTokenUsage,
        diff_usage_snapshots, execute_eval_command, execute_parsed_command,
        execute_parsed_command_with_extensions, parse_cli_command, parse_tool_choice_env,
        token_usage_for_eval,
    };
    use crate::extensions::{
        host::ExtensionHost,
        types::{CliExtension, NextAction, TurnContext},
    };
    use crate::tooling::{ToolingCliConfig, resolve_tooling_from_cli};
    use agent_core::{
        memory::EventStore,
        memory_file::FileEventStore,
        protocol::{Event, EventKind, StopFacts, StopReason, ToolCall},
        runtime::LlmUsageSnapshot,
    };
    use agent_llm::{MultiProviderGateway, OpenAiToolChoice, StaticProvider};
    use serde_json::{Value, json};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn temp_store_path(test_name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        std::env::temp_dir()
            .join("agent-cli-tests")
            .join(format!("{test_name}-{nanos}-{}.jsonl", std::process::id()))
    }

    fn temp_workspace_file(test_name: &str, content: &str) -> (PathBuf, String) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let relative = PathBuf::from("target")
            .join("agent-cli-tests")
            .join(format!("{test_name}-{}-{nanos}.txt", std::process::id()));
        if let Some(parent) = relative.parent() {
            fs::create_dir_all(parent).expect("create workspace temp dir");
        }
        let mut file = fs::File::create(&relative).expect("create workspace temp file");
        file.write_all(content.as_bytes())
            .expect("write workspace temp file");
        (relative.clone(), relative.to_string_lossy().to_string())
    }

    #[test]
    fn routes_help_version_and_subcommands() {
        assert!(matches!(
            parse_cli_command(&args(&["--help"])).expect("parse help"),
            CliCommand::Help
        ));
        assert!(matches!(
            parse_cli_command(&args(&["--version"])).expect("parse version"),
            CliCommand::Version
        ));

        let run = parse_cli_command(&args(&[
            "run",
            "--submission-id",
            "s1",
            "--store-file",
            "/tmp/events.jsonl",
            "hello",
        ]))
        .expect("parse run");
        assert!(matches!(run, CliCommand::Run { .. }));

        let resume = parse_cli_command(&args(&[
            "resume",
            "--submission-id",
            "s1",
            "--store-file",
            "/tmp/events.jsonl",
        ]))
        .expect("parse resume");
        assert!(matches!(resume, CliCommand::Resume { .. }));

        let replay = parse_cli_command(&args(&[
            "replay",
            "--submission-id",
            "s1",
            "--store-file",
            "/tmp/events.jsonl",
        ]))
        .expect("parse replay");
        assert!(matches!(replay, CliCommand::Replay { .. }));

        let approve = parse_cli_command(&args(&[
            "approve",
            "--submission-id",
            "s1",
            "--request-id",
            "approval-1",
            "--approved",
            "true",
            "--store-file",
            "/tmp/events.jsonl",
        ]))
        .expect("parse approve");
        assert!(matches!(approve, CliCommand::Approve { .. }));

        let eval_single = parse_cli_command(&args(&[
            "eval",
            "--task-id",
            "task-1",
            "--submission-id",
            "eval-sub-1",
            "solve this task",
        ]))
        .expect("parse eval single");
        assert!(matches!(eval_single, CliCommand::EvalSingle { .. }));

        let eval_batch = parse_cli_command(&args(&[
            "eval",
            "--tasks-file",
            "/tmp/tasks.jsonl",
            "--store-dir",
            "/tmp/eval-stores",
        ]))
        .expect("parse eval batch");
        assert!(matches!(eval_batch, CliCommand::EvalBatch { .. }));
    }

    #[test]
    fn parses_cli_tool_options_for_llm_commands() {
        let run = parse_cli_command(&args(&[
            "run",
            "--store-file",
            "/tmp/events.jsonl",
            "--tools",
            "read,write,edit,bash",
            "hello",
        ]))
        .expect("parse run with tools");
        match run {
            CliCommand::Run { tooling, .. } => {
                assert_eq!(
                    tooling.tools,
                    Some(vec![
                        "read".to_string(),
                        "write".to_string(),
                        "edit".to_string(),
                        "bash".to_string()
                    ])
                );
                assert!(!tooling.no_tools);
            }
            _ => panic!("expected run command"),
        }

        let resume = parse_cli_command(&args(&[
            "resume",
            "--store-file",
            "/tmp/events.jsonl",
            "--no-tools",
        ]))
        .expect("parse resume with no tools");
        match resume {
            CliCommand::Resume { tooling, .. } => {
                assert!(tooling.no_tools);
                assert_eq!(tooling.tools, None);
            }
            _ => panic!("expected resume command"),
        }

        let shorthand = parse_cli_command(&args(&["--tools", "read", "hello"]))
            .expect("parse shorthand with tools");
        match shorthand {
            CliCommand::Run { tooling, .. } => {
                assert_eq!(tooling.tools, Some(vec!["read".to_string()]));
            }
            _ => panic!("expected run command"),
        }
    }

    #[test]
    fn run_then_resume_continues_same_submission_in_persistent_store() {
        let store_path = temp_store_path("run-resume");
        if let Some(parent) = store_path.parent() {
            fs::create_dir_all(parent).expect("create temp dir");
        }

        let llm =
            MultiProviderGateway::new().with_provider(StaticProvider::new("mock", "assistant> "));

        let store_path_str = store_path.to_string_lossy().to_string();
        let run_command = parse_cli_command(&args(&[
            "run",
            "--submission-id",
            "submission-e2e",
            "--store-file",
            &store_path_str,
            "hello",
            "world",
        ]))
        .expect("parse run");
        let run_result =
            execute_parsed_command(run_command, Some(&llm)).expect("execute run command");
        assert_eq!(run_result.exit_code, 0);

        let resume_command = parse_cli_command(&args(&[
            "resume",
            "--submission-id",
            "submission-e2e",
            "--store-file",
            &store_path_str,
        ]))
        .expect("parse resume");
        let resume_result =
            execute_parsed_command(resume_command, Some(&llm)).expect("execute resume command");
        assert_eq!(resume_result.exit_code, 0);

        let store = FileEventStore::open(&store_path).expect("open store for validation");
        let events = store.events_for_submission("submission-e2e");
        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, EventKind::UserInput { .. })),
            "expected user input event in persisted stream",
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, EventKind::TurnResumed { .. })),
            "expected turn resumed event in persisted stream",
        );
        assert!(
            events.windows(2).all(|pair| pair[1].index > pair[0].index),
            "event indices should remain strictly increasing",
        );

        fs::remove_file(&store_path).expect("cleanup store file");
        if let Some(parent) = store_path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    #[test]
    fn approve_true_resumes_pending_call_and_continues_loop() {
        let store_path = temp_store_path("approve-true");
        let (read_file, read_file_str) = temp_workspace_file("approve-true-read", "approved\n");
        if let Some(parent) = store_path.parent() {
            fs::create_dir_all(parent).expect("create temp dir");
        }

        {
            let mut store = FileEventStore::open(&store_path).expect("open seed store");
            store.append(Event::new(
                "submission-approve".to_string(),
                0,
                EventKind::UserInput {
                    input: "need approval".to_string(),
                },
            ));
            store.append(Event::new(
                "submission-approve".to_string(),
                1,
                EventKind::HumanApprovalRequested {
                    request_id: "approval-7".to_string(),
                    call: ToolCall {
                        name: "read".to_string(),
                        args: json!({ "path": read_file_str }),
                    },
                    reason: "needs human".to_string(),
                },
            ));
            store.append(Event::new(
                "submission-approve".to_string(),
                2,
                EventKind::TurnStopped {
                    reason: StopReason::HumanApprovalRequired,
                    facts: None,
                },
            ));
        }

        let llm =
            MultiProviderGateway::new().with_provider(StaticProvider::new("mock", "assistant> "));
        let store_path_str = store_path.to_string_lossy().to_string();
        let approve_command = parse_cli_command(&args(&[
            "approve",
            "--submission-id",
            "submission-approve",
            "--request-id",
            "approval-7",
            "--approved",
            "true",
            "--reason",
            "approved in cli test",
            "--store-file",
            &store_path_str,
        ]))
        .expect("parse approve");
        let approve_result =
            execute_parsed_command(approve_command, Some(&llm)).expect("execute approve");
        assert_eq!(approve_result.exit_code, 0);

        let store = FileEventStore::open(&store_path).expect("open store for assertions");
        let events = store.events_for_submission("submission-approve");
        assert!(events.iter().any(|event| {
            matches!(
                event.kind,
                EventKind::HumanApprovalResolved {
                    approved: true,
                    reason: Some(ref reason),
                    ..
                } if reason == "approved in cli test"
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event.kind,
                EventKind::ToolExecuted { ref result }
                    if result.call_name == "read" && !result.is_error
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event.kind,
                EventKind::TurnStopped {
                    reason: StopReason::Done,
                    ..
                }
            )
        }));

        fs::remove_file(&store_path).expect("cleanup store file");
        let _ = fs::remove_file(read_file);
        if let Some(parent) = store_path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    #[test]
    fn approve_false_records_denial_without_tool_execution() {
        let store_path = temp_store_path("approve-false");
        let (read_file, read_file_str) = temp_workspace_file("approve-false-read", "denied\n");
        if let Some(parent) = store_path.parent() {
            fs::create_dir_all(parent).expect("create temp dir");
        }

        {
            let mut store = FileEventStore::open(&store_path).expect("open seed store");
            store.append(Event::new(
                "submission-deny".to_string(),
                0,
                EventKind::UserInput {
                    input: "need approval".to_string(),
                },
            ));
            store.append(Event::new(
                "submission-deny".to_string(),
                1,
                EventKind::HumanApprovalRequested {
                    request_id: "approval-8".to_string(),
                    call: ToolCall {
                        name: "read".to_string(),
                        args: json!({ "path": read_file_str }),
                    },
                    reason: "needs human".to_string(),
                },
            ));
            store.append(Event::new(
                "submission-deny".to_string(),
                2,
                EventKind::TurnStopped {
                    reason: StopReason::HumanApprovalRequired,
                    facts: None,
                },
            ));
        }

        let llm =
            MultiProviderGateway::new().with_provider(StaticProvider::new("mock", "assistant> "));
        let store_path_str = store_path.to_string_lossy().to_string();
        let deny_command = parse_cli_command(&args(&[
            "approve",
            "--submission-id",
            "submission-deny",
            "--request-id",
            "approval-8",
            "--approved",
            "false",
            "--reason",
            "rejected in cli test",
            "--store-file",
            &store_path_str,
        ]))
        .expect("parse deny command");
        let deny_result =
            execute_parsed_command(deny_command, Some(&llm)).expect("execute deny command");
        assert_eq!(deny_result.exit_code, 1);

        let store = FileEventStore::open(&store_path).expect("open store for assertions");
        let events = store.events_for_submission("submission-deny");
        assert!(events.iter().any(|event| {
            matches!(
                event.kind,
                EventKind::HumanApprovalResolved {
                    approved: false,
                    reason: Some(ref reason),
                    ..
                } if reason == "rejected in cli test"
            )
        }));
        assert!(
            events
                .iter()
                .all(|event| !matches!(event.kind, EventKind::ToolExecuted { .. })),
            "tool execution must not happen after denial",
        );
        assert!(events.iter().any(|event| {
            matches!(
                event.kind,
                EventKind::TurnStopped {
                    reason: StopReason::PolicyDenied,
                    ..
                }
            )
        }));

        fs::remove_file(&store_path).expect("cleanup store file");
        let _ = fs::remove_file(read_file);
        if let Some(parent) = store_path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    #[test]
    fn resolves_default_tooling_with_read_enabled() {
        let selection = resolve_tooling_from_cli(&ToolingCliConfig::default()).expect("resolve");
        assert_eq!(selection.enabled_tool_names(), vec!["read"]);
        assert!(selection.contains_tool("read"));
    }

    #[test]
    fn rejects_unknown_tool_configuration() {
        let err = resolve_tooling_from_cli(&ToolingCliConfig {
            no_tools: false,
            tools: Some(vec!["missing".to_string()]),
        })
        .expect_err("must reject");
        assert!(err.contains("unsupported tool `missing`"));
    }

    #[test]
    fn provider_and_local_tool_assembly_stay_consistent() {
        let selection = resolve_tooling_from_cli(&ToolingCliConfig {
            no_tools: false,
            tools: Some(vec![
                "read".to_string(),
                "write".to_string(),
                "edit".to_string(),
                "bash".to_string(),
            ]),
        })
        .expect("resolve explicit tools");
        let provider_tools = selection
            .provider_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();

        assert_eq!(provider_tools, selection.enabled_tool_names());
    }

    #[test]
    fn parses_function_tool_choice() {
        let choice = parse_tool_choice_env("function:read").expect("parse tool choice");
        match choice {
            OpenAiToolChoice::Function { name } => assert_eq!(name, "read"),
            _ => panic!("expected function tool choice"),
        }
    }

    #[test]
    fn eval_result_schema_shape_is_stable() {
        let result_set = EvalResultSet {
            schema_version: 1,
            version: super::VERSION.to_string(),
            results: vec![EvalResult {
                schema_version: 1,
                task_id: "task-1".to_string(),
                outcome_or_status: "done".to_string(),
                stop_facts: None,
                tool_calls: vec![ToolCall {
                    name: "read".to_string(),
                    args: json!({"path":"Cargo.toml"}),
                }],
                steps: 1,
                latency_ms: 12,
                token_usage: EvalTokenUsage {
                    estimated_input_tokens: 2,
                    estimated_output_tokens: 3,
                    estimation_method: "heuristic_word_count".to_string(),
                },
                error_type: None,
                version: super::VERSION.to_string(),
                submission_id: "submission-1".to_string(),
                run_metadata: EvalRunMetadata {
                    submission_id: "submission-1".to_string(),
                    provider: "mock".to_string(),
                    model: "mock-static".to_string(),
                    seed: Some("42".to_string()),
                    protocol_schema_version: 1,
                },
            }],
        };

        let actual = serde_json::to_string_pretty(&result_set).expect("serialize eval result set");
        let expected = format!(
            r#"{{
  "schema_version": 1,
  "version": "{}",
  "results": [
    {{
      "schema_version": 1,
      "task_id": "task-1",
      "outcome_or_status": "done",
      "tool_calls": [
        {{
          "name": "read",
          "args": {{
            "path": "Cargo.toml"
          }}
        }}
      ],
      "steps": 1,
      "latency_ms": 12,
      "token_usage": {{
        "estimated_input_tokens": 2,
        "estimated_output_tokens": 3,
        "estimation_method": "heuristic_word_count"
      }},
      "version": "{}",
      "submission_id": "submission-1",
      "run_metadata": {{
        "submission_id": "submission-1",
        "provider": "mock",
        "model": "mock-static",
        "seed": "42",
        "protocol_schema_version": 1
      }}
    }}
  ]
}}"#,
            super::VERSION,
            super::VERSION
        );
        assert_eq!(actual, expected);
    }

    #[test]
    fn eval_batch_fixture_outputs_parseable_results() {
        let tasks_path = temp_store_path("eval-tasks");
        let store_dir = std::env::temp_dir()
            .join("agent-cli-tests")
            .join(format!("eval-store-dir-{}", std::process::id()));
        if let Some(parent) = tasks_path.parent() {
            fs::create_dir_all(parent).expect("create temp dir");
        }
        fs::create_dir_all(&store_dir).expect("create eval store dir");

        let tasks_content = r#"{"task_id":"task-a","prompt":"say hello once"}
{"task_id":"task-b","prompt":"call read tool with path Cargo.toml"}"#;
        fs::write(&tasks_path, tasks_content).expect("write tasks file");

        let llm =
            MultiProviderGateway::new().with_provider(StaticProvider::new("mock", "assistant> "));
        let tasks_path_str = tasks_path.to_string_lossy().to_string();
        let store_dir_str = store_dir.to_string_lossy().to_string();
        let eval_command = parse_cli_command(&args(&[
            "eval",
            "--tasks-file",
            &tasks_path_str,
            "--store-dir",
            &store_dir_str,
        ]))
        .expect("parse eval batch command");

        let tooling = resolve_tooling_from_cli(&ToolingCliConfig::default()).expect("resolve");
        let result_set =
            execute_eval_command(eval_command, &llm, &tooling).expect("execute eval batch");
        assert_eq!(result_set.schema_version, 1);
        assert_eq!(result_set.results.len(), 2);
        assert_eq!(result_set.results[0].task_id, "task-a");
        assert_eq!(result_set.results[1].task_id, "task-b");

        let encoded = serde_json::to_string(&result_set).expect("serialize result set");
        let decoded: Value = serde_json::from_str(&encoded).expect("decode as generic json");
        let results = decoded
            .get("results")
            .and_then(Value::as_array)
            .expect("results array");
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .all(|entry| entry.get("task_id").and_then(Value::as_str).is_some())
        );

        fs::remove_file(&tasks_path).expect("cleanup tasks file");
        let _ = fs::remove_dir_all(&store_dir);
        if let Some(parent) = tasks_path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    struct FollowUpOnceExtension {
        used: bool,
        input: String,
    }

    impl CliExtension for FollowUpOnceExtension {
        fn name(&self) -> &'static str {
            "follow-up-once"
        }

        fn priority(&self) -> i32 {
            10
        }

        fn propose_next_action(
            &mut self,
            context: &TurnContext,
        ) -> Result<Option<NextAction>, String> {
            let _ = (
                &context.submission_id,
                &context.stop_reason,
                &context.events,
            );
            if self.used {
                return Ok(None);
            }
            self.used = true;
            Ok(Some(NextAction::Continue {
                input: self.input.clone(),
            }))
        }
    }

    struct AskHumanMessageOnceExtension {
        used: bool,
        message: String,
    }

    impl CliExtension for AskHumanMessageOnceExtension {
        fn name(&self) -> &'static str {
            "ask-human-message-once"
        }

        fn priority(&self) -> i32 {
            5
        }

        fn propose_next_action(
            &mut self,
            _context: &TurnContext,
        ) -> Result<Option<NextAction>, String> {
            if self.used {
                return Ok(None);
            }
            self.used = true;
            Ok(Some(NextAction::AskHumanMessage {
                message: self.message.clone(),
            }))
        }
    }

    struct FailingExtension;

    impl CliExtension for FailingExtension {
        fn name(&self) -> &'static str {
            "failing-extension"
        }

        fn priority(&self) -> i32 {
            100
        }

        fn on_turn_end(&mut self, _context: &TurnContext) -> Result<(), String> {
            Err("intentional failure for test".to_string())
        }
    }

    struct StopExtension;

    impl CliExtension for StopExtension {
        fn name(&self) -> &'static str {
            "stop-extension"
        }

        fn priority(&self) -> i32 {
            1
        }

        fn propose_next_action(
            &mut self,
            _context: &TurnContext,
        ) -> Result<Option<NextAction>, String> {
            Ok(Some(NextAction::Stop))
        }
    }

    fn dummy_turn_context() -> TurnContext {
        TurnContext {
            submission_id: "sub-ext".to_string(),
            stop_reason: StopReason::Done,
            stop_facts: StopFacts {
                stop_reason: StopReason::Done,
                budget_exceeded: None,
                tool_error_count: 0,
                last_tool_calls: Vec::new(),
                has_pending_approval: false,
            },
            events: Vec::new(),
        }
    }

    #[test]
    fn extension_host_follow_up_injects_second_turn() {
        let store_path = temp_store_path("extension-follow-up");
        if let Some(parent) = store_path.parent() {
            fs::create_dir_all(parent).expect("create temp dir");
        }

        let llm =
            MultiProviderGateway::new().with_provider(StaticProvider::new("mock", "assistant> "));
        let store_path_str = store_path.to_string_lossy().to_string();
        let run_command = parse_cli_command(&args(&[
            "run",
            "--submission-id",
            "sub-ext-follow-up",
            "--store-file",
            &store_path_str,
            "first input",
        ]))
        .expect("parse run");

        let mut host = ExtensionHost::default();
        host.set_max_follow_up_turns(2);
        host.add_extension(Box::new(FollowUpOnceExtension {
            used: false,
            input: "second input from extension".to_string(),
        }));

        let execution =
            execute_parsed_command_with_extensions(run_command, Some(&llm), Some(&mut host))
                .expect("execute command with extensions");
        assert_eq!(execution.exit_code, 0);
        assert!(execution.diagnostics.is_empty());

        let user_inputs = execution
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::UserInput { input } => Some(input.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(user_inputs.len(), 2);
        assert_eq!(user_inputs[0], "first input");
        assert_eq!(user_inputs[1], "second input from extension");

        fs::remove_file(&store_path).expect("cleanup store file");
        if let Some(parent) = store_path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    #[test]
    fn extension_host_conflict_resolution_prefers_higher_priority() {
        let mut host = ExtensionHost::default();
        host.add_extension(Box::new(FollowUpOnceExtension {
            used: false,
            input: "high-priority".to_string(),
        }));
        host.add_extension(Box::new(StopExtension));

        let next = host.process_turn(&dummy_turn_context());
        assert_eq!(
            next,
            Some(NextAction::Continue {
                input: "high-priority".to_string()
            })
        );
        assert!(
            host.diagnostics()
                .iter()
                .any(|message| message.contains("extension action conflict")),
            "expected deterministic conflict diagnostic"
        );
    }

    #[test]
    fn extension_failure_is_isolated_and_reported() {
        let store_path = temp_store_path("extension-failure");
        if let Some(parent) = store_path.parent() {
            fs::create_dir_all(parent).expect("create temp dir");
        }

        let llm =
            MultiProviderGateway::new().with_provider(StaticProvider::new("mock", "assistant> "));
        let store_path_str = store_path.to_string_lossy().to_string();
        let run_command = parse_cli_command(&args(&[
            "run",
            "--submission-id",
            "sub-ext-failure",
            "--store-file",
            &store_path_str,
            "first input",
        ]))
        .expect("parse run");

        let mut host = ExtensionHost::default();
        host.add_extension(Box::new(FailingExtension));
        host.add_extension(Box::new(FollowUpOnceExtension {
            used: false,
            input: "still-runs".to_string(),
        }));

        let execution =
            execute_parsed_command_with_extensions(run_command, Some(&llm), Some(&mut host))
                .expect("execute command with failing extension");
        assert_eq!(execution.exit_code, 0);
        assert!(
            execution
                .diagnostics
                .iter()
                .any(|message| message.contains("failing-extension")),
            "failing extension should be visible in diagnostics"
        );

        let user_inputs = execution
            .events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::UserInput { .. }))
            .count();
        assert_eq!(
            user_inputs, 2,
            "follow-up extension should still run despite failure in another extension"
        );

        fs::remove_file(&store_path).expect("cleanup store file");
        if let Some(parent) = store_path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    #[test]
    fn extension_host_supports_ask_human_message_action() {
        let store_path = temp_store_path("extension-ask-human-message");
        if let Some(parent) = store_path.parent() {
            fs::create_dir_all(parent).expect("create temp dir");
        }

        let llm =
            MultiProviderGateway::new().with_provider(StaticProvider::new("mock", "assistant> "));
        let store_path_str = store_path.to_string_lossy().to_string();
        let run_command = parse_cli_command(&args(&[
            "run",
            "--submission-id",
            "sub-ext-ask-human",
            "--store-file",
            &store_path_str,
            "first input",
        ]))
        .expect("parse run");

        let mut host = ExtensionHost::default();
        host.add_extension(Box::new(AskHumanMessageOnceExtension {
            used: false,
            message: "human follow-up".to_string(),
        }));

        let execution =
            execute_parsed_command_with_extensions(run_command, Some(&llm), Some(&mut host))
                .expect("execute command with ask-human action");
        assert_eq!(execution.exit_code, 0);

        let user_inputs = execution
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::UserInput { input } => Some(input.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(user_inputs.len(), 2);
        assert_eq!(user_inputs[0], "first input");
        assert_eq!(user_inputs[1], "human follow-up");

        fs::remove_file(&store_path).expect("cleanup store file");
        if let Some(parent) = store_path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    #[test]
    fn usage_snapshot_delta_computes_saturating_differences() {
        let before = LlmUsageSnapshot {
            total_input_tokens: 10,
            total_output_tokens: 8,
            native_usage_calls: 1,
            heuristic_usage_calls: 2,
        };
        let after = LlmUsageSnapshot {
            total_input_tokens: 22,
            total_output_tokens: 17,
            native_usage_calls: 3,
            heuristic_usage_calls: 2,
        };

        let delta = diff_usage_snapshots(Some(before), Some(after)).expect("delta");
        assert_eq!(delta.total_input_tokens, 12);
        assert_eq!(delta.total_output_tokens, 9);
        assert_eq!(delta.native_usage_calls, 2);
        assert_eq!(delta.heuristic_usage_calls, 0);
    }

    #[test]
    fn token_usage_prefers_provider_native_when_available() {
        let usage = token_usage_for_eval(
            Some(LlmUsageSnapshot {
                total_input_tokens: 111,
                total_output_tokens: 37,
                native_usage_calls: 2,
                heuristic_usage_calls: 0,
            }),
            EvalTokenUsage {
                estimated_input_tokens: 1,
                estimated_output_tokens: 1,
                estimation_method: "heuristic_word_count".to_string(),
            },
        );

        assert_eq!(usage.estimated_input_tokens, 111);
        assert_eq!(usage.estimated_output_tokens, 37);
        assert_eq!(usage.estimation_method, "provider_native");
    }
}
