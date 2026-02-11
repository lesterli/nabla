use agent_core::{
    memory::InMemoryEventStore,
    policy::AllowAllPolicy,
    protocol::{Op, StopReason},
    runtime::AgentRuntime,
    tools::{EchoTool, ToolRegistry},
};
use agent_llm::{MultiProviderGateway, OpenAiCompatibleProvider, StaticProvider};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn print_usage(program: &str) {
    println!(
        "Usage: {program} [OPTIONS] <prompt>\n\nOptions:\n  -h, --help     Show this help message\n  -V, --version  Show version\n\nLLM env:\n  AGENT_LLM_PROVIDER=mock|openai|openai_compatible (default: mock)\n  AGENT_LLM_BASE_URL (default: https://api.openai.com/v1)\n  AGENT_LLM_API_KEY (required for openai/openai_compatible)\n  AGENT_LLM_MODEL (default: gpt-4o-mini)\n  AGENT_LLM_NAME (optional provider display name)"
    );
}

fn build_gateway_from_env() -> Result<MultiProviderGateway, String> {
    let provider = std::env::var("AGENT_LLM_PROVIDER").unwrap_or_else(|_| "mock".to_string());

    match provider.as_str() {
        "mock" => Ok(MultiProviderGateway::new()
            .with_provider(StaticProvider::new("mock", "assistant> "))),
        "openai" | "openai_compatible" => {
            let base_url = std::env::var("AGENT_LLM_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
            let api_key = std::env::var("AGENT_LLM_API_KEY")
                .map_err(|_| "AGENT_LLM_API_KEY is required for real providers".to_string())?;
            let model =
                std::env::var("AGENT_LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
            let name =
                std::env::var("AGENT_LLM_NAME").unwrap_or_else(|_| "openai-compatible".to_string());

            let provider = OpenAiCompatibleProvider::new(name, base_url, api_key, model)?;
            Ok(MultiProviderGateway::new().with_provider(provider))
        }
        other => Err(format!(
            "unsupported AGENT_LLM_PROVIDER value `{other}` (expected: mock, openai, openai_compatible)"
        )),
    }
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

    let llm = match build_gateway_from_env() {
        Ok(gateway) => gateway,
        Err(err) => {
            eprintln!("LLM configuration error: {err}");
            std::process::exit(2);
        }
    };

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
