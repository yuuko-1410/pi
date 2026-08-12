//! Coding-agent harness over pi-agent-core, port of `server/create-harness.ts`.

use std::sync::Arc;

use pi_agent_core::harness::agent_harness::{AgentHarness, AgentHarnessOptions, HarnessTool};
use pi_agent_core::harness::env::nodejs::StdExecutionEnv;
use pi_agent_core::harness::env::types::FileSystem as _;
use pi_agent_core::harness::session::{InMemorySessionStorage, Session};
use pi_agent_core::harness::session_types::SessionMetadata;
use pi_agent_core::types::{AgentTool, AgentToolResult, ToolExecutionMode};
use pi_ai::types::{JsonSchemaObject, Model, Tool};

use crate::core::system_prompt::build_system_prompt;
use crate::core::tools::bash::{bash_tool_parameters, execute_bash_tool, LocalBashOperations};
use crate::core::tools::edit::{edit_tool_parameters, execute_edit, LocalEditOperations};
use crate::core::tools::read::{execute_read, read_tool_parameters, LocalReadOperations};
use crate::core::tools::write::{execute_write, write_tool_parameters, LocalWriteOperations};

/// A tool plus its prompt contribution, mirroring CodingAgentHarnessTool.
pub struct CodingAgentHarnessTool {
    pub tool: AgentTool,
    pub prompt_snippet: Option<String>,
    pub prompt_guidelines: Vec<String>,
}

impl CodingAgentHarnessTool {
    pub fn name(&self) -> &str {
        &self.tool.tool.name
    }
}

pub struct CreateCodingAgentHarnessOptions {
    pub model: Model,
    pub thinking_level: Option<String>,
    pub env: StdExecutionEnv,
    pub bash_command_prefix: Option<String>,
    pub session_file: Option<String>,
    pub session_id: Option<String>,
    pub tools: Option<Vec<CodingAgentHarnessTool>>,
    pub active_tool_names: Option<Vec<String>>,
    pub system_prompt: Option<String>,
    pub system_prompt_options: Option<crate::core::system_prompt::BuildSystemPromptOptions>,
    pub tool_execution: Option<ToolExecutionMode>,
}

/// Convert a JSON Value schema into JsonSchemaObject (subset needed by the
/// default tool parameter schemas).
fn value_to_schema(value: &pi_protocol::Value) -> JsonSchemaObject {
    match value {
        pi_protocol::Value::Map(entries) => {
            let mut properties = Vec::new();
            let mut required = Vec::new();
            for (key, value) in entries {
                match key.as_str() {
                    "type" => {
                        if let Some(t) = value.as_str() {
                            return JsonSchemaObject {
                                type_: Some(vec![t.to_string()]),
                                description: None,
                                ..Default::default()
                            };
                        }
                    }
                    "description" => {
                        if let Some(d) = value.as_str() {
                            return JsonSchemaObject {
                                description: Some(d.to_string()),
                                ..Default::default()
                            };
                        }
                    }
                    "required" => {
                        if let Some(items) = value.as_array() {
                            required = items
                                .iter()
                                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                                .collect();
                        }
                    }
                    _ => {
                        properties.push((key.clone(), value_to_schema(value)));
                    }
                }
            }
            JsonSchemaObject {
                properties: Some(properties),
                required: if required.is_empty() { None } else { Some(required) },
                ..Default::default()
            }
        }
        pi_protocol::Value::Array(items) => JsonSchemaObject {
            items: items
                .first()
                .map(|item| Box::new(pi_ai::types::JsonSchemaValue::Schema(value_to_schema(item)))),
            ..Default::default()
        },
        _ => JsonSchemaObject::default(),
    }
}

