use agent_core::{
    memory::InMemoryEventStore,
    policy::AllowAllPolicy,
    protocol::{Op, StopReason},
    runtime::AgentRuntime,
    tools::{EchoTool, ToolRegistry},
};
use agent_llm::{MultiProviderGateway, StaticProvider};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn print_usage(program: &str) {
    println!(
        "Usage: {program} [OPTIONS] <prompt>\n\nOptions:\n  -h, --help     Show this help message\n  -V, --version  Show version"
    );
}

fn main() {
    let mut args = std::env::args();
    let program = args.next().unwrap_or_else(|| "agent-cli".to_string());
    let rest: Vec<String> = args.collect();

    if rest.is_empty() {
        print_usage(&program);
        std::process::exit(2);
    }

    if rest.len() == 1 {
        match rest[0].as_str() {
            "-h" | "--help" => {
                print_usage(&program);
                return;
            }
            "-V" | "--version" => {
                println!("{VERSION}");
                return;
            }
            _ => {}
        }
    }

    if rest.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_usage(&program);
        return;
    }

    if rest.iter().any(|arg| arg == "-V" || arg == "--version") {
        println!("{VERSION}");
        return;
    }

    let prompt = rest.join(" ");
    if prompt.trim().is_empty() {
        print_usage(&program);
        std::process::exit(2);
    }

    let mut runtime = AgentRuntime::default();
    let policy = AllowAllPolicy;

    let mut tools = ToolRegistry::default();
    tools.register(EchoTool);

    let mut store = InMemoryEventStore::default();

    let llm = MultiProviderGateway::new().with_provider(StaticProvider::new("mock", "assistant> "));

    let turn = runtime.run_turn(
        Op::UserInput {
            submission_id: "cli-session-1".to_string(),
            input: prompt,
        },
        &llm,
        &policy,
        &tools,
        &mut store,
    );

    for event in turn.events {
        let line = serde_json::to_string(&event).expect("serialize event");
        println!("{line}");
    }

    if turn.stop_reason != StopReason::Done {
        std::process::exit(1);
    }
}
