use agent_core::tools::ToolRegistry;
use agent_llm::OpenAiFunctionTool;
use serde_json::json;

use super::edit::EditTool;
use super::read::ReadTool;
use super::write::WriteTool;

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

fn register_write(registry: &mut ToolRegistry) {
    registry.register(WriteTool::for_current_workspace());
}

fn register_edit(registry: &mut ToolRegistry) {
    registry.register(EditTool::for_current_workspace());
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

fn write_provider_definition() -> OpenAiFunctionTool {
    OpenAiFunctionTool::new(
        "write",
        "Write UTF-8 text to a file in the current workspace. Creates or overwrites files.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Workspace-relative file path." },
                "content": { "type": "string", "description": "Full file content to write." }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        }),
    )
}

fn edit_provider_definition() -> OpenAiFunctionTool {
    OpenAiFunctionTool::new(
        "edit",
        "Edit an existing UTF-8 file by replacing one exact unique text fragment with new text.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Workspace-relative file path." },
                "old_text": { "type": "string", "description": "Exact text to replace (must be unique)." },
                "new_text": { "type": "string", "description": "Replacement text." }
            },
            "required": ["path", "old_text", "new_text"],
            "additionalProperties": false
        }),
    )
}

static TOOL_CATALOG: [ToolSpec; 3] = [
    ToolSpec {
        name: "read",
        register_local_fn: register_read,
        provider_definition_fn: Some(read_provider_definition),
    },
    ToolSpec {
        name: "write",
        register_local_fn: register_write,
        provider_definition_fn: Some(write_provider_definition),
    },
    ToolSpec {
        name: "edit",
        register_local_fn: register_edit,
        provider_definition_fn: Some(edit_provider_definition),
    },
];

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
