use std::time::Duration;

use anyhow::{bail, Context, Result};
use nabla_pdf_rag_core::LlmClient;
use serde_json::{json, Value};

/// Which API provider to use.
#[derive(Debug, Clone)]
pub enum ApiProvider {
    /// OpenAI-compatible (also works with Kimi, MiniMax, DashScope, etc.)
    OpenAi,
    /// Anthropic Claude API
    Anthropic,
}

/// LLM client that calls an HTTP API (OpenAI-compatible or Anthropic).
pub struct ApiLlmClient {
    client: reqwest::blocking::Client,
    provider: ApiProvider,
    api_key: String,
    model: String,
    base_url: String,
    context_tokens: u32,
}

impl ApiLlmClient {
    pub fn new(
        provider: ApiProvider,
        api_key: impl Into<String>,
        model: Option<String>,
        base_url: Option<String>,
        context_tokens: Option<u32>,
    ) -> Self {
        let (default_model, default_url, default_ctx) = match &provider {
            ApiProvider::OpenAi => (
                "gpt-4o".to_string(),
                "https://api.openai.com/v1".to_string(),
                128_000u32,
            ),
            ApiProvider::Anthropic => (
                "claude-sonnet-4-6".to_string(),
                "https://api.anthropic.com/v1".to_string(),
                200_000u32,
            ),
        };

        Self {
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("failed to build HTTP client"),
            provider,
            api_key: api_key.into(),
            model: model.unwrap_or(default_model),
            base_url: base_url.unwrap_or(default_url),
            context_tokens: context_tokens.unwrap_or(default_ctx),
        }
    }

    fn call_openai(&self, prompt: &str, max_tokens: u32) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = json!({
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": max_tokens,
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .context("HTTP request failed")?;

        let status = resp.status();
        let resp_body: Value = resp.json().context("Failed to parse response JSON")?;

        if !status.is_success() {
            let msg = resp_body["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            bail!("OpenAI API {status}: {msg}");
        }

        resp_body["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Unexpected response structure: {}",
                    serde_json::to_string_pretty(&resp_body).unwrap_or_default()
                )
            })
    }

    fn call_openai_json(&self, prompt: &str, max_tokens: u32) -> Result<Value> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = json!({
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": max_tokens,
            "tools": [{
                "type": "function",
                "function": {
                    "name": "structured_output",
                    "description": "Return the structured result as JSON",
                    "parameters": {
                        "type": "object",
                        "properties": {},
                        "additionalProperties": true,
                    }
                }
            }],
            "tool_choice": {
                "type": "function",
                "function": {"name": "structured_output"}
            }
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .context("HTTP request failed")?;

        let status = resp.status();
        let resp_body: Value = resp.json().context("Failed to parse response JSON")?;

        if !status.is_success() {
            let msg = resp_body["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            bail!("OpenAI API {status}: {msg}");
        }

        // tool_calls[0].function.arguments is a JSON string
        let args_str = resp_body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No tool_calls in response: {}",
                    serde_json::to_string_pretty(&resp_body).unwrap_or_default()
                )
            })?;

        serde_json::from_str(args_str).context("Failed to parse tool_calls arguments as JSON")
    }

    fn call_anthropic(&self, prompt: &str, max_tokens: u32) -> Result<String> {
        let url = format!("{}/messages", self.base_url);
        let body = json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "messages": [{"role": "user", "content": prompt}],
        });

        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .context("HTTP request failed")?;

        let status = resp.status();
        let resp_body: Value = resp.json().context("Failed to parse response JSON")?;

        if !status.is_success() {
            let msg = resp_body["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            bail!("Anthropic API {status}: {msg}");
        }

        // content[0].text
        resp_body["content"][0]["text"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Unexpected response: {}",
                    serde_json::to_string_pretty(&resp_body).unwrap_or_default()
                )
            })
    }

    fn call_anthropic_json(&self, prompt: &str, max_tokens: u32) -> Result<Value> {
        let url = format!("{}/messages", self.base_url);
        let body = json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "messages": [{"role": "user", "content": prompt}],
            "tools": [{
                "name": "structured_output",
                "description": "Return the structured result as JSON",
                "input_schema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": true,
                }
            }],
            "tool_choice": {"type": "tool", "name": "structured_output"},
        });

        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .context("HTTP request failed")?;

        let status = resp.status();
        let resp_body: Value = resp.json().context("Failed to parse response JSON")?;

        if !status.is_success() {
            let msg = resp_body["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            bail!("Anthropic API {status}: {msg}");
        }

        // Find tool_use block in content array
        let content = resp_body["content"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("No content array in response"))?;

        for block in content {
            if block["type"].as_str() == Some("tool_use") {
                return Ok(block["input"].clone());
            }
        }

        bail!(
            "No tool_use block in response: {}",
            serde_json::to_string_pretty(&resp_body).unwrap_or_default()
        )
    }
}

impl LlmClient for ApiLlmClient {
    fn complete(&self, prompt: &str, max_tokens: u32) -> Result<String> {
        match &self.provider {
            ApiProvider::OpenAi => self.call_openai(prompt, max_tokens),
            ApiProvider::Anthropic => self.call_anthropic(prompt, max_tokens),
        }
    }

    fn complete_json(&self, prompt: &str, max_tokens: u32) -> Result<Value> {
        match &self.provider {
            ApiProvider::OpenAi => self.call_openai_json(prompt, max_tokens),
            ApiProvider::Anthropic => self.call_anthropic_json(prompt, max_tokens),
        }
    }

    fn max_context_tokens(&self) -> u32 {
        self.context_tokens
    }
}
