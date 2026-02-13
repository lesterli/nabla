use std::{collections::HashMap, sync::Mutex, thread, time::Duration};

use agent_core::{
    protocol::{Event, ToolCall},
    runtime::{LlmGateway, LlmOutput},
};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const DEFAULT_CONTEXT_MAX_EVENTS: usize = 24;
const DEFAULT_CONTEXT_MAX_CHARS: usize = 4000;
const DEFAULT_EVENT_TEXT_MAX_CHARS: usize = 280;

pub trait ProviderAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn complete(&self, prompt: &str, recent_events: &[Event]) -> Result<ProviderResponse, String>;
}

#[derive(Debug, Clone)]
pub struct ProviderResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub estimated_input_tokens: u32,
    pub estimated_output_tokens: u32,
}

#[derive(Debug, Clone, Default)]
pub struct GatewayStats {
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub retry_attempts: u64,
    pub provider_attempts: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub provider_stats: HashMap<String, ProviderStats>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProviderStats {
    pub successful_requests: u64,
    pub failed_attempts: u64,
    pub attempts: u64,
    pub retry_attempts: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts_per_provider: u32,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts_per_provider: 3,
            initial_backoff_ms: 50,
            max_backoff_ms: 500,
        }
    }
}

#[derive(Default)]
pub struct MultiProviderGateway {
    providers: Vec<Box<dyn ProviderAdapter>>,
    stats: Mutex<GatewayStats>,
    retry_policy: RetryPolicy,
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

