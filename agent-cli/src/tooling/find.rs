use std::{
    collections::VecDeque,
    fs,
    path::PathBuf,
};

use agent_core::tools::{Tool, ToolArgField, ToolArgSchema, ToolArgType};
use regex::Regex;
use serde_json::{Value, json};

use super::path_sandbox::WorkspacePathSandbox;

const DEFAULT_MAX_RESULTS: usize = 200;

fn should_skip_dir(name: &str) -> bool {
    name.starts_with('.') || matches!(name, "target" | "node_modules" | "__pycache__")
}

/// If the pattern contains glob characters (`*` or `?`), compile it as a
/// glob-to-regex.  Otherwise treat it as a plain substring match.
enum PatternMatcher {
    Glob(Regex),
    Substring(String),
}

impl PatternMatcher {
    fn new(pattern: &str) -> Result<Self, String> {
        if pattern.contains('*') || pattern.contains('?') {
            let re = glob_to_regex(pattern)?;
            Ok(Self::Glob(re))
        } else {
            Ok(Self::Substring(pattern.to_string()))
        }
    }

    fn matches(&self, name: &str) -> bool {
        match self {
            Self::Glob(re) => re.is_match(name),
            Self::Substring(s) => name.contains(s.as_str()),
        }
    }
}

fn glob_to_regex(pattern: &str) -> Result<Regex, String> {
    let mut re = String::from("(?i)^");
    for ch in pattern.chars() {
        match ch {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '.' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\' => {
                re.push('\\');
                re.push(ch);
            }
            _ => re.push(ch),
        }
    }
    re.push('$');
    Regex::new(&re).map_err(|err| format!("invalid glob pattern `{pattern}`: {err}"))
}

#[derive(Debug, Clone)]
pub struct FindTool {
    sandbox: WorkspacePathSandbox,
    default_max_results: usize,
}

impl FindTool {
    pub fn for_current_workspace() -> Self {
        let sandbox = WorkspacePathSandbox::for_current_dir()
            .unwrap_or_else(|_| WorkspacePathSandbox::new(PathBuf::from(".")));
        Self::new(sandbox)
    }

    pub fn new(sandbox: WorkspacePathSandbox) -> Self {
        Self {
            sandbox,
            default_max_results: DEFAULT_MAX_RESULTS,
        }
    }
}

impl Tool for FindTool {
    fn name(&self) -> &str {
        "find"
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
                    name: "max_results",
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

        let matcher = PatternMatcher::new(pattern_str)?;

        let search_root = match args.get("path").and_then(Value::as_str) {
            Some(p) => self.sandbox.resolve_search_path(p)?,
            None => self.sandbox.root().to_path_buf(),
        };

        if !search_root.is_dir() {
            return Err(format!(
                "path `{}` is not a directory",
                self.sandbox.display_path(&search_root)
            ));
        }

        let max_results = parse_optional_positive_usize(args, "max_results")?
            .unwrap_or(self.default_max_results);
        let collect_limit = max_results + 1; // +1 for truncation detection

        let mut results: Vec<Value> = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(search_root.clone());

        'walk: while let Some(dir) = queue.pop_front() {
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(_) => continue,
            };

            let mut subdirs = Vec::new();
            let mut items = Vec::new();

            for entry in entries.flatten() {
                let ft = match entry.file_type() {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
                let name = entry.file_name();
                let name_str = name.to_string_lossy();

                if ft.is_dir() {
                    if should_skip_dir(&name_str) {
                        continue;
                    }
                    if matcher.matches(&name_str) {
                        items.push((entry.path(), "dir"));
                    }
                    subdirs.push(entry.path());
                } else if ft.is_file() && matcher.matches(&name_str) {
                    items.push((entry.path(), "file"));
                } else if ft.is_symlink() && matcher.matches(&name_str) {
                    items.push((entry.path(), "symlink"));
                }
            }

            subdirs.sort();
            items.sort_by(|a, b| a.0.cmp(&b.0));

            for subdir in subdirs {
                queue.push_back(subdir);
            }

            for (path, kind) in items {
                results.push(json!({
                    "path": self.sandbox.display_path(&path),
                    "type": kind,
                }));
                if results.len() >= collect_limit {
                    break 'walk;
                }
            }
        }

        let truncated = results.len() > max_results;
        results.truncate(max_results);

