use std::collections::HashMap;

use serde_json::{Value, json};

use crate::protocol::{ToolCall, ToolResult};

#[derive(Debug, Clone)]
pub enum ToolArgType {
    String,
    Number,
    Boolean,
    Object,
    Array,
    Null,
}

impl ToolArgType {
    fn matches(&self, value: &Value) -> bool {
        match self {
            Self::String => value.is_string(),
            Self::Number => value.is_number(),
            Self::Boolean => value.is_boolean(),
            Self::Object => value.is_object(),
            Self::Array => value.is_array(),
            Self::Null => value.is_null(),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Object => "object",
            Self::Array => "array",
            Self::Null => "null",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolArgField {
    pub name: &'static str,
    pub arg_type: ToolArgType,
    pub required: bool,
}

impl ToolArgField {
    pub fn required(name: &'static str, arg_type: ToolArgType) -> Self {
        Self {
            name,
            arg_type,
            required: true,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ToolArgSchema {
    Any,
    Object {
        fields: Vec<ToolArgField>,
        allow_unknown_fields: bool,
    },
}

impl ToolArgSchema {
    fn validate(&self, args: &Value) -> Result<(), ToolValidationError> {
        match self {
            Self::Any => Ok(()),
            Self::Object {
                fields,
                allow_unknown_fields,
            } => {
                let Some(object) = args.as_object() else {
                    return Err(ToolValidationError {
                        code: "expected_object",
                        path: "$".to_string(),
                        message: "arguments must be a JSON object".to_string(),
                        expected: Some("object".to_string()),
                        actual: Some(value_kind(args).to_string()),
                    });
                };

                for field in fields {
                    match object.get(field.name) {
                        Some(value) => {
                            if !field.arg_type.matches(value) {
                                return Err(ToolValidationError {
                                    code: "type_mismatch",
                                    path: field.name.to_string(),
                                    message: format!(
                                        "field `{}` must be {}",
                                        field.name,
                                        field.arg_type.as_str()
                                    ),
                                    expected: Some(field.arg_type.as_str().to_string()),
                                    actual: Some(value_kind(value).to_string()),
                                });
                            }
                        }
                        None if field.required => {
                            return Err(ToolValidationError {
                                code: "missing_required_field",
                                path: field.name.to_string(),
                                message: format!("missing required field `{}`", field.name),
                                expected: Some(field.arg_type.as_str().to_string()),
                                actual: None,
                            });
                        }
                        None => {}
                    }
                }

                if !allow_unknown_fields {
                    for field_name in object.keys() {
                        if !fields.iter().any(|field| field.name == field_name) {
                            return Err(ToolValidationError {
                                code: "unknown_field",
                                path: field_name.to_string(),
                                message: format!("unknown field `{field_name}`"),
                                expected: None,
                                actual: Some("present".to_string()),
                            });
                        }
                    }
                }

                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ToolValidationError {
    code: &'static str,
    path: String,
    message: String,
    expected: Option<String>,
    actual: Option<String>,
}

impl ToolValidationError {
    fn to_output(&self) -> Value {
        json!({
            "error": {
                "type": "validation_error",
                "code": self.code,
                "path": self.path,
                "message": self.message,
                "expected": self.expected,
                "actual": self.actual
            }
        })
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> ToolArgSchema {
        ToolArgSchema::Any
    }
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

        if let Err(err) = tool.schema().validate(&call.args) {
            return ToolResult {
                call_name: call.name.clone(),
                output: err.to_output(),
                is_error: true,
                message: Some(format!(
                    "invalid arguments for tool `{}`: {}",
                    call.name, err.message
                )),
            };
        }

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

    fn schema(&self) -> ToolArgSchema {
        ToolArgSchema::Object {
            fields: vec![ToolArgField::required("text", ToolArgType::String)],
            allow_unknown_fields: true,
        }
    }

    fn run(&self, args: &Value) -> Result<Value, String> {
        let Some(text) = args.get("text").and_then(Value::as_str) else {
            return Err("missing `text` argument".to_string());
        };

        Ok(json!({ "echo": text }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use serde_json::json;

    use super::{EchoTool, Tool, ToolArgField, ToolArgSchema, ToolArgType, ToolRegistry};
    use crate::protocol::ToolCall;

    #[test]
    fn unknown_tool_behavior_is_stable() {
        let tools = ToolRegistry::default();
        let result = tools.execute(&ToolCall {
            name: "missing".to_string(),
            args: json!({}),
        });

        assert!(result.is_error);
        assert_eq!(result.output, json!(null));
        assert_eq!(result.message, Some("unknown tool: missing".to_string()));
    }

    #[test]
    fn missing_required_field_returns_structured_validation_error() {
        let mut tools = ToolRegistry::default();
        tools.register(EchoTool);

        let result = tools.execute(&ToolCall {
            name: "echo".to_string(),
            args: json!({}),
        });

        assert!(result.is_error);
        assert_eq!(
            result.message,
            Some("invalid arguments for tool `echo`: missing required field `text`".to_string())
        );
        assert_eq!(result.output["error"]["type"], "validation_error");
        assert_eq!(result.output["error"]["code"], "missing_required_field");
        assert_eq!(result.output["error"]["path"], "text");
    }

    #[test]
    fn wrong_type_returns_path_aware_validation_error() {
        let mut tools = ToolRegistry::default();
        tools.register(EchoTool);

        let result = tools.execute(&ToolCall {
            name: "echo".to_string(),
            args: json!({ "text": 123 }),
        });

        assert!(result.is_error);
        assert_eq!(result.output["error"]["type"], "validation_error");
        assert_eq!(result.output["error"]["code"], "type_mismatch");
        assert_eq!(result.output["error"]["path"], "text");
        assert_eq!(result.output["error"]["expected"], "string");
        assert_eq!(result.output["error"]["actual"], "number");
    }

    struct CountingTool {
        runs: Arc<AtomicUsize>,
    }

    impl Tool for CountingTool {
        fn name(&self) -> &str {
            "counting"
        }

        fn schema(&self) -> ToolArgSchema {
            ToolArgSchema::Object {
                fields: vec![ToolArgField::required("value", ToolArgType::String)],
                allow_unknown_fields: true,
            }
        }

        fn run(&self, _args: &serde_json::Value) -> Result<serde_json::Value, String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        }
    }

    #[test]
    fn validation_failure_does_not_invoke_tool_run() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut tools = ToolRegistry::default();
        tools.register(CountingTool { runs: runs.clone() });

        let result = tools.execute(&ToolCall {
            name: "counting".to_string(),
            args: json!({}),
        });

        assert!(result.is_error);
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }
}
