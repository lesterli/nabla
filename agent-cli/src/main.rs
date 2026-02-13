mod extensions;

use std::path::PathBuf;

use agent_core::{
    memory::{EventStore, InMemoryEventStore},
    memory_file::FileEventStore,
    policy::AllowAllPolicy,
    protocol::{Event, Op, StopReason},
    runtime::{AgentRuntime, LlmGateway},
    tools::{EchoTool, ToolRegistry},
};
use agent_llm::{
    MultiProviderGateway, OpenAiCompatibleProvider, OpenAiFunctionTool, OpenAiToolChoice,
    StaticProvider,
};
use extensions::{
    host::ExtensionHost,
    types::{NextAction, TurnContext},
};
use serde_json::json;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_SUBMISSION_ID: &str = "cli-session-1";

#[derive(Debug, Clone, PartialEq, Eq)]
enum CliCommand {
    Help,
    Version,
    Run {
        submission_id: String,
        prompt: String,
        store_file: Option<PathBuf>,
    },
    Resume {
        submission_id: String,
        checkpoint_id: Option<String>,
        store_file: PathBuf,
    },
    Approve {
        submission_id: String,
        request_id: String,
        approved: bool,
        reason: Option<String>,
        store_file: PathBuf,
    },
    Replay {
        submission_id: String,
        store_file: PathBuf,
    },
}

#[derive(Debug, Clone)]
struct CommandExecution {
    events: Vec<Event>,
    exit_code: i32,
    diagnostics: Vec<String>,
}

fn print_usage(program: &str) {
    println!(
        "Usage:
  {program} [OPTIONS] <prompt>
  {program} run --store-file <path> [--submission-id <id>] <prompt>
  {program} resume --store-file <path> [--submission-id <id>] [--checkpoint-id <id>]
  {program} approve --store-file <path> [--submission-id <id>] --request-id <id> --approved <true|false> [--reason <text>]
  {program} replay --store-file <path> [--submission-id <id>]

Options:
  -h, --help     Show this help message
  -V, --version  Show version

Notes:
  - The shorthand `{program} <prompt>` is kept for backward compatibility.
  - Lifecycle subcommands (`run/resume/approve/replay`) require `--store-file` for persistence.

LLM env:
  AGENT_LLM_PROVIDER=mock|openai|openai_compatible (default: mock)
  AGENT_LLM_BASE_URL (default: https://api.openai.com/v1)
  AGENT_LLM_API_KEY (required for openai/openai_compatible)
  AGENT_LLM_MODEL (default: gpt-4o-mini)
  AGENT_LLM_NAME (optional provider display name)
  AGENT_LLM_TOOLS (optional comma list; supported: echo)
  AGENT_LLM_TOOL_CHOICE (optional: auto|required|none|echo|function:<name>)"
    );
}

fn parse_tools_env(raw: &str) -> Result<Vec<&str>, String> {
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|tool| match tool {
            "echo" => Ok("echo"),
            other => Err(format!(
                "unsupported tool `{other}` in AGENT_LLM_TOOLS (supported: echo)"
            )),
        })
        .collect()
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
        "echo" => Ok(OpenAiToolChoice::Function {
            name: "echo".to_string(),
        }),
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
                "unsupported AGENT_LLM_TOOL_CHOICE value `{normalized}` (expected: auto, required, none, echo, function:<name>)"
            ))
        }
    }
}

fn echo_tool_definition() -> OpenAiFunctionTool {
    OpenAiFunctionTool::new(
        "echo",
        "Echo input text for connectivity checks.",
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string" }
            },
            "required": ["text"],
            "additionalProperties": false
        }),
    )
}

fn build_gateway_from_env() -> Result<MultiProviderGateway, String> {
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
            let tools_env = std::env::var("AGENT_LLM_TOOLS").unwrap_or_default();
            for tool_name in parse_tools_env(&tools_env)? {
                if tool_name == "echo" {
                    provider = provider.with_tool(echo_tool_definition());
                }
            }

            if let Ok(raw_choice) = std::env::var("AGENT_LLM_TOOL_CHOICE") {
                let parsed_choice = parse_tool_choice_env(&raw_choice)?;
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
        flag if flag.starts_with('-') => Err(format!("unsupported option `{flag}`")),
        _ => {
            let prompt = args.join(" ");
            if prompt.trim().is_empty() {
                return Err("prompt cannot be empty".to_string());
            }
            Ok(CliCommand::Run {
                submission_id: DEFAULT_SUBMISSION_ID.to_string(),
                prompt,
                store_file: None,
            })
        }
    }
}

