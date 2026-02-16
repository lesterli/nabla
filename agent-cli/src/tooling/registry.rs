use agent_core::tools::ToolRegistry;
use agent_llm::OpenAiFunctionTool;
use serde_json::json;

use super::bash::BashTool;
use super::edit::EditTool;
use super::find::FindTool;
use super::grep::GrepTool;
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

fn register_bash(registry: &mut ToolRegistry) {
    registry.register(BashTool::for_current_workspace());
}

fn register_grep(registry: &mut ToolRegistry) {
    registry.register(GrepTool::for_current_workspace());
}

fn register_find(registry: &mut ToolRegistry) {
    registry.register(FindTool::for_current_workspace());
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

fn grep_provider_definition() -> OpenAiFunctionTool {
    OpenAiFunctionTool::new(
        "grep",
        "Search file contents by regex pattern within the workspace. Returns matching file paths, line numbers, and snippets.",
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to search for." },
                "path": { "type": "string", "description": "Optional file or directory path to search within (defaults to workspace root)." },
                "include": { "type": "string", "description": "Optional filename glob (e.g. \"*.rs\") to filter which files are searched." },
                "max_matches": { "type": "integer", "minimum": 1, "description": "Maximum number of matching lines to return (default 100)." }
            },
            "required": ["pattern"],
            "additionalProperties": false
        }),
    )
}

fn find_provider_definition() -> OpenAiFunctionTool {
    OpenAiFunctionTool::new(
        "find",
        "Find files and directories by name pattern within the workspace. Supports glob wildcards (* ?) and substring matching.",
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern (e.g. \"*.rs\", \"test_?\") or substring to match against filenames." },
                "path": { "type": "string", "description": "Optional directory path to search within (defaults to workspace root)." },
                "max_results": { "type": "integer", "minimum": 1, "description": "Maximum number of entries to return (default 200)." }
            },
            "required": ["pattern"],
            "additionalProperties": false
        }),
    )
}

fn bash_provider_definition() -> OpenAiFunctionTool {
    OpenAiFunctionTool::new(
        "bash",
        "Run a shell command in the workspace root and return stdout/stderr plus exit status.",
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute." },
                "timeout_ms": { "type": "integer", "minimum": 1, "description": "Optional timeout in milliseconds (max 30000)." },
                "max_output_bytes": { "type": "integer", "minimum": 1, "description": "Optional stdout/stderr truncation size in bytes." }
            },
            "required": ["command"],
            "additionalProperties": false
        }),
    )
}

static TOOL_CATALOG: [ToolSpec; 6] = [
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
    ToolSpec {
        name: "bash",
        register_local_fn: register_bash,
        provider_definition_fn: Some(bash_provider_definition),
    },
    ToolSpec {
        name: "grep",
        register_local_fn: register_grep,
        provider_definition_fn: Some(grep_provider_definition),
    },
    ToolSpec {
        name: "find",
        register_local_fn: register_find,
        provider_definition_fn: Some(find_provider_definition),
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
