use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
};

use nabla::tools::{Tool, ToolArgField, ToolArgSchema, ToolArgType};
use regex::Regex;
use serde_json::{Value, json};

use super::path_sandbox::WorkspacePathSandbox;

const DEFAULT_MAX_MATCHES: usize = 100;
const MAX_LINE_DISPLAY_BYTES: usize = 500;

fn should_skip_dir(name: &str) -> bool {
    name.starts_with('.') || matches!(name, "target" | "node_modules" | "__pycache__")
}

fn matches_include(filename: &str, pattern: &str) -> bool {
    match pattern.strip_prefix('*') {
        Some(suffix) => filename.ends_with(suffix),
        None => filename == pattern,
    }
}

#[derive(Debug, Clone)]
pub struct GrepTool {
    sandbox: WorkspacePathSandbox,
    default_max_matches: usize,
}

impl GrepTool {
    pub fn for_current_workspace() -> Self {
        let sandbox = WorkspacePathSandbox::for_current_dir()
            .unwrap_or_else(|_| WorkspacePathSandbox::new(PathBuf::from(".")));
        Self::new(sandbox)
    }

    pub fn new(sandbox: WorkspacePathSandbox) -> Self {
        Self {
            sandbox,
            default_max_matches: DEFAULT_MAX_MATCHES,
        }
    }
}

impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn schema(&self) -> ToolArgSchema {
        ToolArgSchema::Object {
            fields: vec![
                ToolArgField::required("pattern", ToolArgType::String),
                ToolArgField {
                    name: "path",
                    arg_type: ToolArgType::String,
                    required: false,
                },
                ToolArgField {
                    name: "include",
                    arg_type: ToolArgType::String,
                    required: false,
                },
                ToolArgField {
                    name: "max_matches",
                    arg_type: ToolArgType::Number,
                    required: false,
                },
            ],
            allow_unknown_fields: false,
        }
    }

    fn run(&self, args: &Value) -> Result<Value, String> {
        let pattern_str = args
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing `pattern` argument".to_string())?;

        let re = Regex::new(pattern_str)
            .map_err(|err| format!("invalid regex pattern `{pattern_str}`: {err}"))?;

        let search_root = match args.get("path").and_then(Value::as_str) {
            Some(p) => self.sandbox.resolve_search_path(p)?,
            None => self.sandbox.root().to_path_buf(),
        };

        let include = args.get("include").and_then(Value::as_str);
        let max_matches =
            parse_optional_positive_usize(args, "max_matches")?.unwrap_or(self.default_max_matches);

        let mut matches: Vec<Value> = Vec::new();
        let mut files_searched: u64 = 0;
        let mut files_matched: u64 = 0;

        if search_root.is_file() {
            files_searched = 1;
            let hits = search_file(&re, &search_root, max_matches + 1);
            if !hits.is_empty() {
                files_matched = 1;
                let display = self.sandbox.display_path(&search_root);
                for hit in hits {
                    matches.push(hit.to_json(&display));
                }
            }
        } else {
            let mut queue = VecDeque::new();
            queue.push_back(search_root.clone());

            'walk: while let Some(dir) = queue.pop_front() {
                let entries = match fs::read_dir(&dir) {
                    Ok(entries) => entries,
                    Err(_) => continue,
                };

                let mut subdirs = Vec::new();
                let mut files = Vec::new();

                for entry in entries.flatten() {
                    let ft = match entry.file_type() {
                        Ok(ft) => ft,
                        Err(_) => continue,
                    };
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();

                    if ft.is_dir() {
                        if !should_skip_dir(&name_str) {
                            subdirs.push(entry.path());
                        }
                    } else if ft.is_file() {
                        if let Some(inc) = include
                            && !matches_include(&name_str, inc)
                        {
                            continue;
                        }
                        files.push(entry.path());
                    }
                }

                subdirs.sort();
                files.sort();

                for subdir in subdirs {
                    queue.push_back(subdir);
                }

                for file_path in files {
                    files_searched += 1;
                    let remaining = (max_matches + 1).saturating_sub(matches.len());
                    let hits = search_file(&re, &file_path, remaining);
                    if !hits.is_empty() {
                        files_matched += 1;
                        let display = self.sandbox.display_path(&file_path);
                        for hit in hits {
                            matches.push(hit.to_json(&display));
                        }
                        if matches.len() > max_matches {
                            break 'walk;
                        }
                    }
                }
            }
        }

        let truncated = matches.len() > max_matches;
        matches.truncate(max_matches);

        Ok(json!({
            "pattern": pattern_str,
            "search_root": self.sandbox.display_path(&search_root),
            "matches": matches,
            "match_count": matches.len(),
            "files_searched": files_searched,
            "files_matched": files_matched,
            "is_truncated": truncated,
        }))
    }
}

