use std::path::PathBuf;

use nabla::{
    memory::{EventStore, InMemoryEventStore},
    memory_file::FileEventStore,
    policy::AllowAllPolicy,
    protocol::{Event, Op, StopReason},
    runtime::{AgentRuntime, LlmGateway},
    tools::ToolRegistry,
};
use nabla_llm::{MultiProviderGateway, OpenAiToolChoice, StaticProvider};

use crate::{
    cli::CliCommand,
    extensions::{
        host::ExtensionHost,
        types::{CliExtension, NextAction, TurnContext},
    },
    tooling::{ToolingSelection, resolve_tooling_from_cli},
};

#[derive(Debug, Clone)]
pub struct CommandExecution {
    pub events: Vec<Event>,
    pub exit_code: i32,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct EnvSingleActionExtension {
    name: &'static str,
    priority: i32,
    action: NextAction,
    used: bool,
}

impl EnvSingleActionExtension {
    pub fn continue_once(input: String, priority: i32) -> Self {
        Self {
            name: "env-continue-once",
            priority,
            action: NextAction::Continue { input },
            used: false,
        }
    }

    pub fn ask_human_message_once(message: String, priority: i32) -> Self {
        Self {
            name: "env-ask-human-message-once",
            priority,
            action: NextAction::AskHumanMessage { message },
            used: false,
        }
    }

    pub fn stop_once(priority: i32) -> Self {
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

pub fn parse_tool_choice_env(raw: &str) -> Result<OpenAiToolChoice, String> {
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

pub fn build_gateway_from_env(
    tooling: &ToolingSelection,
) -> Result<MultiProviderGateway, String> {
    let provider = std::env::var("AGENT_LLM_PROVIDER").unwrap_or_else(|_| "mock".to_string());

    match provider.as_str() {
        "mock" => Ok(
            MultiProviderGateway::new()
                .with_provider(StaticProvider::new("mock", "assistant> ")),
        ),
        "openai" | "openai_compatible" => {
            let base_url = std::env::var("AGENT_LLM_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
            let api_key = std::env::var("AGENT_LLM_API_KEY")
                .map_err(|_| "AGENT_LLM_API_KEY is required for real providers".to_string())?;
            let model =
                std::env::var("AGENT_LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
            let name = std::env::var("AGENT_LLM_NAME")
                .unwrap_or_else(|_| "openai-compatible".to_string());

            let mut provider =
                nabla_llm::OpenAiCompatibleProvider::new(name, base_url, api_key, model)?;
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

pub fn build_extension_host_from_env() -> Result<ExtensionHost, String> {
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

pub fn execute_parsed_command(
    command: CliCommand,
    llm: Option<&dyn LlmGateway>,
) -> Result<CommandExecution, String> {
    execute_parsed_command_with_extensions(command, llm, None)
}

pub fn execute_parsed_command_with_extensions(
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

pub fn execute_parsed_command_with_extensions_and_tooling(
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
            execute_run(
                submission_id,
                prompt,
                store_file,
                llm,
                host,
                tooling,
            )
        }
        CliCommand::Resume {
            submission_id,
            checkpoint_id,
            store_file,
            tooling: _,
        } => {
            let llm = llm.ok_or_else(|| "resume requires an LLM gateway".to_string())?;
            execute_resume(
                submission_id,
                checkpoint_id,
                store_file,
                llm,
                host,
                tooling,
            )
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

pub fn execute_run(
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