fn normalize_snippet(snippet: &str) -> String {
    snippet
        .replace(['\r', '\n'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// Build the system prompt for the harness
/// (buildCodingAgentHarnessSystemPrompt).
pub fn build_coding_agent_harness_system_prompt(
    cwd: &str,
    tools: &[CodingAgentHarnessTool],
    active_tool_names: &[String],
    system_prompt_options: Option<&crate::core::system_prompt::BuildSystemPromptOptions>,
) -> String {
    let active_tools: Vec<&CodingAgentHarnessTool> = active_tool_names
        .iter()
        .filter_map(|name| tools.iter().find(|tool| tool.name() == name))
        .collect();
    let tool_snippets: Vec<(String, String)> = active_tools
        .iter()
        .filter_map(|tool| {
            tool.prompt_snippet
                .as_ref()
                .map(|snippet| (tool.name().to_string(), normalize_snippet(snippet)))
        })
        .collect();
    let prompt_guidelines: Vec<String> = active_tools
        .iter()
        .flat_map(|tool| tool.prompt_guidelines.clone())
        .collect();
    let base = system_prompt_options.map(|options| crate::core::system_prompt::BuildSystemPromptOptions {
        custom_prompt: options.custom_prompt.clone(),
        selected_tools: options.selected_tools.clone(),
        tool_snippets: options.tool_snippets.clone(),
        prompt_guidelines: options.prompt_guidelines.clone(),
        append_system_prompt: options.append_system_prompt.clone(),
        cwd: options.cwd.clone(),
        context_files: options.context_files.clone(),
        skills: options.skills.clone(),
    });
    let mut options = base.unwrap_or_else(|| crate::core::system_prompt::BuildSystemPromptOptions {
        custom_prompt: None,
        selected_tools: None,
        tool_snippets: None,
        prompt_guidelines: None,
        append_system_prompt: None,
        cwd: String::new(),
        context_files: Vec::new(),
        skills: Vec::new(),
    });
    options.cwd = cwd.to_string();
    options.selected_tools = Some(active_tools.iter().map(|tool| tool.name().to_string()).collect());
    options.tool_snippets = Some(tool_snippets);
    options.prompt_guidelines = Some(prompt_guidelines);
    build_system_prompt(&options)
}

/// Create the default coding-agent harness tools (read/bash/edit/write).
pub fn default_harness_tools(
    env: StdExecutionEnv,
    _bash_command_prefix: Option<String>,
    _session_file: Option<String>,
    _session_id: Option<String>,
) -> Vec<CodingAgentHarnessTool> {
    let cwd = env.cwd().to_string();
    let bash_cwd = env.cwd().to_string();
    let edit_cwd = cwd.clone();
    let write_cwd = cwd.clone();
    vec![
        CodingAgentHarnessTool {
            tool: AgentTool {
                tool: Tool {
                    name: "read".to_string(),
                    description: "Read a file from the filesystem".to_string(),
                    parameters: value_to_schema(&read_tool_parameters()),
                    constrained_sampling: None,
                },
                label: "read".to_string(),
                execute: Some(Arc::new(move |_tool_call_id, args, _token, _on_update| {
                    let path = args
                        .as_map()
                        .and_then(|entries| entries.iter().find(|(k, _)| k == "path").and_then(|(_, v)| v.as_str()))
                        .unwrap_or("")
                        .to_string();
                    let offset = args
                        .as_map()
                        .and_then(|entries| entries.iter().find(|(k, _)| k == "offset").and_then(|(_, v)| v.as_number()));
                    let limit = args
                        .as_map()
                        .and_then(|entries| entries.iter().find(|(k, _)| k == "limit").and_then(|(_, v)| v.as_number()));
                    let (content, details) = execute_read(&cwd, &path, offset, limit, &LocalReadOperations)?;
                    let _ = details;
                    Ok(AgentToolResult {
                        content,
                        details: pi_protocol::Value::Null,
                        usage: None,
                        added_tool_names: None,
                        terminate: None,
                    })
                })),
                execution_mode: None,
            },
            prompt_snippet: Some("read <path>".to_string()),
            prompt_guidelines: vec!["Use absolute paths when reading files".to_string()],
        },
        CodingAgentHarnessTool {
            tool: AgentTool {
                tool: Tool {
                    name: "bash".to_string(),
                    description: "Run a bash command".to_string(),
                    parameters: value_to_schema(&bash_tool_parameters()),
                    constrained_sampling: None,
                },
                label: "bash".to_string(),
                execute: Some(Arc::new(move |_tool_call_id, args, _token, _on_update| {
                    let command = args
                        .as_map()
                        .and_then(|entries| entries.iter().find(|(k, _)| k == "command").and_then(|(_, v)| v.as_str()))
                        .unwrap_or("")
                        .to_string();
                    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let (content, details) =
                        execute_bash_tool(&bash_cwd, &command, None, &LocalBashOperations::new(None), &cancelled)?;
                    let _ = details;
                    Ok(AgentToolResult {
                        content,
                        details: pi_protocol::Value::Null,
                        usage: None,
                        added_tool_names: None,
                        terminate: None,
                    })
                })),
                execution_mode: None,
            },
            prompt_snippet: Some("bash <command>".to_string()),
            prompt_guidelines: vec!["Commands run in the project working directory".to_string()],
        },
        CodingAgentHarnessTool {
            tool: AgentTool {
                tool: Tool {
                    name: "edit".to_string(),
                    description: "Edit a file with precise replacements".to_string(),
                    parameters: value_to_schema(&edit_tool_parameters()),
                    constrained_sampling: None,
                },
                label: "edit".to_string(),
                execute: Some(Arc::new(move |_tool_call_id, args, _token, _on_update| {
                    let (content, details) = execute_edit(&edit_cwd, args, &LocalEditOperations)?;
                    let details = details.map(|d| pi_protocol::Value::String(d.diff)).unwrap_or(pi_protocol::Value::Null);
                    Ok(AgentToolResult {
                        content,
                        details,
                        usage: None,
                        added_tool_names: None,
                        terminate: None,
                    })
                })),
                execution_mode: None,
            },
            prompt_snippet: Some("edit <path> with replacements".to_string()),
            prompt_guidelines: vec!["Prefer the smallest precise edit".to_string()],
        },
        CodingAgentHarnessTool {
            tool: AgentTool {
                tool: Tool {
                    name: "write".to_string(),
                    description: "Write content to a file".to_string(),
                    parameters: value_to_schema(&write_tool_parameters()),
                    constrained_sampling: None,
                },
                label: "write".to_string(),
                execute: Some(Arc::new(move |_tool_call_id, args, _token, _on_update| {
                    let path = args
                        .as_map()
                        .and_then(|entries| entries.iter().find(|(k, _)| k == "path").and_then(|(_, v)| v.as_str()))
                        .unwrap_or("")
                        .to_string();
                    let content = args
                        .as_map()
                        .and_then(|entries| entries.iter().find(|(k, _)| k == "content").and_then(|(_, v)| v.as_str()))
                        .unwrap_or("")
                        .to_string();
                    let (content, details) = execute_write(&write_cwd, &path, &content, &LocalWriteOperations)?;
                    Ok(AgentToolResult {
                        content,
                        details: details.unwrap_or(pi_protocol::Value::Null),
                        usage: None,
                        added_tool_names: None,
                        terminate: None,
                    })
                })),
                execution_mode: None,
            },
            prompt_snippet: Some("write <path> <content>".to_string()),
            prompt_guidelines: vec!["Create parent directories when needed".to_string()],
        },
    ]
}

/// Build the harness synchronously (the JS create is async; the sync port
/// materializes tools eagerly and attaches an in-memory session).
pub fn create_coding_agent_harness(
    options: CreateCodingAgentHarnessOptions,
) -> AgentHarness<Session<InMemorySessionStorage>> {
    let harness_cwd = options.env.cwd().to_string();
    let env = options.env;
    let bash_command_prefix = options.bash_command_prefix;
    let session_file = options.session_file;
    let session_id = options.session_id.clone();
    let tools = options.tools.unwrap_or_else(|| {
        default_harness_tools(env, bash_command_prefix, session_file, session_id.clone())
    });
    let active_tool_names = options
        .active_tool_names
        .unwrap_or_else(|| tools.iter().map(|tool| tool.name().to_string()).collect());
    let system_prompt = options.system_prompt.unwrap_or_else(|| {
        build_coding_agent_harness_system_prompt(
            &harness_cwd,
            &tools,
            &active_tool_names,
            options.system_prompt_options.as_ref(),
        )
    });

    let harness_tools: Vec<HarnessTool> = tools
        .into_iter()
        .map(|tool| HarnessTool {
            tool: tool.tool,
            replay: None,
        })
        .collect();

    let storage = InMemorySessionStorage::new(SessionMetadata {
        id: session_id.clone().unwrap_or_else(pi_ai::utils::uuid::uuidv7),
        created_at: crate::core::session_manager::now_iso()
            .parse()
            .unwrap_or(0.0),
        parent_session_id: None,
    })
    .unwrap_or_else(|_| InMemorySessionStorage::new(SessionMetadata {
        id: pi_ai::utils::uuid::uuidv7(),
        created_at: 0.0,
        parent_session_id: None,
    }).unwrap());
    let session = Session::new(storage);

    AgentHarness::new(
        AgentHarnessOptions {
            model: options.model,
            thinking_level: options.thinking_level,
            active_tool_names: Some(active_tool_names),
            tools: Some(harness_tools),
            system_prompt: Some(system_prompt),
            stream_options: None,
            retry: None,
            steering_mode: None,
            follow_up_mode: None,
            tool_execution: options.tool_execution,
        },
        session,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_model() -> Model {
        Model {
            id: "m".to_string(),
            name: "m".to_string(),
            api: "openai".to_string(),
            provider: "acme".to_string(),
            base_url: String::new(),
            reasoning: false,
            thinking_level_map: None,
            input: Vec::new(),
            cost: pi_ai::types::ModelCost {
                rates: pi_ai::types::ModelCostRates {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                tiers: None,
            },
            context_window: 0.0,
            max_tokens: 0.0,
            sampling_params: None,
            headers: None,
            compat: None,
        }
    }

    #[test]
    fn normalizes_snippets() {
        assert_eq!(normalize_snippet("  foo\n\tbar  baz "), "foo bar baz");
    }

    #[test]
    fn builds_system_prompt() {
        let env = StdExecutionEnv::new("/tmp", None, None);
        let tools = default_harness_tools(env, None, None, None);
        let active: Vec<String> = tools.iter().map(|tool| tool.name().to_string()).collect();
        let prompt = build_coding_agent_harness_system_prompt("/tmp", &tools, &active, None);
        assert!(prompt.contains("read"));
        assert!(prompt.contains("bash"));
    }

    #[test]
    fn default_tools_are_four() {
        let env = StdExecutionEnv::new("/tmp", None, None);
        let tools = default_harness_tools(env, None, None, None);
        assert_eq!(tools.len(), 4);
        assert_eq!(tools[0].name(), "read");
        assert_eq!(tools[1].name(), "bash");
    }

    #[test]
    fn read_tool_executes() {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-harness-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("file.txt");
        std::fs::write(&file, "hello").unwrap();
        let env = StdExecutionEnv::new(&dir.to_string_lossy(), None, None);
        let tools = default_harness_tools(env, None, None, None);
        let execute = tools[0].tool.execute.clone().unwrap();
        let args = pi_protocol::Value::Map(vec![("path".to_string(), pi_protocol::Value::String(file.to_string_lossy().to_string()))]);
        let result = execute("call-1", &args, None, None).unwrap();
        assert!(result.content.iter().any(|content| matches!(content, pi_ai::types::Content::Text(text) if text.text.contains("hello"))));
    }
}
