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
        let (trimmed, candidate) = self.parse_candidate(raw_path)?;

        let canonical_candidate = fs::canonicalize(&candidate)
            .map_err(|err| format!("path `{trimmed}` is not accessible: {err}"))?;

        self.ensure_inside_workspace(trimmed, &canonical_candidate)?;

        let metadata = fs::metadata(&canonical_candidate)
            .map_err(|err| format!("failed to stat `{trimmed}`: {err}"))?;
        if !metadata.is_file() {
            return Err(format!("path `{trimmed}` is not a file"));
        }

        Ok(canonical_candidate)
    }

    pub fn resolve_writable_file(&self, raw_path: &str) -> Result<PathBuf, String> {
        let (trimmed, candidate) = self.parse_candidate(raw_path)?;

        if candidate.exists() {
            let canonical_candidate = fs::canonicalize(&candidate)
                .map_err(|err| format!("path `{trimmed}` is not accessible: {err}"))?;
            self.ensure_inside_workspace(trimmed, &canonical_candidate)?;

            let metadata = fs::metadata(&canonical_candidate)
                .map_err(|err| format!("failed to stat `{trimmed}`: {err}"))?;
            if metadata.is_dir() {
                return Err(format!("path `{trimmed}` is a directory"));
            }
            return Ok(canonical_candidate);
        }

        let Some(parent) = candidate.parent() else {
            return Err(format!("path `{trimmed}` has no parent directory"));
        };
        let canonical_parent = fs::canonicalize(parent)
            .map_err(|err| format!("parent directory for `{trimmed}` is not accessible: {err}"))?;
        self.ensure_inside_workspace(trimmed, &canonical_parent)?;
        if !canonical_parent.is_dir() {
            return Err(format!(
                "parent directory for `{trimmed}` is not a directory"
            ));
        }

        Ok(candidate)
    }

    pub fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map(|relative| relative.display().to_string())
            .unwrap_or_else(|_| path.display().to_string())
    }

    fn parse_candidate<'a>(&self, raw_path: &'a str) -> Result<(&'a str, PathBuf), String> {
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
        Ok((trimmed, candidate))
    }

    fn ensure_inside_workspace(&self, raw: &str, canonical_path: &Path) -> Result<(), String> {
        if canonical_path.starts_with(&self.root) {
            Ok(())
        } else {
            Err(format!(
                "path `{raw}` escapes workspace root `{}`",
                self.root.display()
            ))
        }
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

    #[test]
    fn resolves_writable_new_file_inside_workspace() {
        let root = unique_temp_dir("write-inside");
        fs::create_dir_all(root.join("sub")).expect("create sub dir");
        let canonical_root = fs::canonicalize(&root).expect("canonical root");

        let sandbox = WorkspacePathSandbox::new(root.clone());
        let writable = sandbox
            .resolve_writable_file("sub/new.txt")
            .expect("resolve writable file");
        assert!(writable.starts_with(&canonical_root));
        assert!(!writable.exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_writable_path_outside_workspace() {
        let root = unique_temp_dir("write-root");
        let outside_parent = unique_temp_dir("write-outside-parent");
        fs::create_dir_all(&root).expect("create root");
        fs::create_dir_all(&outside_parent).expect("create outside");
        let outside_file = outside_parent.join("outside.txt");

        let sandbox = WorkspacePathSandbox::new(root.clone());
        let err = sandbox
            .resolve_writable_file(&outside_file.to_string_lossy())
            .expect_err("must reject outside path");
        assert!(err.contains("escapes workspace root"));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside_parent);
    }
}
