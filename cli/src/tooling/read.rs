use std::fs;

use nabla::tools::{Tool, ToolArgField, ToolArgSchema, ToolArgType};
use serde_json::{Value, json};

use super::path_sandbox::WorkspacePathSandbox;

const DEFAULT_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct ReadTool {
    sandbox: WorkspacePathSandbox,
    default_max_bytes: usize,
}

impl ReadTool {
    pub fn for_current_workspace() -> Self {
        let sandbox = WorkspacePathSandbox::for_current_dir()
            .unwrap_or_else(|_| WorkspacePathSandbox::new(std::path::PathBuf::from(".")));
        Self::new(sandbox)
    }

    pub fn new(sandbox: WorkspacePathSandbox) -> Self {
        Self {
            sandbox,
            default_max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn schema(&self) -> ToolArgSchema {
        ToolArgSchema::Object {
            fields: vec![
                ToolArgField::required("path", ToolArgType::String),
                ToolArgField {
                    name: "start_line",
                    arg_type: ToolArgType::Number,
                    required: false,
                },
                ToolArgField {
                    name: "end_line",
                    arg_type: ToolArgType::Number,
                    required: false,
                },
                ToolArgField {
                    name: "max_bytes",
                    arg_type: ToolArgType::Number,
                    required: false,
                },
            ],
            allow_unknown_fields: false,
        }
    }

    fn run(&self, args: &Value) -> Result<Value, String> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing `path` argument".to_string())?;

        let start_line = parse_optional_positive_u64(args, "start_line")?;
        let end_line = parse_optional_positive_u64(args, "end_line")?;

        if let (Some(start), Some(end)) = (start_line, end_line) {
            if start > end {
                return Err("`start_line` must be <= `end_line`".to_string());
            }
        }

        let requested_max_bytes = parse_optional_positive_u64(args, "max_bytes")?
            .map(|value| value as usize)
            .unwrap_or(self.default_max_bytes);

        let resolved_path = self.sandbox.resolve_file(path)?;
        let bytes = fs::read(&resolved_path)
            .map_err(|err| format!("failed to read `{}`: {err}", resolved_path.display()))?;

        if bytes.contains(&0) {
            return Err("binary files are not supported".to_string());
        }

        let content =
            String::from_utf8(bytes).map_err(|_| "file is not valid UTF-8 text".to_string())?;
        let total_lines = content.lines().count() as u64;

        let (line_start, line_end, selected_content) =
            select_line_range(&content, start_line, end_line)?;
        let total_selected_bytes = selected_content.len();
        let (returned_content, is_truncated) =
            truncate_utf8_by_bytes(&selected_content, requested_max_bytes);

        Ok(json!({
            "path": self.sandbox.display_path(&resolved_path),
            "content": returned_content,
            "is_truncated": is_truncated,
            "total_bytes": total_selected_bytes,
            "returned_bytes": returned_content.len(),
            "line_start": line_start,
            "line_end": line_end,
            "total_lines": total_lines,
        }))
    }
}

fn parse_optional_positive_u64(args: &Value, key: &str) -> Result<Option<u64>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };

    let Some(as_u64) = value.as_u64() else {
        return Err(format!("`{key}` must be a positive integer"));
    };

    if as_u64 == 0 {
        return Err(format!("`{key}` must be >= 1"));
    }

    Ok(Some(as_u64))
}

fn select_line_range(
    content: &str,
    start_line: Option<u64>,
    end_line: Option<u64>,
) -> Result<(Value, Value, String), String> {
    let lines = content.lines().collect::<Vec<_>>();
    let total_lines = lines.len() as u64;

    if total_lines == 0 {
        if start_line.is_some() || end_line.is_some() {
            return Err("line range is out of bounds for empty file".to_string());
        }
        return Ok((Value::Null, Value::Null, String::new()));
    }

    let start = start_line.unwrap_or(1);
    let end = end_line.unwrap_or(total_lines);

    if start > total_lines {
        return Err(format!(
            "`start_line` ({start}) exceeds total lines ({total_lines})"
        ));
    }
    if end > total_lines {
        return Err(format!(
            "`end_line` ({end}) exceeds total lines ({total_lines})"
        ));
    }
    if start > end {
        return Err("`start_line` must be <= `end_line`".to_string());
    }

    let begin = (start - 1) as usize;
    let finish = end as usize;
    let selected = lines[begin..finish].join("\n");

    Ok((json!(start), json!(end), selected))
}

fn truncate_utf8_by_bytes(input: &str, max_bytes: usize) -> (String, bool) {
    if input.len() <= max_bytes {
        return (input.to_string(), false);
    }

    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }

    (input[..end].to_string(), true)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::ReadTool;
    use crate::tooling::path_sandbox::WorkspacePathSandbox;
    use nabla::tools::Tool;
    use serde_json::json;

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agent-cli-read-tool-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn reads_file_content_and_line_range() {
        let root = unique_temp_dir("line-range");
        fs::create_dir_all(&root).expect("create root");
        let file = root.join("notes.txt");
        fs::write(&file, "line1\nline2\nline3\nline4\n").expect("write file");

        let tool = ReadTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool
            .run(&json!({
                "path": "notes.txt",
                "start_line": 2,
                "end_line": 3
            }))
            .expect("read tool output");

        assert_eq!(
            output.get("content").and_then(|v| v.as_str()),
            Some("line2\nline3")
        );
        assert_eq!(output.get("line_start").and_then(|v| v.as_u64()), Some(2));
        assert_eq!(output.get("line_end").and_then(|v| v.as_u64()), Some(3));
        assert_eq!(
            output.get("is_truncated").and_then(|v| v.as_bool()),
            Some(false)
        );

        let _ = fs::remove_dir_all(&root);
    }

}
