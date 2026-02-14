use agent_core::tools::ToolRegistry;
use agent_llm::OpenAiFunctionTool;
use serde_json::json;

use super::read::ReadTool;

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

fn register_read(registry: &mut ToolRegistry) {
    registry.register(ReadTool::for_current_workspace());
}

fn read_provider_definition() -> OpenAiFunctionTool {
    OpenAiFunctionTool::new(
        "read",
        "Read a UTF-8 text file from the current workspace. Optional line range and truncation controls are supported.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Workspace-relative file path." },
                "start_line": { "type": "integer", "minimum": 1 },
                "end_line": { "type": "integer", "minimum": 1 },
                "max_bytes": { "type": "integer", "minimum": 1 }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    )
}

static TOOL_CATALOG: [ToolSpec; 1] = [ToolSpec {
    name: "read",
    register_local_fn: register_read,
    provider_definition_fn: Some(read_provider_definition),
}];

const DEFAULT_TOOL_NAMES: [&str; 1] = ["read"];

pub fn catalog() -> &'static [ToolSpec] {
    &TOOL_CATALOG
}

pub fn find_tool(name: &str) -> Option<&'static ToolSpec> {
    TOOL_CATALOG.iter().find(|tool| tool.name == name)
}

pub fn default_tool_names() -> &'static [&'static str] {
    &DEFAULT_TOOL_NAMES
}