        Ok(json!({
            "pattern": pattern_str,
            "search_root": self.sandbox.display_path(&search_root),
            "entries": results,
            "result_count": results.len(),
            "is_truncated": truncated,
        }))
    }
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

    use agent_core::tools::Tool;
    use serde_json::json;

    use super::FindTool;
    use crate::tooling::path_sandbox::WorkspacePathSandbox;

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agent-cli-find-tool-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn finds_files_by_glob_pattern() {
        let root = unique_temp_dir("glob");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(root.join("src/main.rs"), "").expect("write main.rs");
        fs::write(root.join("src/lib.rs"), "").expect("write lib.rs");
        fs::write(root.join("src/notes.txt"), "").expect("write notes.txt");
        fs::write(root.join("Cargo.toml"), "").expect("write Cargo.toml");

        let tool = FindTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool
            .run(&json!({ "pattern": "*.rs" }))
            .expect("find output");

        let entries = output.get("entries").and_then(|v| v.as_array()).unwrap();
        assert_eq!(entries.len(), 2);
        for entry in entries {
            let path = entry.get("path").and_then(|v| v.as_str()).unwrap();
            assert!(path.ends_with(".rs"), "unexpected: {path}");
            assert_eq!(
                entry.get("type").and_then(|v| v.as_str()),
                Some("file")
            );
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_files_by_substring() {
        let root = unique_temp_dir("substring");
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("config.yaml"), "").expect("write config");
        fs::write(root.join("readme.md"), "").expect("write readme");
        fs::write(root.join("myconfig.json"), "").expect("write myconfig");

        let tool = FindTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool
            .run(&json!({ "pattern": "config" }))
            .expect("find output");

        let entries = output.get("entries").and_then(|v| v.as_array()).unwrap();
        assert_eq!(entries.len(), 2);
        for entry in entries {
            let path = entry.get("path").and_then(|v| v.as_str()).unwrap();
            assert!(path.contains("config"), "unexpected: {path}");
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn respects_path_scope() {
        let root = unique_temp_dir("scope");
        fs::create_dir_all(root.join("a")).expect("create a");
        fs::create_dir_all(root.join("b")).expect("create b");
        fs::write(root.join("a/found.txt"), "").expect("write a/found");
        fs::write(root.join("b/found.txt"), "").expect("write b/found");

        let tool = FindTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool
            .run(&json!({ "pattern": "found", "path": "a" }))
            .expect("find output");

        let entries = output.get("entries").and_then(|v| v.as_array()).unwrap();
        assert_eq!(entries.len(), 1);
        let path = entries[0].get("path").and_then(|v| v.as_str()).unwrap();
        assert!(path.contains("a/found.txt") || path.contains("a\\found.txt"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn truncates_at_max_results() {
        let root = unique_temp_dir("truncate");
        fs::create_dir_all(&root).expect("create root");
        for i in 0..20 {
            fs::write(root.join(format!("file_{i}.txt")), "").expect("write file");
        }

        let tool = FindTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool
            .run(&json!({ "pattern": "*.txt", "max_results": 5 }))
            .expect("find output");

        let entries = output.get("entries").and_then(|v| v.as_array()).unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(
            output.get("is_truncated").and_then(|v| v.as_bool()),
            Some(true)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_hidden_directories() {
        let root = unique_temp_dir("hidden");
        fs::create_dir_all(root.join(".git")).expect("create .git");
        fs::write(root.join(".git/config"), "").expect("write git config");
        fs::write(root.join("visible.txt"), "").expect("write visible");

        let tool = FindTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool
            .run(&json!({ "pattern": "config" }))
            .expect("find output");

        let entries = output.get("entries").and_then(|v| v.as_array()).unwrap();
        assert!(entries.is_empty(), "should not find .git/config");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_path_outside_workspace() {
        let root = unique_temp_dir("escape-root");
        let outside = unique_temp_dir("escape-outside");
        fs::create_dir_all(&root).expect("create root");
        fs::create_dir_all(&outside).expect("create outside");

        let tool = FindTool::new(WorkspacePathSandbox::new(root.clone()));
        let err = tool
            .run(&json!({ "pattern": "*", "path": outside.to_string_lossy() }))
            .expect_err("must reject outside path");
        assert!(err.contains("escapes workspace root"));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn glob_is_case_insensitive() {
        let root = unique_temp_dir("case");
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("README.md"), "").expect("write README");
        fs::write(root.join("readme.txt"), "").expect("write readme.txt");

        let tool = FindTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool
            .run(&json!({ "pattern": "README*" }))
            .expect("find output");

        let entries = output.get("entries").and_then(|v| v.as_array()).unwrap();
        assert_eq!(entries.len(), 2);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_matching_directories() {
        let root = unique_temp_dir("dirs");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::create_dir_all(root.join("tests")).expect("create tests");
        fs::write(root.join("src/lib.rs"), "").expect("write lib");

        let tool = FindTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool
            .run(&json!({ "pattern": "src" }))
            .expect("find output");

        let entries = output.get("entries").and_then(|v| v.as_array()).unwrap();
        assert!(entries.len() >= 1);
        let dir_entry = entries
            .iter()
            .find(|e| e.get("type").and_then(|v| v.as_str()) == Some("dir"))
            .expect("should find src directory");
        let path = dir_entry.get("path").and_then(|v| v.as_str()).unwrap();
        assert!(path.contains("src"));

        let _ = fs::remove_dir_all(&root);
    }
}