struct LineHit {
    line: u64,
    content: String,
}

impl LineHit {
    fn to_json(&self, file: &str) -> Value {
        json!({
            "file": file,
            "line": self.line,
            "content": self.content,
        })
    }
}

fn search_file(re: &Regex, path: &Path, limit: usize) -> Vec<LineHit> {
    if limit == 0 {
        return Vec::new();
    }

    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };

    // Skip binary files (files containing null bytes in the first 8KB).
    let probe = &bytes[..bytes.len().min(8192)];
    if probe.contains(&0) {
        return Vec::new();
    }

    let content = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let mut hits = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        if re.is_match(line) {
            let display = if line.len() > MAX_LINE_DISPLAY_BYTES {
                let mut end = MAX_LINE_DISPLAY_BYTES;
                while end > 0 && !line.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}...", &line[..end])
            } else {
                line.to_string()
            };

            hits.push(LineHit {
                line: (idx + 1) as u64,
                content: display,
            });

            if hits.len() >= limit {
                break;
            }
        }
    }

    hits
}

fn parse_optional_positive_usize(args: &Value, key: &str) -> Result<Option<usize>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    let Some(as_u64) = value.as_u64() else {
        return Err(format!("`{key}` must be a positive integer"));
    };
    if as_u64 == 0 {
        return Err(format!("`{key}` must be >= 1"));
    }
    Ok(Some(as_u64 as usize))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use nabla::tools::Tool;
    use serde_json::json;

    use super::GrepTool;
    use crate::tooling::path_sandbox::WorkspacePathSandbox;

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agent-cli-grep-tool-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn finds_matches_with_line_numbers() {
        let root = unique_temp_dir("basic");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .expect("write main.rs");
        fs::write(
            root.join("src/lib.rs"),
            "pub fn greet() {\n    println!(\"hi\");\n}\n",
        )
        .expect("write lib.rs");

        let tool = GrepTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool
            .run(&json!({ "pattern": "println" }))
            .expect("grep output");

        let matches = output.get("matches").and_then(|v| v.as_array()).unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(
            output.get("files_matched").and_then(|v| v.as_u64()),
            Some(2)
        );

        // Each match should have file, line, content
        for m in matches {
            assert!(m.get("file").and_then(|v| v.as_str()).is_some());
            assert!(m.get("line").and_then(|v| v.as_u64()).is_some());
            let content = m.get("content").and_then(|v| v.as_str()).unwrap();
            assert!(content.contains("println"));
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn respects_include_filter() {
        let root = unique_temp_dir("include");
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("hello.rs"), "fn hello() {}\n").expect("write rs");
        fs::write(root.join("hello.txt"), "hello world\n").expect("write txt");

        let tool = GrepTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool
            .run(&json!({ "pattern": "hello", "include": "*.rs" }))
            .expect("grep output");

        let matches = output.get("matches").and_then(|v| v.as_array()).unwrap();
        assert_eq!(matches.len(), 1);
        let file = matches[0].get("file").and_then(|v| v.as_str()).unwrap();
        assert!(file.ends_with(".rs"));

        let _ = fs::remove_dir_all(&root);
    }

}
