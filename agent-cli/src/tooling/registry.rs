use agent_core::tools::{EchoTool, ToolRegistry};
use agent_llm::OpenAiFunctionTool;
use serde_json::json;

#[derive(Debug)]
pub struct ToolSpec {
    pub name: &'static str,
    register_local_fn: fn(&mut ToolRegistry),
    provider_definition_fn: Option<fn() -> OpenAiFunctionTool>,
}

impl ToolSpec {
    pub fn register_local(&self, registry: &mut ToolRegistry) {
        (self.register_local_fn)(registry);
    }

    pub fn provider_definition(&self) -> Option<OpenAiFunctionTool> {
        self.provider_definition_fn.map(|build| build())
    }
}

fn register_echo(registry: &mut ToolRegistry) {
    registry.register(EchoTool);
}

fn echo_provider_definition() -> OpenAiFunctionTool {
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

static TOOL_CATALOG: [ToolSpec; 1] = [ToolSpec {
    name: "echo",
    register_local_fn: register_echo,
    provider_definition_fn: Some(echo_provider_definition),
}];

const DEFAULT_TOOL_NAMES: [&str; 1] = ["echo"];

pub fn catalog() -> &'static [ToolSpec] {
    &TOOL_CATALOG
}

pub fn find_tool(name: &str) -> Option<&'static ToolSpec> {
    TOOL_CATALOG.iter().find(|tool| tool.name == name)
}

pub fn default_tool_names() -> &'static [&'static str] {
    &DEFAULT_TOOL_NAMES
}
