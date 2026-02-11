use std::sync::Mutex;

use agent_core::{
    protocol::Event,
    runtime::{LlmGateway, LlmOutput},
};

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

#[cfg(test)]
mod tests {
    use super::{MultiProviderGateway, StaticProvider};
    use agent_core::{protocol::Event, runtime::LlmGateway};

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
}