    pub fn with_retry_policy(mut self, retry_policy: RetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    pub fn set_retry_policy(&mut self, retry_policy: RetryPolicy) {
        self.retry_policy = retry_policy;
    }

    pub fn stats(&self) -> GatewayStats {
        self.stats
            .lock()
            .expect("llm gateway stats mutex poisoned")
            .clone()
    }
}

impl LlmGateway for MultiProviderGateway {
    fn complete(&self, prompt: &str, recent_events: &[Event]) -> Result<LlmOutput, String> {
        if self.providers.is_empty() {
            let mut stats = self.stats.lock().expect("llm gateway stats mutex poisoned");
            stats.failed_requests += 1;
            return Err("no provider adapters registered".to_string());
        }

        let max_attempts = self.retry_policy.max_attempts_per_provider.max(1);
        let mut provider_failures = Vec::new();

        for provider in &self.providers {
            let mut backoff_ms = self.retry_policy.initial_backoff_ms;
            let mut attempt = 0u32;
            let mut attempt_errors = Vec::new();

            loop {
                attempt += 1;
                {
                    let mut stats = self.stats.lock().expect("llm gateway stats mutex poisoned");
                    stats.provider_attempts += 1;
                    let provider_stats = stats
                        .provider_stats
                        .entry(provider.name().to_string())
                        .or_default();
                    provider_stats.attempts += 1;
                }

                match provider.complete(prompt, recent_events) {
                    Ok(response) => {
                        let mut stats =
                            self.stats.lock().expect("llm gateway stats mutex poisoned");
                        stats.successful_requests += 1;
                        stats.total_input_tokens += u64::from(response.estimated_input_tokens);
                        stats.total_output_tokens += u64::from(response.estimated_output_tokens);
                        if let Some(provider_stats) = stats.provider_stats.get_mut(provider.name())
                        {
                            provider_stats.successful_requests += 1;
                        }

                        return Ok(LlmOutput {
                            text: response.text,
                            tool_calls: response.tool_calls,
                        });
                    }
                    Err(err) => {
                        {
                            let mut stats =
                                self.stats.lock().expect("llm gateway stats mutex poisoned");
                            if let Some(provider_stats) =
                                stats.provider_stats.get_mut(provider.name())
                            {
                                provider_stats.failed_attempts += 1;
                            }
                        }

                        let retryable = is_retryable_error(&err);
                        attempt_errors.push(format!(
                            "attempt {attempt}: retryable={retryable}, error={err}"
                        ));

                        if retryable && attempt < max_attempts {
                            let mut stats =
                                self.stats.lock().expect("llm gateway stats mutex poisoned");
                            stats.retry_attempts += 1;
                            if let Some(provider_stats) =
                                stats.provider_stats.get_mut(provider.name())
                            {
                                provider_stats.retry_attempts += 1;
                            }
                            drop(stats);

                            if backoff_ms > 0 {
                                thread::sleep(Duration::from_millis(backoff_ms));
                            }
                            if self.retry_policy.max_backoff_ms > 0 {
                                backoff_ms = (backoff_ms.saturating_mul(2))
                                    .min(self.retry_policy.max_backoff_ms);
                            }
                            continue;
                        }

                        provider_failures.push(format!(
                            "provider `{}` failed after {} attempt(s): {}",
                            provider.name(),
                            attempt,
                            attempt_errors.join(" | ")
                        ));
                        break;
                    }
                }
            }
        }

        let mut stats = self.stats.lock().expect("llm gateway stats mutex poisoned");
        stats.failed_requests += 1;

        Err(format!(
            "all providers failed; {}",
            provider_failures.join(" ; ")
        ))
    }
}

fn is_retryable_error(err: &str) -> bool {
    let normalized = err.to_ascii_lowercase();
    normalized.contains("request failed")
        || normalized.contains("timeout")
        || normalized.contains("timed out")
        || normalized.contains("connection")
        || normalized.contains("http 429")
        || normalized.contains("http 500")
        || normalized.contains("http 502")
        || normalized.contains("http 503")
        || normalized.contains("http 504")
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

    fn complete(&self, prompt: &str, _recent_events: &[Event]) -> Result<ProviderResponse, String> {
        Ok(ProviderResponse {
            text: format!("{}{}", self.response_prefix, prompt),
            tool_calls: Vec::new(),
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
    tools: Vec<OpenAiFunctionTool>,
    tool_choice: Option<OpenAiToolChoice>,
    context_max_events: usize,
    context_max_chars: usize,
    client: Client,
}

#[derive(Debug, Clone)]
pub struct OpenAiFunctionTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl OpenAiFunctionTool {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

#[derive(Debug, Clone)]
pub enum OpenAiToolChoice {
    Auto,
    Required,
    None,
    Function { name: String },
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
            tools: Vec::new(),
            tool_choice: None,
            context_max_events: DEFAULT_CONTEXT_MAX_EVENTS,
            context_max_chars: DEFAULT_CONTEXT_MAX_CHARS,
            client,
        })
    }

    fn completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    pub fn with_tool(mut self, tool: OpenAiFunctionTool) -> Self {
        self.tools.push(tool);
        self
    }

    pub fn with_tool_choice(mut self, tool_choice: OpenAiToolChoice) -> Self {
        self.tool_choice = Some(tool_choice);
        self
    }

    pub fn with_context_window(mut self, max_events: usize, max_chars: usize) -> Self {
        self.context_max_events = max_events.max(1);
        self.context_max_chars = max_chars.max(64);
        self
    }

    fn build_chat_completions_request(
        &self,
        prompt: &str,
        recent_events: &[Event],
    ) -> OpenAiChatCompletionsRequest {
        let tools = if self.tools.is_empty() {
            None
        } else {
            Some(
                self.tools
                    .iter()
                    .map(|tool| OpenAiRequestTool {
                        kind: "function".to_string(),
                        function: OpenAiRequestToolFunction {
                            name: tool.name.clone(),
                            description: tool.description.clone(),
                            parameters: tool.parameters.clone(),
                        },
                    })
                    .collect(),
            )
        };

        let tool_choice = if tools.is_some() {
            Some(match self.tool_choice.as_ref() {
                Some(OpenAiToolChoice::Required) => Value::String("required".to_string()),
                Some(OpenAiToolChoice::None) => Value::String("none".to_string()),
                Some(OpenAiToolChoice::Function { name }) => serde_json::json!({
                    "type": "function",
                    "function": { "name": name }
                }),
                _ => Value::String("auto".to_string()),
            })
        } else {
            None
        };

        let mut messages = Vec::new();
        if let Some(context_message) = build_recent_context_message(
            recent_events,
            self.context_max_events,
            self.context_max_chars,
        ) {
            messages.push(OpenAiMessage {
                role: "system".to_string(),
                content: context_message,
            });
        }
        messages.push(OpenAiMessage {
            role: "user".to_string(),
            content: prompt.to_string(),
        });

        OpenAiChatCompletionsRequest {
            model: self.model.clone(),
            messages,
            tools,
            tool_choice,
        }
    }
}

impl ProviderAdapter for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn complete(&self, prompt: &str, recent_events: &[Event]) -> Result<ProviderResponse, String> {
        let request = self.build_chat_completions_request(prompt, recent_events);

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

        parse_openai_chat_completions_response(&body, prompt)
    }
}

fn build_recent_context_message(
    recent_events: &[Event],
    max_events: usize,
    max_chars: usize,
) -> Option<String> {
    let summaries: Vec<String> = recent_events
        .iter()
        .filter_map(summarize_event_for_context)
        .collect();

    if summaries.is_empty() {
        return None;
    }

    let tail_start = summaries.len().saturating_sub(max_events.max(1));
    let mut selected = summaries[tail_start..].to_vec();
    trim_lines_to_char_budget(&mut selected, max_chars.max(64));

    Some(format!(
        "Session context from recent events:\n{}",
        selected.join("\n")
    ))
}

fn summarize_event_for_context(event: &Event) -> Option<String> {
    let line = match &event.kind {
        agent_core::protocol::EventKind::UserInput { input } => {
            format!(
                "event#{} user_input: {}",
                event.index,
                truncate_text(input, DEFAULT_EVENT_TEXT_MAX_CHARS)
            )
        }
        agent_core::protocol::EventKind::LlmText { text } => {
            format!(
                "event#{} llm_text: {}",
                event.index,
                truncate_text(text, DEFAULT_EVENT_TEXT_MAX_CHARS)
            )
        }
        agent_core::protocol::EventKind::ToolCallProposed { call } => format!(
            "event#{} tool_call: {} args={}",
            event.index,
            call.name,
            compact_json(&call.args, DEFAULT_EVENT_TEXT_MAX_CHARS)
        ),
        agent_core::protocol::EventKind::ToolExecuted { result } => format!(
            "event#{} tool_executed: {} is_error={} output={}",
            event.index,
            result.call_name,
            result.is_error,
            compact_json(&result.output, DEFAULT_EVENT_TEXT_MAX_CHARS)
        ),
        agent_core::protocol::EventKind::HumanApprovalRequested {
            request_id, reason, ..
        } => format!(
            "event#{} approval_requested: request_id={} reason={}",
            event.index,
            request_id,
            truncate_text(reason, DEFAULT_EVENT_TEXT_MAX_CHARS)
        ),
        agent_core::protocol::EventKind::HumanApprovalResolved {
            request_id,
            approved,
            reason,
        } => format!(
            "event#{} approval_resolved: request_id={} approved={} reason={}",
            event.index,
            request_id,
            approved,
            truncate_text(
                reason.as_deref().unwrap_or(""),
                DEFAULT_EVENT_TEXT_MAX_CHARS
            )
        ),
        agent_core::protocol::EventKind::LlmError { message } => format!(
            "event#{} llm_error: {}",
            event.index,
            truncate_text(message, DEFAULT_EVENT_TEXT_MAX_CHARS)
        ),
        agent_core::protocol::EventKind::TurnStopped { reason } => {
            format!("event#{} turn_stopped: {reason:?}", event.index)
        }
        _ => return None,
    };

    Some(line)
}

fn trim_lines_to_char_budget(lines: &mut Vec<String>, max_chars: usize) {
    while lines.len() > 1 && total_chars(lines) > max_chars {
        lines.remove(0);
    }

    let still_over_budget = total_chars(lines) > max_chars;
    if still_over_budget {
        if let Some(last) = lines.last_mut() {
            *last = truncate_text(last, max_chars.saturating_sub(3));
        }
    }
}

fn total_chars(lines: &[String]) -> usize {
    lines.iter().map(|line| line.chars().count() + 1).sum()
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return "...".to_string();
    }
    let kept: String = text.chars().take(max_chars - 3).collect();
    format!("{kept}...")
}

