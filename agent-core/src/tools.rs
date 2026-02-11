use std::collections::HashMap;

use serde_json::{Value, json};

use crate::protocol::{ToolCall, ToolResult};

pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn run(&self, args: &Value) -> Result<Value, String>;
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        self.tools.insert(tool.name().to_string(), Box::new(tool));
    }

    pub fn execute(&self, call: &ToolCall) -> ToolResult {
        let Some(tool) = self.tools.get(&call.name) else {
            return ToolResult {
                call_name: call.name.clone(),
                output: json!(null),
                is_error: true,
                message: Some(format!("unknown tool: {}", call.name)),
            };
        };

        match tool.run(&call.args) {
            Ok(output) => ToolResult {
                call_name: call.name.clone(),
                output,
                is_error: false,
                message: None,
            },
            Err(err) => ToolResult {
                call_name: call.name.clone(),
                output: json!(null),
                is_error: true,
                message: Some(err),
            },
        }
    }
}

#[derive(Debug, Default)]
pub struct EchoTool;

impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn run(&self, args: &Value) -> Result<Value, String> {
        let Some(text) = args.get("text").and_then(Value::as_str) else {
            return Err("missing `text` argument".to_string());
        };

        Ok(json!({ "echo": text }))
    }
}
