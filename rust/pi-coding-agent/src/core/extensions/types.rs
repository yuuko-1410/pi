//! Extension types and tool definitions, port of
//! `packages/coding-agent/src/core/extensions/types.ts` (runtime subset;
//! the file is mostly TypeScript interfaces for event payloads and
//! contexts — the Rust port keeps the executable surface: tool
//! definitions, handler registration, and the Extension record).

use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_protocol::cbor::Value;

/// Tool definition executed by the extension system (JS `ToolDefinition`).
#[derive(Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Option<Value>,
    /// Execute callback: (tool_call_id, params, state) -> Result.
    pub execute: std::sync::Arc<
        dyn Fn(&str, Value, Option<Value>) -> Result<Value, String> + Send + Sync,
    >,
}

impl std::fmt::Debug for ToolDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolDefinition")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("parameters", &self.parameters)
            .finish_non_exhaustive()
    }
}

impl ToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Option<Value>,
        execute: impl Fn(&str, Value, Option<Value>) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            execute: std::sync::Arc::new(execute),
        }
    }
}

/// Preserve parameter inference for standalone tool definitions (JS
/// `defineTool`); identity in Rust.
pub fn define_tool(tool: ToolDefinition) -> ToolDefinition {
    tool
}

/// Convert a ToolDefinition to an agent-core AgentTool (the agent runtime
/// consumes AgentTool values).
pub fn tool_to_agent_tool(tool: &ToolDefinition) -> AgentTool {
    let tool = tool.clone();
    AgentTool {
        tool: pi_ai::types::Tool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: empty_json_schema(),
            constrained_sampling: None,
        },
        label: tool.name.clone(),
        execute: Some(std::sync::Arc::new(
            move |_tool_call_id, args, _token, _on_update| {
                let result = (tool.execute)(tool_call_id_placeholder(), args.clone(), None);
                match result {
                    Ok(value) => Ok(AgentToolResult {
                        content: vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
                            text: pi_ai::utils::json::json_stringify(&value),
                            text_signature: None,
                        })],
                        details: pi_ai::types::JsonValue::Null,
                        usage: None,
                        added_tool_names: None,
                        terminate: None,
                    }),
                    Err(message) => Err(message),
                }
            },
        )),
        execution_mode: None,
    }
}

fn tool_call_id_placeholder() -> &'static str {
    "tool-call"
}

fn empty_json_schema() -> pi_ai::types::JsonSchemaObject {
    pi_ai::types::JsonSchemaObject {
        type_: None,
        description: None,
        properties: None,
        required: None,
        items: None,
        additional_properties: None,
        all_of: None,
        any_of: None,
        one_of: None,
        enum_values: None,
        default: None,
        const_value: None,
        minimum: None,
        maximum: None,
        exclusive_minimum: None,
        exclusive_maximum: None,
        min_length: None,
        max_length: None,
        pattern: None,
        min_items: None,
        max_items: None,
        unique_items: None,
        min_properties: None,
        max_properties: None,
        not: None,
        nullable: None,
    }
}

/// One registered tool handler within an extension.
#[derive(Clone)]
pub struct RegisteredTool {
    pub definition: ToolDefinition,
    pub hidden: bool,
}

impl std::fmt::Debug for RegisteredTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredTool")
            .field("name", &self.definition.name)
            .field("hidden", &self.hidden)
            .finish()
    }
}

/// A handler function for an extension event.
pub type HandlerFn = std::sync::Arc<dyn Fn(Value) -> Result<Option<Value>, String> + Send + Sync>;

/// One loaded extension (JS `Extension`).
#[derive(Clone, Default)]
pub struct Extension {
    pub path: String,
    pub resolved_path: String,
    pub hidden: Option<bool>,
    pub handlers: std::collections::HashMap<String, Vec<HandlerFn>>,
    pub tools: std::collections::HashMap<String, RegisteredTool>,
    pub commands: std::collections::HashMap<String, RegisteredCommand>,
}

/// Result of loading extensions (JS `LoadExtensionsResult`).
#[derive(Clone, Default)]
pub struct LoadExtensionsResult {
    pub extensions: Vec<Extension>,
    pub errors: Vec<(String, String)>,
}