fn compact_json(value: &Value, max_chars: usize) -> String {
    let serialized =
        serde_json::to_string(value).unwrap_or_else(|_| "<invalid_json_value>".to_string());
    truncate_text(&serialized, max_chars)
}

fn estimate_tokens(text: &str) -> u32 {
    text.split_whitespace().count() as u32
}

fn parse_openai_chat_completions_response(
    body: &str,
    prompt: &str,
) -> Result<ProviderResponse, String> {
    let parsed: OpenAiChatCompletionsResponse =
        serde_json::from_str(body).map_err(|err| format!("invalid provider response: {err}"))?;

    let Some(choice) = parsed.choices.first() else {
        return Err("provider returned no choices".to_string());
    };

    let tool_calls = extract_openai_tool_calls(&choice.message)?;
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
        tool_calls,
        estimated_input_tokens,
        estimated_output_tokens,
    })
}

fn extract_openai_message_text(content: &Value) -> Result<String, String> {
    if content.is_null() {
        return Ok(String::new());
    }

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

fn extract_openai_tool_calls(message: &OpenAiChoiceMessage) -> Result<Vec<ToolCall>, String> {
    let Some(tool_calls) = message.tool_calls.as_ref() else {
        return Ok(Vec::new());
    };

    let mut parsed = Vec::with_capacity(tool_calls.len());
    for (idx, tool_call) in tool_calls.iter().enumerate() {
        if tool_call.kind != "function" {
            return Err(format!(
                "provider returned unsupported tool_call type `{}` at index {idx}",
                tool_call.kind
            ));
        }

        let Some(function) = tool_call.function.as_ref() else {
            return Err(format!(
                "provider returned function tool_call without function payload at index {idx}"
            ));
        };

        if function.name.trim().is_empty() {
            return Err(format!(
                "provider returned function tool_call with empty name at index {idx}"
            ));
        }

        let args = parse_openai_tool_arguments(&function.arguments).map_err(|err| {
            format!(
                "provider returned invalid arguments for tool `{}`: {err}",
                function.name
            )
        })?;

        parsed.push(ToolCall {
            name: function.name.clone(),
            args,
        });
    }

    Ok(parsed)
}

fn parse_openai_tool_arguments(arguments: &Value) -> Result<Value, String> {
    if let Some(arguments_json) = arguments.as_str() {
        return serde_json::from_str(arguments_json)
            .map_err(|err| format!("arguments is not valid JSON: {err}"));
    }

    Ok(arguments.clone())
}

#[derive(Debug, Clone, Serialize)]
struct OpenAiChatCompletionsRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAiRequestTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAiMessage {
    role: String,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAiRequestTool {
    #[serde(rename = "type")]
    kind: String,
    function: OpenAiRequestToolFunction,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAiRequestToolFunction {
    name: String,
    description: String,
    parameters: Value,
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
    tool_calls: Option<Vec<OpenAiMessageToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAiMessageToolCall {
    #[serde(rename = "type")]
    kind: String,
    function: Option<OpenAiFunctionCall>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAiFunctionCall {
    name: String,
    arguments: Value,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{
        MultiProviderGateway, OpenAiCompatibleProvider, OpenAiFunctionTool, OpenAiToolChoice,
        ProviderAdapter, ProviderResponse, RetryPolicy, StaticProvider,
        build_recent_context_message, extract_openai_message_text,
        parse_openai_chat_completions_response,
    };
    use agent_core::{
        memory::{EventStore, InMemoryEventStore},
        policy::AllowAllPolicy,
        protocol::{Event, EventKind, Op, StopReason, ToolCall},
        runtime::{AgentRuntime, LlmGateway},
        tools::{EchoTool, ToolRegistry},
    };
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

    #[test]
    fn parses_tool_calls_from_openai_response_body() {
        let body = json!({
            "choices": [
                {
                    "message": {
                        "content": null,
                        "tool_calls": [
                            {
                                "id": "call_123",
                                "type": "function",
                                "function": {
                                    "name": "echo",
                                    "arguments": "{\"text\":\"hello\"}"
                                }
                            }
                        ]
                    }
                }
            ],
            "usage": {
                "prompt_tokens": 3,
                "completion_tokens": 5
            }
        })
        .to_string();

        let response =
            parse_openai_chat_completions_response(&body, "say hello").expect("parse provider");

        assert_eq!(response.text, "");
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "echo");
        assert_eq!(response.tool_calls[0].args, json!({ "text": "hello" }));
    }

    #[test]
    fn returns_parse_error_for_malformed_tool_call_arguments() {
        let body = json!({
            "choices": [
                {
                    "message": {
                        "content": "",
                        "tool_calls": [
                            {
                                "type": "function",
                                "function": {
                                    "name": "echo",
                                    "arguments": "{not-json}"
                                }
                            }
                        ]
                    }
                }
            ]
        })
        .to_string();

        let err =
            parse_openai_chat_completions_response(&body, "hi").expect_err("expected parse error");
        assert!(err.contains("invalid arguments for tool `echo`"));
    }

    #[test]
    fn request_payload_includes_tools_and_tool_choice() {
        let provider = OpenAiCompatibleProvider::new(
            "openai-compatible",
            "https://api.openai.com/v1",
            "test-key",
            "gpt-4o-mini",
        )
        .expect("provider should build")
        .with_tool(OpenAiFunctionTool::new(
            "echo",
            "Echo input text",
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"]
            }),
        ))
        .with_tool_choice(OpenAiToolChoice::Required);

        let request = provider.build_chat_completions_request("say hi", &[]);
        let payload = serde_json::to_value(&request).expect("serialize request");

        assert_eq!(payload["tools"][0]["type"], "function");
        assert_eq!(payload["tools"][0]["function"]["name"], "echo");
        assert_eq!(payload["tool_choice"], "required");
    }

    #[test]
    fn request_payload_omits_tools_and_tool_choice_when_no_tools_configured() {
        let provider = OpenAiCompatibleProvider::new(
            "openai-compatible",
            "https://api.openai.com/v1",
            "test-key",
            "gpt-4o-mini",
        )
        .expect("provider should build")
        .with_tool_choice(OpenAiToolChoice::Required);

        let request = provider.build_chat_completions_request("say hi", &[]);
        let payload = serde_json::to_value(&request).expect("serialize request");

        assert!(payload.get("tools").is_none());
        assert!(payload.get("tool_choice").is_none());
    }

    #[test]
    fn request_payload_contains_serialized_recent_context() {
        let provider = OpenAiCompatibleProvider::new(
            "openai-compatible",
            "https://api.openai.com/v1",
            "test-key",
            "gpt-4o-mini",
        )
        .expect("provider should build")
        .with_context_window(8, 500);

        let recent_events = vec![
            Event::new(
                "sub-ctx".to_string(),
                1,
                EventKind::UserInput {
                    input: "analyze file src/main.rs".to_string(),
                },
            ),
            Event::new(
                "sub-ctx".to_string(),
                2,
                EventKind::ToolExecuted {
                    result: agent_core::protocol::ToolResult {
                        call_name: "echo".to_string(),
                        output: json!({ "echo": "tool output line" }),
                        is_error: false,
                        message: None,
                    },
                },
            ),
        ];

        let request = provider.build_chat_completions_request("continue", &recent_events);
        let payload = serde_json::to_value(&request).expect("serialize request");

        assert_eq!(payload["messages"][0]["role"], "system");
        let context = payload["messages"][0]["content"]
            .as_str()
            .expect("context content should be string");
        assert!(context.contains("Session context from recent events"));
        assert!(context.contains("user_input"));
        assert!(context.contains("tool_executed"));
        assert_eq!(payload["messages"][1]["role"], "user");
        assert_eq!(payload["messages"][1]["content"], "continue");
    }

    #[test]
    fn recent_context_truncation_is_deterministic() {
        let events = vec![
            Event::new(
                "sub-trunc".to_string(),
                1,
                EventKind::UserInput {
                    input: "older context".to_string(),
                },
            ),
            Event::new(
                "sub-trunc".to_string(),
                2,
                EventKind::LlmText {
                    text: "middle context".to_string(),
                },
            ),
            Event::new(
                "sub-trunc".to_string(),
                3,
                EventKind::ToolExecuted {
                    result: agent_core::protocol::ToolResult {
                        call_name: "echo".to_string(),
                        output: json!({ "echo": "newest context should survive" }),
                        is_error: false,
                        message: None,
                    },
                },
            ),
        ];

        let context =
            build_recent_context_message(&events, 3, 120).expect("context should be generated");
        assert!(context.contains("event#3"));
        assert!(
            !context.contains("event#1"),
            "oldest line should be truncated first"
        );
    }

    #[derive(Debug)]
    struct FixtureParsedProvider {
        first_body: String,
        next_body: String,
        call_count: Mutex<usize>,
    }

    impl ProviderAdapter for FixtureParsedProvider {
        fn name(&self) -> &str {
            "fixture-parsed"
        }

        fn complete(
            &self,
            prompt: &str,
            _recent_events: &[Event],
        ) -> Result<ProviderResponse, String> {
            let mut call_count = self
                .call_count
                .lock()
                .expect("fixture parsed provider mutex poisoned");
            let body = if *call_count == 0 {
                &self.first_body
            } else {
                &self.next_body
            };
            *call_count += 1;
            parse_openai_chat_completions_response(body, prompt)
        }
    }

    #[test]
    fn runtime_executes_tool_call_parsed_from_provider_output() {
        let provider_body = json!({
            "choices": [
                {
                    "message": {
                        "content": "calling echo",
                        "tool_calls": [
                            {
                                "type": "function",
                                "function": {
                                    "name": "echo",
                                    "arguments": "{\"text\":\"from-provider\"}"
                                }
                            }
                        ]
                    }
                }
            ],
            "usage": {
                "prompt_tokens": 2,
                "completion_tokens": 4
            }
        })
        .to_string();

        let done_body = json!({
            "choices": [
                {
                    "message": {
                        "content": "done",
                        "tool_calls": []
                    }
                }
            ],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1
            }
        })
        .to_string();

        let llm = MultiProviderGateway::new().with_provider(FixtureParsedProvider {
            first_body: provider_body,
            next_body: done_body,
            call_count: Mutex::new(0),
        });
        let mut runtime = AgentRuntime::default();
        let mut tools = ToolRegistry::default();
        tools.register(EchoTool);
        let mut store = InMemoryEventStore::default();

        let result = runtime.run_turn(
            Op::UserInput {
                submission_id: "sub-tool-integration".to_string(),
                input: "please call echo".to_string(),
            },
            &llm,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );

        assert_eq!(result.stop_reason, StopReason::Done);
        let executed = store.events().iter().any(|event| {
            matches!(
                event.kind,
                EventKind::ToolExecuted { ref result }
                    if result.call_name == "echo" && !result.is_error
            )
        });
        assert!(executed, "expected runtime to execute parsed tool call");
    }

    #[derive(Debug)]
    struct ResumeContextProvider {
        call_count: Mutex<usize>,
    }

    impl ProviderAdapter for ResumeContextProvider {
        fn name(&self) -> &str {
            "resume-context"
        }

        fn complete(
            &self,
            _prompt: &str,
            recent_events: &[Event],
        ) -> Result<ProviderResponse, String> {
            let mut call_count = self
                .call_count
                .lock()
                .expect("resume context provider mutex poisoned");
            let current_call = *call_count;
            *call_count += 1;

            if current_call == 0 {
                return Ok(ProviderResponse {
                    text: "call tool".to_string(),
                    tool_calls: vec![ToolCall {
                        name: "echo".to_string(),
                        args: json!({ "text": "from-tool" }),
                    }],
                    estimated_input_tokens: 2,
                    estimated_output_tokens: 3,
                });
            }

            if current_call == 1 {
                return Ok(ProviderResponse {
                    text: "first turn done".to_string(),
                    tool_calls: Vec::new(),
                    estimated_input_tokens: 1,
                    estimated_output_tokens: 2,
                });
            }

            let saw_prior_tool_output = recent_events.iter().any(|event| {
                matches!(
                    event.kind,
                    EventKind::ToolExecuted { ref result }
                        if result.call_name == "echo"
                            && result.output == json!({ "echo": "from-tool" })
                            && !result.is_error
                )
            });
            if !saw_prior_tool_output {
                return Err("resume call did not receive prior tool output context".to_string());
            }

            Ok(ProviderResponse {
                text: "resume saw prior tool output".to_string(),
                tool_calls: Vec::new(),
                estimated_input_tokens: 1,
                estimated_output_tokens: 3,
            })
        }
    }

    #[test]
    fn run_then_resume_receives_prior_tool_output_context() {
        let llm = MultiProviderGateway::new().with_provider(ResumeContextProvider {
            call_count: Mutex::new(0),
        });
        let mut runtime = AgentRuntime::default();
        let mut tools = ToolRegistry::default();
        tools.register(EchoTool);
        let mut store = InMemoryEventStore::default();

        let first = runtime.run_turn(
            Op::UserInput {
                submission_id: "sub-resume-ctx".to_string(),
                input: "start".to_string(),
            },
            &llm,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );
        assert_eq!(first.stop_reason, StopReason::Done);

        let mut resumed_runtime = AgentRuntime::default();
        let second = resumed_runtime.run_turn(
            Op::Resume {
                submission_id: "sub-resume-ctx".to_string(),
                checkpoint_id: None,
            },
            &llm,
            &AllowAllPolicy,
            &tools,
            &mut store,
        );
        assert_eq!(second.stop_reason, StopReason::Done);
        assert!(second.events.iter().any(|event| {
            matches!(
                event.kind,
                EventKind::LlmText { ref text } if text == "resume saw prior tool output"
            )
        }));
    }

    #[derive(Debug)]
    struct FlakyRetryableProvider {
        name: String,
        failures_before_success: usize,
        attempts: Arc<Mutex<usize>>,
    }

    impl ProviderAdapter for FlakyRetryableProvider {
        fn name(&self) -> &str {
            &self.name
        }

        fn complete(
            &self,
            prompt: &str,
            _recent_events: &[Event],
        ) -> Result<ProviderResponse, String> {
            let mut attempts = self
                .attempts
                .lock()
                .expect("flaky provider attempts mutex poisoned");
            *attempts += 1;
            if *attempts <= self.failures_before_success {
                return Err("HTTP 503 upstream unavailable".to_string());
            }

            Ok(ProviderResponse {
                text: format!("ok: {prompt}"),
                tool_calls: Vec::new(),
                estimated_input_tokens: 2,
                estimated_output_tokens: 3,
            })
        }
    }

    #[derive(Debug)]
    struct AlwaysFailProvider {
        name: String,
        error_message: String,
    }

    impl ProviderAdapter for AlwaysFailProvider {
        fn name(&self) -> &str {
            &self.name
        }

        fn complete(
            &self,
            _prompt: &str,
            _recent_events: &[Event],
        ) -> Result<ProviderResponse, String> {
            Err(self.error_message.clone())
        }
    }

    #[test]
    fn flaky_provider_succeeds_after_retry_and_metrics_reflect_retries() {
        let attempts = Arc::new(Mutex::new(0usize));
        let gateway = MultiProviderGateway::new()
            .with_retry_policy(RetryPolicy {
                max_attempts_per_provider: 3,
                initial_backoff_ms: 0,
                max_backoff_ms: 0,
            })
            .with_provider(FlakyRetryableProvider {
                name: "flaky-a".to_string(),
                failures_before_success: 2,
                attempts: attempts.clone(),
            });

        let response = gateway
            .complete("retry me", &Vec::<Event>::new())
            .expect("gateway should eventually succeed");
        assert_eq!(response.text, "ok: retry me");
        assert_eq!(
            *attempts.lock().expect("attempts mutex poisoned"),
            3,
            "provider should be attempted until success",
        );

        let stats = gateway.stats();
        assert_eq!(stats.successful_requests, 1);
        assert_eq!(stats.failed_requests, 0);
        assert_eq!(stats.retry_attempts, 2);
        assert_eq!(stats.provider_attempts, 3);
        let flaky_stats = stats
            .provider_stats
            .get("flaky-a")
            .expect("provider stats should include flaky-a");
        assert_eq!(flaky_stats.attempts, 3);
        assert_eq!(flaky_stats.failed_attempts, 2);
        assert_eq!(flaky_stats.retry_attempts, 2);
        assert_eq!(flaky_stats.successful_requests, 1);
    }

    #[test]
    fn all_provider_failure_returns_aggregated_diagnostic_context() {
        let gateway = MultiProviderGateway::new()
            .with_retry_policy(RetryPolicy {
                max_attempts_per_provider: 2,
                initial_backoff_ms: 0,
                max_backoff_ms: 0,
            })
            .with_provider(AlwaysFailProvider {
                name: "provider-a".to_string(),
                error_message: "HTTP 503 upstream unavailable".to_string(),
            })
            .with_provider(AlwaysFailProvider {
                name: "provider-b".to_string(),
                error_message: "invalid provider response: malformed json".to_string(),
            });

        let err = gateway
            .complete("hello", &Vec::<Event>::new())
            .expect_err("all providers should fail");

        assert!(err.contains("all providers failed"));
        assert!(err.contains("provider `provider-a` failed after 2 attempt(s)"));
        assert!(err.contains("provider `provider-b` failed after 1 attempt(s)"));
        assert!(err.contains("retryable=true"));
        assert!(err.contains("retryable=false"));

        let stats = gateway.stats();
        assert_eq!(stats.successful_requests, 0);
        assert_eq!(stats.failed_requests, 1);
        assert_eq!(stats.retry_attempts, 1);
        assert_eq!(stats.provider_attempts, 3);
        let provider_a_stats = stats
            .provider_stats
            .get("provider-a")
            .expect("provider-a stats should be present");
        assert_eq!(provider_a_stats.attempts, 2);
        let provider_b_stats = stats
            .provider_stats
            .get("provider-b")
            .expect("provider-b stats should be present");
        assert_eq!(provider_b_stats.attempts, 1);
    }
}
