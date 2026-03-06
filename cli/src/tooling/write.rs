use std::fs;

use nabla::tools::{Tool, ToolArgField, ToolArgSchema, ToolArgType};
use serde_json::{Value, json};

use super::path_sandbox::WorkspacePathSandbox;

#[derive(Debug, Clone)]
pub struct WriteTool {
    sandbox: WorkspacePathSandbox,
}

impl WriteTool {
    pub fn for_current_workspace() -> Self {
        let sandbox = WorkspacePathSandbox::for_current_dir()
            .unwrap_or_else(|_| WorkspacePathSandbox::new(std::path::PathBuf::from(".")));
        Self::new(sandbox)
    }

    pub fn new(sandbox: WorkspacePathSandbox) -> Self {
        Self { sandbox }
    }
}

impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn schema(&self) -> ToolArgSchema {
        ToolArgSchema::Object {
            fields: vec![
                ToolArgField::required("path", ToolArgType::String),
                ToolArgField::required("content", ToolArgType::String),
            ],
            allow_unknown_fields: false,
        }
    }

    fn run(&self, args: &Value) -> Result<Value, String> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing `path` argument".to_string())?;
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing `content` argument".to_string())?;

        let writable_path = self.sandbox.resolve_writable_file(path)?;
        let created = !writable_path.exists();

        fs::write(&writable_path, content)
            .map_err(|err| format!("failed to write `{}`: {err}", writable_path.display()))?;

        Ok(json!({
            "path": self.sandbox.display_path(&writable_path),
            "bytes_written": content.as_bytes().len(),
            "created": created
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::WriteTool;
    use crate::tooling::path_sandbox::WorkspacePathSandbox;
    use nabla::tools::Tool;
    use serde_json::json;

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agent-cli-write-tool-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn creates_new_file_inside_workspace() {
        let root = unique_temp_dir("create");
        fs::create_dir_all(&root).expect("create root");

        let tool = WriteTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool
            .run(&json!({
                "path": "new.txt",
                "content": "hello write"
            }))
            .expect("write output");

        assert_eq!(output.get("created").and_then(|v| v.as_bool()), Some(true));
        let written = fs::read_to_string(root.join("new.txt")).expect("read written file");
        assert_eq!(written, "hello write");

        let _ = fs::remove_dir_all(&root);
    }

}