fn parse_run_subcommand(args: &[String]) -> Result<CliCommand, String> {
    let mut submission_id = DEFAULT_SUBMISSION_ID.to_string();
    let mut store_file: Option<PathBuf> = None;
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
    })
}

fn parse_resume_subcommand(args: &[String]) -> Result<CliCommand, String> {
    let mut submission_id = DEFAULT_SUBMISSION_ID.to_string();
    let mut checkpoint_id: Option<String> = None;
    let mut store_file: Option<PathBuf> = None;

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

fn parse_approve_subcommand(args: &[String]) -> Result<CliCommand, String> {
    let mut submission_id = DEFAULT_SUBMISSION_ID.to_string();
    let mut request_id: Option<String> = None;
    let mut approved: Option<bool> = None;
    let mut reason: Option<String> = None;
    let mut store_file: Option<PathBuf> = None;

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
    })
}

fn build_tools() -> ToolRegistry {
    let mut tools = ToolRegistry::default();
    tools.register(EchoTool);
    tools
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
    let mut default_host = ExtensionHost::default();
    let host = match extension_host {
        Some(host) => host,
        None => &mut default_host,
    };

    match command {
        CliCommand::Run {
            submission_id,
            prompt,
            store_file,
        } => {
            let llm = llm.ok_or_else(|| "run requires an LLM gateway".to_string())?;
            execute_run(submission_id, prompt, store_file, llm, host)
        }
        CliCommand::Resume {
            submission_id,
            checkpoint_id,
            store_file,
        } => {
            let llm = llm.ok_or_else(|| "resume requires an LLM gateway".to_string())?;
            execute_resume(submission_id, checkpoint_id, store_file, llm, host)
        }
        CliCommand::Approve {
            submission_id,
            request_id,
            approved,
            reason,
            store_file,
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
            )
        }
        CliCommand::Replay {
            submission_id,
            store_file,
        } => execute_replay(submission_id, store_file),
        CliCommand::Help | CliCommand::Version => Err("cannot execute help/version".to_string()),
    }
}

fn execute_run(
    submission_id: String,
    prompt: String,
    store_file: Option<PathBuf>,
    llm: &dyn LlmGateway,
    extension_host: &mut ExtensionHost,
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
) -> Result<CommandExecution, String> {
    let mut runtime = AgentRuntime::default();
    let policy = AllowAllPolicy;
    let tools = build_tools();

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
    )
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

    match parsed {
        CliCommand::Help => {
            print_usage(&program);
            return;
        }
        CliCommand::Version => {
            println!("{VERSION}");
            return;
        }
        CliCommand::Run { .. } | CliCommand::Resume { .. } | CliCommand::Approve { .. } => {
            let llm = match build_gateway_from_env() {
                Ok(gateway) => gateway,
                Err(err) => {
                    eprintln!("LLM configuration error: {err}");
                    std::process::exit(2);
                }
            };

            let execution = match execute_parsed_command(parsed, Some(&llm)) {
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
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        CliCommand, execute_parsed_command, execute_parsed_command_with_extensions,
        parse_cli_command, parse_tool_choice_env, parse_tools_env,
    };
    use crate::extensions::{
        host::ExtensionHost,
        types::{CliExtension, NextAction, TurnContext},
    };
    use agent_core::{
        memory::EventStore,
        memory_file::FileEventStore,
        protocol::{Event, EventKind, StopFacts, StopReason, ToolCall},
    };
    use agent_llm::{MultiProviderGateway, OpenAiToolChoice, StaticProvider};
    use serde_json::json;

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
                        name: "echo".to_string(),
                        args: json!({ "text": "approved" }),
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
                    if result.call_name == "echo" && !result.is_error
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
        if let Some(parent) = store_path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    #[test]
    fn approve_false_records_denial_without_tool_execution() {
        let store_path = temp_store_path("approve-false");
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
                        name: "echo".to_string(),
                        args: json!({ "text": "should-not-run" }),
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
        if let Some(parent) = store_path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    #[test]
    fn parses_supported_tools_list() {
        let tools = parse_tools_env(" echo ").expect("parse tools");
        assert_eq!(tools, vec!["echo"]);
    }

    #[test]
    fn rejects_unsupported_tool() {
        let err = parse_tools_env("shell").expect_err("should reject");
        assert!(err.contains("unsupported tool"));
    }

    #[test]
    fn parses_function_tool_choice() {
        let choice = parse_tool_choice_env("function:echo").expect("parse tool choice");
        match choice {
            OpenAiToolChoice::Function { name } => assert_eq!(name, "echo"),
            _ => panic!("expected function tool choice"),
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
}
