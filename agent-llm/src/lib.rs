use std::sync::Mutex;

use agent_core::{
    protocol::Event,
    runtime::{LlmGateway, LlmOutput},
};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub trait ProviderAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn complete(&self, prompt: &str) -> Result<ProviderResponse, String>;
}

#[derive(Debug, Clone)]
pub struct ProviderResponse {
    pub text: String,
    pub estimated_input_tokens: u32,
    pub estimated_output_tokens: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GatewayStats {
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

#[derive(Default)]
pub struct MultiProviderGateway {
    providers: Vec<Box<dyn ProviderAdapter>>,
    stats: Mutex<GatewayStats>,
}

impl MultiProviderGateway {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_provider<P: ProviderAdapter + 'static>(mut self, provider: P) -> Self {
        self.providers.push(Box::new(provider));
        self
    }

    pub fn register_provider<P: ProviderAdapter + 'static>(&mut self, provider: P) {
        self.providers.push(Box::new(provider));
    }

    pub fn stats(&self) -> GatewayStats {
        *self.stats.lock().expect("llm gateway stats mutex poisoned")
    }
}

impl LlmGateway for MultiProviderGateway {
    fn complete(&self, prompt: &str, _recent_events: &[Event]) -> Result<LlmOutput, String> {
        if self.providers.is_empty() {
            let mut stats = self.stats.lock().expect("llm gateway stats mutex poisoned");
            stats.failed_requests += 1;
            return Err("no provider adapters registered".to_string());
        }

        let mut last_error = "all providers failed".to_string();

        for provider in &self.providers {
            match provider.complete(prompt) {
                Ok(response) => {
                    let mut stats = self.stats.lock().expect("llm gateway stats mutex poisoned");
                    stats.successful_requests += 1;
                    stats.total_input_tokens += u64::from(response.estimated_input_tokens);
                    stats.total_output_tokens += u64::from(response.estimated_output_tokens);

                    return Ok(LlmOutput {
                        text: response.text,
                        tool_calls: Vec::new(),
                    });
                }
                Err(err) => {
                    last_error = format!("{}: {}", provider.name(), err);
                }
            }
        }

        let mut stats = self.stats.lock().expect("llm gateway stats mutex poisoned");
        stats.failed_requests += 1;

        Err(last_error)
    }
}

#[derive(Debug, Clone)]
pub struct StaticProvider {
    name: String,
    response_prefix: String,
}

impl StaticProvider {
    pub fn new(name: impl Into<String>, response_prefix: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            response_prefix: response_prefix.into(),
        }
    }
}

impl ProviderAdapter for StaticProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn complete(&self, prompt: &str) -> Result<ProviderResponse, String> {
        Ok(ProviderResponse {
            text: format!("{}{}", self.response_prefix, prompt),
            estimated_input_tokens: prompt.split_whitespace().count() as u32,
            estimated_output_tokens: 8,
        })
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatibleProvider {
    name: String,
    base_url: String,
    api_key: String,
    model: String,
    client: Client,
}

impl OpenAiCompatibleProvider {
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, String> {
        let client = Client::builder()
            .build()
            .map_err(|err| format!("failed to build HTTP client: {err}"))?;

        Ok(Self {
            name: name.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
            client,
        })
    }

    fn completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }
}

impl ProviderAdapter for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn complete(&self, prompt: &str) -> Result<ProviderResponse, String> {
        let request = OpenAiChatCompletionsRequest {
            model: self.model.clone(),
            messages: vec![OpenAiMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
            }],
        };

        let response = self
            .client
            .post(self.completions_url())
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .map_err(|err| format!("request failed: {err}"))?;

        let status = response.status();
        let body = response
            .text()
            .map_err(|err| format!("failed to read response body: {err}"))?;

        if !status.is_success() {
            return Err(format!("HTTP {status}: {body}"));
        }

        let parsed: OpenAiChatCompletionsResponse = serde_json::from_str(&body)
            .map_err(|err| format!("invalid provider response: {err}"))?;

        let Some(choice) = parsed.choices.first() else {
            return Err("provider returned no choices".to_string());
        };

        let text = extract_openai_message_text(&choice.message.content)?;
        let estimated_input_tokens = parsed
            .usage
            .as_ref()
            .map_or_else(|| estimate_tokens(prompt), |usage| usage.prompt_tokens);
        let estimated_output_tokens = parsed
            .usage
            .as_ref()
            .map_or_else(|| estimate_tokens(&text), |usage| usage.completion_tokens);

        Ok(ProviderResponse {
            text,
            estimated_input_tokens,
            estimated_output_tokens,
        })
    }
}

fn estimate_tokens(text: &str) -> u32 {
    text.split_whitespace().count() as u32
}

fn extract_openai_message_text(content: &Value) -> Result<String, String> {
    if let Some(text) = content.as_str() {
        return Ok(text.to_string());
    }

    if let Some(parts) = content.as_array() {
        let joined = parts
            .iter()
            .filter_map(|part| part.get("text"))
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("");
        if joined.is_empty() {
            return Err("provider returned structured content without text parts".to_string());
        }
        return Ok(joined);
    }

    Err("provider returned unsupported message content shape".to_string())
}

#[derive(Debug, Clone, Serialize)]
struct OpenAiChatCompletionsRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAiMessage {
    role: String,
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAiChatCompletionsResponse {
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAiChoice {
    message: OpenAiChoiceMessage,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAiChoiceMessage {
    content: Value,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::{MultiProviderGateway, StaticProvider, extract_openai_message_text};
    use agent_core::{protocol::Event, runtime::LlmGateway};
    use serde_json::json;

    #[test]
    fn gateway_tracks_usage_for_successful_request() {
        let gateway =
            MultiProviderGateway::new().with_provider(StaticProvider::new("static", "assistant: "));

        let response = gateway
            .complete("hello world", &Vec::<Event>::new())
            .expect("gateway should respond");

        assert!(response.text.starts_with("assistant: "));

        let stats = gateway.stats();
        assert_eq!(stats.successful_requests, 1);
        assert_eq!(stats.failed_requests, 0);
        assert!(stats.total_input_tokens >= 2);
    }

    #[test]
    fn extracts_text_from_string_content() {
        let text = extract_openai_message_text(&json!("hello")).expect("extract text");
        assert_eq!(text, "hello");
    }

    #[test]
    fn extracts_text_from_structured_content() {
        let text = extract_openai_message_text(&json!([
            { "type": "text", "text": "hello " },
            { "type": "text", "text": "world" }
        ]))
        .expect("extract text");
        assert_eq!(text, "hello world");
    }
}
