use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct WorkspacePathSandbox {
    root: PathBuf,
}

impl WorkspacePathSandbox {
    pub fn new(root: PathBuf) -> Self {
        let canonical_root = fs::canonicalize(&root).unwrap_or(root);
        Self {
            root: canonical_root,
        }
    }

    pub fn for_current_dir() -> Result<Self, String> {
        let cwd = std::env::current_dir().map_err(|err| format!("failed to resolve cwd: {err}"))?;
        Ok(Self::new(cwd))
    }

    pub fn resolve_file(&self, raw_path: &str) -> Result<PathBuf, String> {
        let trimmed = raw_path.trim();
        if trimmed.is_empty() {
            return Err("`path` cannot be empty".to_string());
        }

        let input = Path::new(trimmed);
        let candidate = if input.is_absolute() {
            input.to_path_buf()
        } else {
            self.root.join(input)
        };

        let canonical_candidate = fs::canonicalize(&candidate)
            .map_err(|err| format!("path `{trimmed}` is not accessible: {err}"))?;

        if !canonical_candidate.starts_with(&self.root) {
            return Err(format!(
                "path `{trimmed}` escapes workspace root `{}`",
                self.root.display()
            ));
        }

        let metadata = fs::metadata(&canonical_candidate)
            .map_err(|err| format!("failed to stat `{trimmed}`: {err}"))?;
        if !metadata.is_file() {
            return Err(format!("path `{trimmed}` is not a file"));
        }

        Ok(canonical_candidate)
    }

    pub fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map(|relative| relative.display().to_string())
            .unwrap_or_else(|_| path.display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::WorkspacePathSandbox;

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agent-cli-path-sandbox-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn resolves_file_inside_workspace() {
        let root = unique_temp_dir("inside");
        fs::create_dir_all(&root).expect("create root");
        let file = root.join("ok.txt");
        fs::write(&file, "hello").expect("write file");

        let sandbox = WorkspacePathSandbox::new(root.clone());
        let resolved = sandbox.resolve_file("ok.txt").expect("resolve file");
        assert_eq!(resolved, fs::canonicalize(&file).expect("canonical file"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_path_outside_workspace() {
        let root = unique_temp_dir("root");
        let outside_parent = unique_temp_dir("outside-parent");
        fs::create_dir_all(&root).expect("create root");
        fs::create_dir_all(&outside_parent).expect("create outside parent");
        let outside_file = outside_parent.join("outside.txt");
        fs::write(&outside_file, "secret").expect("write outside file");

        let sandbox = WorkspacePathSandbox::new(root.clone());
        let err = sandbox
            .resolve_file(&outside_file.to_string_lossy())
            .expect_err("must reject outside path");
        assert!(err.contains("escapes workspace root"));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside_parent);
    }
}
