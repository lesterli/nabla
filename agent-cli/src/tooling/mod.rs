use std::collections::HashSet;

use agent_core::tools::ToolRegistry;
use agent_llm::OpenAiFunctionTool;

pub mod bash;
pub mod edit;
pub mod grep;
pub mod path_sandbox;
pub mod read;
pub mod registry;
pub mod write;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolingCliConfig {
    pub no_tools: bool,
    pub tools: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct ToolingSelection {
    tools: Vec<&'static registry::ToolSpec>,
}

impl ToolingSelection {
    pub fn empty() -> Self {
        Self { tools: Vec::new() }
    }

    pub fn enabled_tool_names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|tool| tool.name).collect()
    }

    pub fn contains_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool.name == name)
    }

    pub fn register_local_tools(&self, registry: &mut ToolRegistry) {
        for tool in &self.tools {
            tool.register_local(registry);
        }
    }

    pub fn provider_tools(&self) -> Vec<OpenAiFunctionTool> {
        self.tools
            .iter()
            .filter_map(|tool| tool.provider_definition())
            .collect()
    }
}

pub fn resolve_tooling_from_cli(config: &ToolingCliConfig) -> Result<ToolingSelection, String> {
    let tool_names = if let Some(explicit_tools) = &config.tools {
        explicit_tools.iter().map(|name| name.as_str()).collect()
    } else if config.no_tools {
        Vec::new()
    } else {
        registry::default_tool_names().to_vec()
    };

    let mut seen = HashSet::new();
    let mut selected = Vec::new();

    for raw_name in tool_names {
        let name = raw_name.trim();
        if name.is_empty() || !seen.insert(name) {
            continue;
        }

        let Some(tool) = registry::find_tool(name) else {
            return Err(format!(
                "unsupported tool `{name}` (supported: {})",
                registry::catalog()
                    .iter()
                    .map(|tool| tool.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };

        selected.push(tool);
    }

    Ok(ToolingSelection { tools: selected })
}

#[cfg(test)]
mod tests {
    use super::{ToolingCliConfig, resolve_tooling_from_cli};

    #[test]
    fn defaults_to_builtin_tools() {
        let selection = resolve_tooling_from_cli(&ToolingCliConfig::default()).expect("resolve");
        assert_eq!(selection.enabled_tool_names(), vec!["read"]);
    }

    #[test]
    fn no_tools_disables_all_tools() {
        let selection = resolve_tooling_from_cli(&ToolingCliConfig {
            no_tools: true,
            tools: None,
        })
        .expect("resolve");
        assert!(selection.enabled_tool_names().is_empty());
    }

    #[test]
    fn explicit_tools_override_defaults() {
        let selection = resolve_tooling_from_cli(&ToolingCliConfig {
            no_tools: false,
            tools: Some(vec!["read".to_string()]),
        })
        .expect("resolve");
        assert_eq!(selection.enabled_tool_names(), vec!["read"]);
    }

    #[test]
    fn no_tools_plus_explicit_tools_keeps_explicit_selection() {
        let selection = resolve_tooling_from_cli(&ToolingCliConfig {
            no_tools: true,
            tools: Some(vec![
                "read".to_string(),
                "write".to_string(),
                "edit".to_string(),
                "bash".to_string(),
            ]),
        })
        .expect("resolve");
        assert_eq!(
            selection.enabled_tool_names(),
            vec!["read", "write", "edit", "bash"]
        );
    }

    #[test]
    fn rejects_unknown_tool_name() {
        let err = resolve_tooling_from_cli(&ToolingCliConfig {
            no_tools: false,
            tools: Some(vec!["missing".to_string()]),
        })
        .expect_err("must fail");
        assert!(err.contains("unsupported tool `missing`"));
    }
}
