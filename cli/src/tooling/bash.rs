use std::{
    io::{ErrorKind, Read},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use nabla::tools::{Tool, ToolArgField, ToolArgSchema, ToolArgType};
use serde_json::{Value, json};

use super::path_sandbox::WorkspacePathSandbox;

const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const MAX_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone)]
pub struct BashTool {
    sandbox: WorkspacePathSandbox,
    default_timeout_ms: u64,
    default_max_output_bytes: usize,
}

impl BashTool {
    pub fn for_current_workspace() -> Self {
        let sandbox = WorkspacePathSandbox::for_current_dir()
            .unwrap_or_else(|_| WorkspacePathSandbox::new(std::path::PathBuf::from(".")));
        Self::new(sandbox)
    }

    pub fn new(sandbox: WorkspacePathSandbox) -> Self {
        Self {
            sandbox,
            default_timeout_ms: DEFAULT_TIMEOUT_MS,
            default_max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn schema(&self) -> ToolArgSchema {
        ToolArgSchema::Object {
            fields: vec![
                ToolArgField::required("command", ToolArgType::String),
                ToolArgField {
                    name: "timeout_ms",
                    arg_type: ToolArgType::Number,
                    required: false,
                },
                ToolArgField {
                    name: "max_output_bytes",
                    arg_type: ToolArgType::Number,
                    required: false,
                },
            ],
            allow_unknown_fields: false,
        }
    }

    fn run(&self, args: &Value) -> Result<Value, String> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing `command` argument".to_string())?
            .trim()
            .to_string();
        if command.is_empty() {
            return Err("`command` cannot be empty".to_string());
        }

        let requested_timeout_ms = parse_optional_positive_u64(args, "timeout_ms")?;
        let timeout_ms = requested_timeout_ms
            .unwrap_or(self.default_timeout_ms)
            .min(MAX_TIMEOUT_MS);
        let timeout = Duration::from_millis(timeout_ms);

        let max_output_bytes = parse_optional_positive_u64(args, "max_output_bytes")?
            .map(|v| v as usize)
            .unwrap_or(self.default_max_output_bytes);

        let execution = execute_shell_command(&command, self.sandbox.root(), timeout)?;
        let (stdout, stdout_truncated) = truncate_output_bytes(&execution.stdout, max_output_bytes);
        let (stderr, stderr_truncated) = truncate_output_bytes(&execution.stderr, max_output_bytes);

        Ok(json!({
            "command": command,
            "cwd": self.sandbox.root().display().to_string(),
            "success": execution.status.success() && !execution.timed_out,
            "timed_out": execution.timed_out,
            "exit_code": execution.status.code(),
            "stdout": stdout,
            "stderr": stderr,
            "stdout_bytes": execution.stdout.len(),
            "stderr_bytes": execution.stderr.len(),
            "stdout_truncated": stdout_truncated,
            "stderr_truncated": stderr_truncated,
            "timeout_ms": timeout_ms,
        }))
    }
}

struct CommandExecution {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

fn execute_shell_command(
    command: &str,
    cwd: &Path,
    timeout: Duration,
) -> Result<CommandExecution, String> {
    let mut process = shell_command(command);
    process
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(cwd);

    let mut child = process
        .spawn()
        .map_err(|err| format!("failed to spawn command: {err}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to capture stderr".to_string())?;

    let stdout_reader = thread::spawn(move || read_stream(stdout));
    let stderr_reader = thread::spawn(move || read_stream(stderr));

    let start = Instant::now();
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    if let Err(err) = child.kill() {
                        if err.kind() != ErrorKind::InvalidInput {
                            return Err(format!("failed to kill timed out command: {err}"));
                        }
                    }
                    break true;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(err) => return Err(format!("failed to poll command status: {err}")),
        }
    };

    let status = child
        .wait()
        .map_err(|err| format!("failed to wait for command completion: {err}"))?;
    let stdout = join_reader(stdout_reader, "stdout")?;
    let stderr = join_reader(stderr_reader, "stderr")?;

    Ok(CommandExecution {
        status,
        stdout,
        stderr,
        timed_out,
    })
}

fn join_reader(
    handle: thread::JoinHandle<Result<Vec<u8>, String>>,
    stream_name: &str,
) -> Result<Vec<u8>, String> {
    let joined = handle
        .join()
        .map_err(|_| format!("failed to join {stream_name} reader thread"))?;
    joined.map_err(|err| format!("failed to read {stream_name}: {err}"))
}

fn read_stream<R: Read>(mut reader: R) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|err| format!("read error: {err}"))?;
    Ok(bytes)
}

fn truncate_output_bytes(bytes: &[u8], max_bytes: usize) -> (String, bool) {
    if bytes.len() <= max_bytes {
        return (String::from_utf8_lossy(bytes).to_string(), false);
    }
    (
        String::from_utf8_lossy(&bytes[..max_bytes]).to_string(),
        true,
    )
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

#[cfg(unix)]
fn shell_command(command: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-lc").arg(command);
    cmd
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.arg("/C").arg(command);
    cmd
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use nabla::tools::Tool;
    use serde_json::json;

    use super::BashTool;
    use crate::tooling::path_sandbox::WorkspacePathSandbox;

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agent-cli-bash-tool-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn executes_command_and_captures_stdout() {
        let root = unique_temp_dir("stdout");
        fs::create_dir_all(&root).expect("create root");
        let tool = BashTool::new(WorkspacePathSandbox::new(root.clone()));

        let output = tool
            .run(&json!({ "command": "printf hello" }))
            .expect("bash output");
        assert_eq!(output.get("success").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(output.get("stdout").and_then(|v| v.as_str()), Some("hello"));
        assert_eq!(
            output.get("timed_out").and_then(|v| v.as_bool()),
            Some(false)
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn enforces_timeout() {
        let root = unique_temp_dir("timeout");
        fs::create_dir_all(&root).expect("create root");
        let tool = BashTool::new(WorkspacePathSandbox::new(root.clone()));

        let output = tool
            .run(&json!({
                "command": "sleep 1",
                "timeout_ms": 50
            }))
            .expect("bash output");
        assert_eq!(
            output.get("timed_out").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(output.get("success").and_then(|v| v.as_bool()), Some(false));

        let _ = fs::remove_dir_all(&root);
    }
}
