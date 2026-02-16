use std::{
    collections::VecDeque,
    fs,
    path::PathBuf,
};

use agent_core::tools::{Tool, ToolArgField, ToolArgSchema, ToolArgType};
use serde_json::{Value, json};

use super::path_sandbox::WorkspacePathSandbox;

const DEFAULT_MAX_ENTRIES: usize = 200;
const DEFAULT_MAX_DEPTH: usize = 1;

#[derive(Debug, Clone)]
pub struct LsTool {
    sandbox: WorkspacePathSandbox,
    default_max_entries: usize,
    default_max_depth: usize,
}

impl LsTool {
    pub fn for_current_workspace() -> Self {
        let sandbox = WorkspacePathSandbox::for_current_dir()
            .unwrap_or_else(|_| WorkspacePathSandbox::new(PathBuf::from(".")));
        Self::new(sandbox)
    }

    pub fn new(sandbox: WorkspacePathSandbox) -> Self {
        Self {
            sandbox,
            default_max_entries: DEFAULT_MAX_ENTRIES,
            default_max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

impl Tool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }

    fn schema(&self) -> ToolArgSchema {
        ToolArgSchema::Object {
            fields: vec![
                ToolArgField {
                    name: "path",
                    arg_type: ToolArgType::String,
                    required: false,
                },
                ToolArgField {
                    name: "depth",
                    arg_type: ToolArgType::Number,
                    required: false,
                },
                ToolArgField {
                    name: "max_entries",
                    arg_type: ToolArgType::Number,
                    required: false,
                },
            ],
            allow_unknown_fields: false,
        }
    }

    fn run(&self, args: &Value) -> Result<Value, String> {
        let dir_path = match args.get("path").and_then(Value::as_str) {
            Some(p) => self.sandbox.resolve_search_path(p)?,
            None => self.sandbox.root().to_path_buf(),
        };

        if !dir_path.is_dir() {
            return Err(format!(
                "path `{}` is not a directory",
                self.sandbox.display_path(&dir_path)
            ));
        }

        let max_depth = parse_optional_positive_usize(args, "depth")?
            .unwrap_or(self.default_max_depth);
        let max_entries = parse_optional_positive_usize(args, "max_entries")?
            .unwrap_or(self.default_max_entries);
        let collect_limit = max_entries + 1;

        let mut entries: Vec<Value> = Vec::new();
        // BFS queue: (directory_path, current_depth)
        let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
        queue.push_back((dir_path.clone(), 0));

        'walk: while let Some((current_dir, depth)) = queue.pop_front() {
            let read_dir = match fs::read_dir(&current_dir) {
                Ok(rd) => rd,
                Err(_) => continue,
            };

            let mut subdirs = Vec::new();
            let mut items: Vec<(PathBuf, &str, Option<u64>)> = Vec::new();

            for entry in read_dir.flatten() {
                let ft = match entry.file_type() {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
                let path = entry.path();

                if ft.is_dir() {
                    subdirs.push(path.clone());
                    items.push((path, "dir", None));
                } else if ft.is_file() {
                    let size = entry.metadata().ok().map(|m| m.len());
                    items.push((path, "file", size));
                } else if ft.is_symlink() {
                    items.push((path, "symlink", None));
                }
            }

            // Sort: directories first, then files, alphabetically within each group.
            items.sort_by(|a, b| {
                let dir_order = |kind: &str| -> u8 {
                    if kind == "dir" { 0 } else { 1 }
                };
                dir_order(a.1)
                    .cmp(&dir_order(b.1))
                    .then_with(|| a.0.cmp(&b.0))
            });

            for (path, kind, size) in items {
                let display = self.sandbox.display_path(&path);
                let mut entry = json!({
                    "path": display,
                    "type": kind,
                });
                if let Some(s) = size {
                    entry["size"] = json!(s);
                }
                entries.push(entry);
                if entries.len() >= collect_limit {
                    break 'walk;
                }
            }

            // Enqueue subdirectories for the next level if within depth limit.
            if depth + 1 < max_depth {
                subdirs.sort();
                for subdir in subdirs {
                    queue.push_back((subdir, depth + 1));
                }
            }
        }

        let truncated = entries.len() > max_entries;
        entries.truncate(max_entries);

        Ok(json!({
            "path": self.sandbox.display_path(&dir_path),
            "entries": entries,
            "entry_count": entries.len(),
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

    use super::LsTool;
    use crate::tooling::path_sandbox::WorkspacePathSandbox;

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agent-cli-ls-tool-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn lists_immediate_children() {
        let root = unique_temp_dir("basic");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(root.join("Cargo.toml"), "[package]").expect("write toml");
        fs::write(root.join("src/main.rs"), "fn main() {}").expect("write main");

        let tool = LsTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool.run(&json!({})).expect("ls output");

        let entries = output.get("entries").and_then(|v| v.as_array()).unwrap();
        // depth=1: only root-level items (src dir + Cargo.toml), not src/main.rs
        assert_eq!(entries.len(), 2);

        // Directories should come before files in sort order.
        assert_eq!(
            entries[0].get("type").and_then(|v| v.as_str()),
            Some("dir")
        );
        assert_eq!(
            entries[1].get("type").and_then(|v| v.as_str()),
            Some("file")
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn lists_subdirectory_with_path() {
        let root = unique_temp_dir("subdir");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(root.join("src/main.rs"), "fn main() {}").expect("write main");
        fs::write(root.join("src/lib.rs"), "").expect("write lib");

        let tool = LsTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool
            .run(&json!({ "path": "src" }))
            .expect("ls output");

        let entries = output.get("entries").and_then(|v| v.as_array()).unwrap();
        assert_eq!(entries.len(), 2);
        for entry in entries {
            let path = entry.get("path").and_then(|v| v.as_str()).unwrap();
            assert!(path.contains("src/") || path.contains("src\\"));
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recurses_with_depth() {
        let root = unique_temp_dir("depth");
        fs::create_dir_all(root.join("a/b")).expect("create a/b");
        fs::write(root.join("top.txt"), "").expect("write top");
        fs::write(root.join("a/mid.txt"), "").expect("write mid");
        fs::write(root.join("a/b/deep.txt"), "").expect("write deep");

        let tool = LsTool::new(WorkspacePathSandbox::new(root.clone()));

        // depth=1: only top-level
        let out1 = tool.run(&json!({ "depth": 1 })).expect("depth=1");
        let e1 = out1.get("entries").and_then(|v| v.as_array()).unwrap();
        assert_eq!(e1.len(), 2); // a/ + top.txt

        // depth=2: top + a's children
        let out2 = tool.run(&json!({ "depth": 2 })).expect("depth=2");
        let e2 = out2.get("entries").and_then(|v| v.as_array()).unwrap();
        assert_eq!(e2.len(), 4); // a/ + top.txt + a/b/ + a/mid.txt

        // depth=3: everything
        let out3 = tool.run(&json!({ "depth": 3 })).expect("depth=3");
        let e3 = out3.get("entries").and_then(|v| v.as_array()).unwrap();
        assert_eq!(e3.len(), 5); // all entries

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn files_include_size() {
        let root = unique_temp_dir("size");
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("hello.txt"), "hello").expect("write file");

        let tool = LsTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool.run(&json!({})).expect("ls output");

        let entries = output.get("entries").and_then(|v| v.as_array()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].get("size").and_then(|v| v.as_u64()),
            Some(5)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn truncates_at_max_entries() {
        let root = unique_temp_dir("truncate");
        fs::create_dir_all(&root).expect("create root");
        for i in 0..20 {
            fs::write(root.join(format!("f{i:02}.txt")), "").expect("write file");
        }

        let tool = LsTool::new(WorkspacePathSandbox::new(root.clone()));
        let output = tool
            .run(&json!({ "max_entries": 5 }))
            .expect("ls output");

        let entries = output.get("entries").and_then(|v| v.as_array()).unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(
            output.get("is_truncated").and_then(|v| v.as_bool()),
            Some(true)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_non_directory_path() {
        let root = unique_temp_dir("not-dir");
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("file.txt"), "data").expect("write file");

        let tool = LsTool::new(WorkspacePathSandbox::new(root.clone()));
        let err = tool
            .run(&json!({ "path": "file.txt" }))
            .expect_err("must reject non-directory");
        assert!(err.contains("is not a directory"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_path_outside_workspace() {
        let root = unique_temp_dir("escape-root");
        let outside = unique_temp_dir("escape-outside");
        fs::create_dir_all(&root).expect("create root");
        fs::create_dir_all(&outside).expect("create outside");

        let tool = LsTool::new(WorkspacePathSandbox::new(root.clone()));
        let err = tool
            .run(&json!({ "path": outside.to_string_lossy() }))
            .expect_err("must reject outside path");
        assert!(err.contains("escapes workspace root"));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }
}
