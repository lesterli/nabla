use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use nabla_pdf_rag_core::LlmClient;
use serde_json::Value;

/// Which local CLI tool to use.
#[derive(Debug, Clone)]
pub enum LocalCliTool {
    /// `claude` CLI (Anthropic)
    Claude,
    /// Custom command (e.g., `ollama run llama3`)
    Custom(String),
}

/// LLM client that invokes a local CLI tool (no API key needed).
pub struct LocalCliLlmClient {
    tool: LocalCliTool,
    context_tokens: u32,
}

impl LocalCliLlmClient {
    pub fn new(tool: LocalCliTool, context_tokens: Option<u32>) -> Self {
        Self {
            tool,
            context_tokens: context_tokens.unwrap_or(4096),
        }
    }

    fn invoke(&self, prompt: &str) -> Result<String> {
        let (cmd, args) = match &self.tool {
            LocalCliTool::Claude => ("claude".to_string(), vec!["-p".to_string()]),
            LocalCliTool::Custom(command) => {
                let parts: Vec<&str> = command.split_whitespace().collect();
                let cmd = parts[0].to_string();
                let args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
                (cmd, args)
            }
        };

        let mut child = Command::new(&cmd)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to spawn: {cmd}"))?;

        if let Some(stdin) = child.stdin.as_mut() {
            write!(stdin, "{}", prompt)?;
        }
        drop(child.stdin.take());

        let output = child.wait_with_output().context("Failed to wait for CLI")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("CLI exited with {}: {}", output.status, stderr.trim());
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            bail!("CLI returned empty output");
        }

        Ok(stdout)
    }
}

impl LlmClient for LocalCliLlmClient {
    fn complete(&self, prompt: &str, _max_tokens: u32) -> Result<String> {
        self.invoke(prompt)
    }

    fn complete_json(&self, prompt: &str, _max_tokens: u32) -> Result<Value> {
        let full_prompt = format!(
            "{}\n\nRespond with valid JSON only. No markdown, no explanation.",
            prompt
        );
        let output = self.invoke(&full_prompt)?;

        // Try to extract JSON from the output (handle markdown code blocks, envelopes)
        let json_str = extract_json(&output).unwrap_or(&output);
        serde_json::from_str(json_str)
            .with_context(|| format!("Failed to parse CLI output as JSON: {output}"))
    }

    fn max_context_tokens(&self) -> u32 {
        self.context_tokens
    }
}

/// Extract JSON from a string that might be wrapped in markdown code blocks or envelopes.
fn extract_json(s: &str) -> Option<&str> {
    // Try ```json ... ``` blocks
    if let Some(start) = s.find("```json") {
        let content_start = start + 7;
        if let Some(end) = s[content_start..].find("```") {
            return Some(s[content_start..content_start + end].trim());
        }
    }
    // Try ``` ... ``` blocks
    if let Some(start) = s.find("```") {
        let content_start = start + 3;
        // skip optional language tag on same line
        let line_end = s[content_start..].find('\n').unwrap_or(0);
        let actual_start = content_start + line_end;
        if let Some(end) = s[actual_start..].find("```") {
            return Some(s[actual_start..actual_start + end].trim());
        }
    }
    // Try finding first { ... last }
    let first_brace = s.find('{')?;
    let last_brace = s.rfind('}')?;
    if first_brace < last_brace {
        return Some(&s[first_brace..=last_brace]);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_from_markdown_block() {
        let input = "Here is the result:\n```json\n{\"key\": \"value\"}\n```\nDone.";
        assert_eq!(extract_json(input), Some("{\"key\": \"value\"}"));
    }

    #[test]
    fn extract_json_from_raw_braces() {
        let input = "Some text {\"a\": 1} more text";
        assert_eq!(extract_json(input), Some("{\"a\": 1}"));
    }

    #[test]
    fn extract_json_none_for_no_json() {
        assert_eq!(extract_json("no json here"), None);
    }
}
