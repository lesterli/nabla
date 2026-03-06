use std::fs;

use nabla::tools::{Tool, ToolArgField, ToolArgSchema, ToolArgType};
use serde_json::{Value, json};

use super::path_sandbox::WorkspacePathSandbox;

#[derive(Debug, Clone)]
pub struct EditTool {
    sandbox: WorkspacePathSandbox,
}

impl EditTool {
    pub fn for_current_workspace() -> Self {
        let sandbox = WorkspacePathSandbox::for_current_dir()
            .unwrap_or_else(|_| WorkspacePathSandbox::new(std::path::PathBuf::from(".")));
        Self::new(sandbox)
    }

    pub fn new(sandbox: WorkspacePathSandbox) -> Self {
        Self { sandbox }
    }
}

impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn schema(&self) -> ToolArgSchema {
        ToolArgSchema::Object {
            fields: vec![
                ToolArgField::required("path", ToolArgType::String),
                ToolArgField::required("old_text", ToolArgType::String),
                ToolArgField::required("new_text", ToolArgType::String),
            ],
            allow_unknown_fields: false,
        }
    }

    fn run(&self, args: &Value) -> Result<Value, String> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing `path` argument".to_string())?;
        let old_text = args
            .get("old_text")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing `old_text` argument".to_string())?;
        let new_text = args
            .get("new_text")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing `new_text` argument".to_string())?;

        if old_text.is_empty() {
            return Err("`old_text` cannot be empty".to_string());
        }

        let resolved_path = self.sandbox.resolve_file(path)?;
        let bytes = fs::read(&resolved_path)
            .map_err(|err| format!("failed to read `{}`: {err}", resolved_path.display()))?;
        if bytes.contains(&0) {
            return Err("binary files are not supported".to_string());
        }

        let content =
            String::from_utf8(bytes).map_err(|_| "file is not valid UTF-8 text".to_string())?;

        let occurrences = content.match_indices(old_text).count();
        if occurrences == 0 {
            return Err(format!(
                "could not find `old_text` in `{}`",
                self.sandbox.display_path(&resolved_path)
            ));
        }
        if occurrences > 1 {
            return Err(format!(
                "found {occurrences} matches for `old_text` in `{}`; provide more specific context",
                self.sandbox.display_path(&resolved_path)
            ));
        }

        let byte_index = content
            .find(old_text)
            .ok_or_else(|| "failed to locate replacement target".to_string())?;
        let first_changed_line = content[..byte_index]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count() as u64
            + 1;

        let updated = content.replacen(old_text, new_text, 1);
        if updated == content {
            return Err("replacement made no changes".to_string());
        }

        fs::write(&resolved_path, updated.as_bytes())
            .map_err(|err| format!("failed to write `{}`: {err}", resolved_path.display()))?;

        Ok(json!({
            "path": self.sandbox.display_path(&resolved_path),
            "occurrences_replaced": 1,
            "bytes_written": updated.len(),
            "first_changed_line": first_changed_line
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::EditTool;
    use crate::tooling::path_sandbox::WorkspacePathSandbox;
    use nabla::tools::Tool;
    use serde_json::json;

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agent-cli-edit-tool-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn replaces_unique_occurrence() {
        let root = unique_temp_dir("replace");
        fs::create_dir_all(&root).expect("create root");
        let file = root.join("demo.txt");
        fs::write(&file, "hello\nold-value\nworld\n").expect("write seed file");

        let tool = EditTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool
            .run(&json!({
                "path": "demo.txt",
                "old_text": "old-value",
                "new_text": "new-value"
            }))
            .expect("edit output");

        assert_eq!(
            output.get("occurrences_replaced").and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            output.get("first_changed_line").and_then(|v| v.as_u64()),
            Some(2)
        );
        let actual = fs::read_to_string(&file).expect("read updated file");
        assert_eq!(actual, "hello\nnew-value\nworld\n");

        let _ = fs::remove_dir_all(&root);
    }

}