/// A registered /command (JS `RegisteredCommand`).
#[derive(Clone)]
pub struct RegisteredCommand {
    pub name: String,
    pub description: Option<String>,
    pub handler: HandlerFn,
}

/// Inline extension constructed programmatically (JS `InlineExtension`).
#[derive(Clone, Default)]
pub struct InlineExtension {
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub path: Option<String>,
    pub tools: Vec<ToolDefinition>,
    pub handlers: Vec<(String, HandlerFn)>,
    pub commands: Vec<RegisteredCommand>,
}

impl std::fmt::Debug for InlineExtension {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InlineExtension")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("version", &self.version)
            .field("path", &self.path)
            .field("tools", &self.tools.len())
            .finish()
    }
}

impl std::fmt::Debug for LoadExtensionsResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoadExtensionsResult")
            .field("extensions", &self.extensions.len())
            .field("errors", &self.errors)
            .finish()
    }
}

impl InlineExtension {
    pub fn to_extension(&self) -> Extension {
        let mut extension = Extension {
            path: self.path.clone().unwrap_or_else(|| format!("inline:{}", self.name)),
            resolved_path: self.path.clone().unwrap_or_else(|| format!("inline:{}", self.name)),
            hidden: Some(true),
            ..Extension::default()
        };
        for tool in &self.tools {
            extension.tools.insert(tool.name.clone(), RegisteredTool {
                definition: tool.clone(),
                hidden: false,
            });
        }
        for (event, handler) in &self.handlers {
            extension.handlers.entry(event.clone()).or_default().push(handler.clone());
        }
        for command in &self.commands {
            extension.commands.insert(command.name.clone(), command.clone());
        }
        extension
    }
}

/// Extension error record (JS `ExtensionError`).
#[derive(Clone, Debug, PartialEq)]
pub struct ExtensionError {
    pub extension_path: String,
    pub event: String,
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn define_tool_is_identity() {
        let tool = ToolDefinition::new("t", "desc", None, |_id, _params, _state| Ok(Value::Null));
        let defined = define_tool(tool.clone());
        assert_eq!(defined.name, "t");
        assert_eq!(defined.description, "desc");
    }

    #[test]
    fn tool_executes() {
        let tool = ToolDefinition::new("add", "adds", None, |_id, params, _state| {
            let entries: Vec<(String, Value)> = params.as_map().map(|map| map.to_vec()).unwrap_or_default();
            let a = entries.iter().find(|(k, _)| k == "a").and_then(|(_, v)| v.as_number()).unwrap_or(0.0);
            let b = entries.iter().find(|(k, _)| k == "b").and_then(|(_, v)| v.as_number()).unwrap_or(0.0);
            Ok(Value::Number(a + b))
        });
        let result = (tool.execute)("call-1", Value::Map(vec![
            ("a".to_string(), Value::Number(2.0)),
            ("b".to_string(), Value::Number(3.0)),
        ]), None)
        .unwrap();
        assert_eq!(result.as_number(), Some(5.0));
    }

    #[test]
    fn inline_extension_builds_extension() {
        let inline = InlineExtension {
            name: "test-ext".to_string(),
            description: Some("d".to_string()),
            version: Some("1.0.0".to_string()),
            path: Some("/tmp/ext".to_string()),
            tools: vec![ToolDefinition::new("tool-a", "a", None, |_id, _params, _state| Ok(Value::Null))],
            handlers: vec![(
                "session_start".to_string(),
                std::sync::Arc::new(|_event| Ok(None)),
            )],
            commands: vec![RegisteredCommand {
                name: "cmd".to_string(),
                description: None,
                handler: std::sync::Arc::new(|_event| Ok(None)),
            }],
        };
        let extension = inline.to_extension();
        assert_eq!(extension.path, "/tmp/ext");
        assert!(extension.tools.contains_key("tool-a"));
        assert!(extension.handlers.contains_key("session_start"));
        assert!(extension.commands.contains_key("cmd"));
    }
}
