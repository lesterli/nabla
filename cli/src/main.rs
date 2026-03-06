mod cli;
mod commands;
mod eval;
mod extensions;
mod tooling;

use cli::{CliCommand, VERSION, parse_cli_command, print_usage};
use commands::{build_gateway_from_env, execute_parsed_command_with_extensions_and_tooling};
use eval::execute_eval_command;
use tooling::resolve_tooling_from_cli;

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

            let llm = match build_gateway_from_env(tooling) {
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

            let llm = match build_gateway_from_env(tooling) {
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
            let execution = match commands::execute_parsed_command(parsed, None) {
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

    use crate::cli::parse_cli_command;
    use crate::commands::{
        EnvSingleActionExtension, execute_parsed_command,
        execute_parsed_command_with_extensions,
    };
    use crate::eval::execute_eval_command;
    use crate::extensions::{
        host::ExtensionHost,
        types::{CliExtension, NextAction, TurnContext},
    };
    use crate::tooling::{ToolingCliConfig, resolve_tooling_from_cli};
    use nabla::{
        memory::EventStore,
        memory_file::FileEventStore,
        protocol::{Event, EventKind, StopReason, ToolCall},
    };
    use nabla_llm::{MultiProviderGateway, StaticProvider};
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

    fn json_schema_shape(value: &Value) -> Value {
        match value {
            Value::Null => json!("null"),
            Value::Bool(_) => json!("bool"),
            Value::Number(_) => json!("number"),
            Value::String(_) => json!("string"),
            Value::Array(items) => Value::Array(items.iter().map(json_schema_shape).collect()),
            Value::Object(map) => {
                let mut entries = map.iter().collect::<Vec<_>>();
                entries.sort_by(|(left, _), (right, _)| left.cmp(right));
                let mut shaped = serde_json::Map::new();
                for (key, value) in entries {
                    shaped.insert(key.clone(), json_schema_shape(value));
                }
                Value::Object(shaped)
            }
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
    fn extension_system_handles_continue_stop_ask_and_budget() {
        let llm =
            MultiProviderGateway::new().with_provider(StaticProvider::new("mock", "assistant> "));

        let user_inputs_from = |result: &crate::commands::CommandExecution| -> Vec<String> {
            result
                .events
                .iter()
                .filter_map(|event| match &event.kind {
                    EventKind::UserInput { input } => Some(input.clone()),
                    _ => None,
                })
                .collect()
        };

        // continue_once triggers a follow-up turn
        {
            let command = parse_cli_command(&args(&[
                "run",
                "--store-file",
                &temp_store_path("ext-continue").to_string_lossy(),
                "hello",
            ]))
            .expect("parse");
            let mut host = ExtensionHost::default();
            host.add_extension(Box::new(EnvSingleActionExtension::continue_once(
                "follow-up".to_string(),
                100,
            )));
            let result =
                execute_parsed_command_with_extensions(command, Some(&llm), Some(&mut host))
                    .expect("continue");
            let inputs = user_inputs_from(&result);
            assert!(inputs.len() >= 2);
            assert!(inputs.iter().any(|i| i == "follow-up"));
        }

        // stop_once suppresses lower-priority continue
        {
            let command = parse_cli_command(&args(&[
                "run",
                "--store-file",
                &temp_store_path("ext-stop").to_string_lossy(),
                "hello",
            ]))
            .expect("parse");
            let mut host = ExtensionHost::default();
            host.add_extension(Box::new(EnvSingleActionExtension::stop_once(80)));
            host.add_extension(Box::new(EnvSingleActionExtension::continue_once(
                "should-not-appear".to_string(),
                70,
            )));
            let result =
                execute_parsed_command_with_extensions(command, Some(&llm), Some(&mut host))
                    .expect("stop");
            assert_eq!(user_inputs_from(&result), vec!["hello"]);
        }

        // ask_human injects a message as follow-up
        {
            let command = parse_cli_command(&args(&[
                "run",
                "--store-file",
                &temp_store_path("ext-ask").to_string_lossy(),
                "hello",
            ]))
            .expect("parse");
            let mut host = ExtensionHost::default();
            host.add_extension(Box::new(EnvSingleActionExtension::ask_human_message_once(
                "human message".to_string(),
                90,
            )));
            let result =
                execute_parsed_command_with_extensions(command, Some(&llm), Some(&mut host))
                    .expect("ask");
            let inputs = user_inputs_from(&result);
            assert!(inputs.len() >= 2);
            assert!(inputs.iter().any(|i| i == "human message"));
        }

        // budget limits infinite follow-ups
        {
            let command = parse_cli_command(&args(&[
                "run",
                "--store-file",
                &temp_store_path("ext-budget").to_string_lossy(),
                "hello",
            ]))
            .expect("parse");

            struct InfiniteExtension;
            impl CliExtension for InfiniteExtension {
                fn name(&self) -> &'static str {
                    "infinite-continue"
                }
                fn priority(&self) -> i32 {
                    100
                }
                fn propose_next_action(
                    &mut self,
                    _context: &TurnContext,
                ) -> Result<Option<NextAction>, String> {
                    Ok(Some(NextAction::Continue {
                        input: "again".to_string(),
                    }))
                }
            }

            let mut host = ExtensionHost::default();
            host.set_max_follow_up_turns(2);
            host.add_extension(Box::new(InfiniteExtension));
            let result =
                execute_parsed_command_with_extensions(command, Some(&llm), Some(&mut host))
                    .expect("budget");
            assert!(user_inputs_from(&result).len() <= 3);
            assert!(result
                .diagnostics
                .iter()
                .any(|d| d.contains("follow-up budget exceeded")));
        }
    }

    #[test]
    fn eval_single_produces_expected_schema_shape() {
        let llm =
            MultiProviderGateway::new().with_provider(StaticProvider::new("mock", "eval done"));
        let command = parse_cli_command(&args(&[
            "eval",
            "--task-id",
            "task-schema-test",
            "summarize the code",
        ]))
        .expect("parse eval single");

        let tooling = resolve_tooling_from_cli(&ToolingCliConfig::default()).expect("resolve");
        let result_set = execute_eval_command(command, &llm, &tooling).expect("eval run");

        assert_eq!(result_set.schema_version, 1);
        assert_eq!(result_set.results.len(), 1);

        let result = &result_set.results[0];
        assert_eq!(result.task_id, "task-schema-test");
        assert!(result.outcome_or_status == "done" || result.outcome_or_status == "error");
        assert!(result.steps >= 1 || result.error_type.is_some());

        let json_value = serde_json::to_value(result).expect("serialize eval result");
        let shape = json_schema_shape(&json_value);
        assert_eq!(shape["schema_version"], json!("number"));
        assert_eq!(shape["task_id"], json!("string"));
        assert_eq!(shape["outcome_or_status"], json!("string"));
        assert_eq!(shape["steps"], json!("number"));
        assert_eq!(shape["latency_ms"], json!("number"));
        assert_eq!(shape["version"], json!("string"));

        // run_metadata and tool_call_stats presence (merged from eval_result_json_includes_run_metadata_and_tool_stats)
        assert!(!result.run_metadata.provider.is_empty());
        assert!(!result.run_metadata.model.is_empty());
        assert_eq!(result.run_metadata.protocol_schema_version, 1);
        assert!(json_value.get("run_metadata").is_some());
        assert!(json_value.get("tool_call_stats").is_some());
        assert!(json_value["tool_call_stats"].get("total_proposed").is_some());
        assert!(json_value["tool_call_stats"].get("by_tool").is_some());
    }

    #[test]
    fn eval_batch_runs_multiple_tasks() {
        let tasks_dir = std::env::temp_dir()
            .join("agent-cli-tests")
            .join("eval-batch");
        fs::create_dir_all(&tasks_dir).expect("create tasks dir");
        let tasks_file = tasks_dir.join("tasks.json");
        fs::write(
            &tasks_file,
            r#"[
                {"task_id": "t1", "prompt": "task one"},
                {"task_id": "t2", "prompt": "task two"}
            ]"#,
        )
        .expect("write tasks file");

        let llm =
            MultiProviderGateway::new().with_provider(StaticProvider::new("mock", "batch done"));
        let command = parse_cli_command(&args(&[
            "eval",
            "--tasks-file",
            &tasks_file.to_string_lossy(),
        ]))
        .expect("parse eval batch");

        let tooling = resolve_tooling_from_cli(&ToolingCliConfig::default()).expect("resolve");
        let result_set = execute_eval_command(command, &llm, &tooling).expect("eval batch");

        assert_eq!(result_set.results.len(), 2);
        assert_eq!(result_set.results[0].task_id, "t1");
        assert_eq!(result_set.results[1].task_id, "t2");

        let _ = fs::remove_file(&tasks_file);
        let _ = fs::remove_dir_all(&tasks_dir);
    }

}
