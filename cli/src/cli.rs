use std::path::PathBuf;

use crate::tooling::ToolingCliConfig;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_SUBMISSION_ID: &str = "cli-session-1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
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
    pub fn tooling_config(&self) -> Option<&ToolingCliConfig> {
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

pub fn print_usage(program: &str) {
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
  --tools <list>   Enable specific built-in tools (supported: read,write,edit,bash,grep,find,ls)
  --no-tools       Disable all built-in tools (if combined with --tools, only listed tools are enabled)

Extension env (optional):
  AGENT_EXTENSION_MAX_FOLLOW_UP_TURNS (default: 4)
  AGENT_EXTENSION_CONTINUE_ONCE (inject one continue input)
  AGENT_EXTENSION_ASK_HUMAN_MESSAGE_ONCE (inject one ask-human message)
  AGENT_EXTENSION_STOP_ONCE=true|false (inject one stop action)"
    );
}

pub fn parse_cli_command(args: &[String]) -> Result<CliCommand, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn parses_shorthand_prompt() {
        let command = parse_cli_command(&args(&["hello", "world"])).expect("parse");
        assert_eq!(
            command,
            CliCommand::Run {
                submission_id: DEFAULT_SUBMISSION_ID.to_string(),
                prompt: "hello world".to_string(),
                store_file: None,
                tooling: ToolingCliConfig::default(),
            }
        );
    }

    #[test]
    fn parses_run_with_store_file() {
        let command = parse_cli_command(&args(&[
            "run",
            "--store-file",
            "/tmp/test.jsonl",
            "do something",
        ]))
        .expect("parse");
        match command {
            CliCommand::Run {
                submission_id,
                prompt,
                store_file,
                ..
            } => {
                assert_eq!(submission_id, DEFAULT_SUBMISSION_ID);
                assert_eq!(prompt, "do something");
                assert_eq!(
                    store_file,
                    Some(std::path::PathBuf::from("/tmp/test.jsonl"))
                );
            }
            _ => panic!("expected CliCommand::Run"),
        }
    }

    #[test]
    fn rejects_empty_args() {
        assert!(parse_cli_command(&[]).is_err());
    }

    #[test]
    fn shorthand_run_with_tools() {
        let command = parse_cli_command(&args(&["--tools", "read,write", "hello"])).expect("parse");
        match command {
            CliCommand::Run { tooling, .. } => {
                assert_eq!(
                    tooling.tools,
                    Some(vec!["read".to_string(), "write".to_string()])
                );
            }
            _ => panic!("expected CliCommand::Run"),
        }
    }

    #[test]
    fn shorthand_run_with_no_tools() {
        let command = parse_cli_command(&args(&["--no-tools", "hello"])).expect("parse");
        match command {
            CliCommand::Run { tooling, .. } => {
                assert!(tooling.no_tools);
            }
            _ => panic!("expected CliCommand::Run"),
        }
    }
}
