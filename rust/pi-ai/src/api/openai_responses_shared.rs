//! OpenAI Responses API message/tool conversion, port of
//! `packages/ai/src/api/openai-responses-shared.ts` (the pure-conversion
//! parts; stream processing follows with the HTTP layer).

use pi_protocol::Value;

use crate::api::constrained_sampling::{
    get_grammar_tool_input, resolve_grammar_constrained_sampling, resolve_json_schema_strict_sampling,
};
use crate::api::transform_messages::transform_messages;
use crate::types::{
    AssistantMessage, Content, Context, Model, Tool, Usage,
};
use crate::utils::hash::short_hash;

// ---------------------------------------------------------------------------
// Response input/output item types (OpenAI Responses API shapes)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct ResponseInputText {
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResponseInputImage {
    pub detail: String,
    pub image_url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResponseInputContent {
    InputText(ResponseInputText),
    InputImage(ResponseInputImage),
}

impl ResponseInputContent {
    pub fn to_value(&self) -> Value {
        match self {
            Self::InputText(text) => Value::Map(vec![
                ("type".to_string(), Value::String("input_text".to_string())),
                ("text".to_string(), Value::String(text.text.clone())),
            ]),
            Self::InputImage(image) => Value::Map(vec![
                ("type".to_string(), Value::String("input_image".to_string())),
                ("detail".to_string(), Value::String(image.detail.clone())),
                ("image_url".to_string(), Value::String(image.image_url.clone())),
            ]),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResponseOutputText {
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResponseOutputMessage {
    pub id: String,
    pub content: Vec<ResponseOutputText>,
    pub phase: Option<String>,
}

/// Parsed reasoning item carried through from a thinking signature.
pub type ResponseReasoningItem = Value;

#[derive(Clone, Debug, PartialEq)]
pub enum ResponseInputItem {
    /// System/developer prompt entry.
    Developer {
        role: String,
        content: Vec<ResponseInputText>,
    },
    User {
        content: Vec<ResponseInputContent>,
    },
    AssistantMessage(ResponseOutputMessage),
    Reasoning(ResponseReasoningItem),
    FunctionCall {
        id: Option<String>,
        call_id: String,
        name: String,
        arguments: String,
        namespace: Option<String>,
    },
    CustomToolCall {
        id: Option<String>,
        call_id: String,
        name: String,
        input: String,
        namespace: Option<String>,
    },
    FunctionCallOutput {
        call_id: String,
        output: ResponseToolResultOutput,
    },
    CustomToolCallOutput {
        call_id: String,
        output: ResponseToolResultOutput,
    },
    AdditionalTools {
        tools: Vec<OpenAITool>,
    },
    ToolSearchCall {
        call_id: String,
        arguments: Value,
    },
    ToolSearchOutput {
        call_id: String,
        tools: Vec<OpenAITool>,
    },
}

impl ResponseInputItem {
    pub fn to_value(&self) -> Value {
        match self {
            Self::Developer { role, content } => Value::Map(vec![
                ("role".to_string(), Value::String(role.clone())),
                (
                    "content".to_string(),
                    Value::Array(
                        content
                            .iter()
                            .map(|text| Value::Map(vec![("text".to_string(), Value::String(text.text.clone()))]))
                            .collect(),
                    ),
                ),
            ]),
            Self::User { content } => Value::Map(vec![
                ("role".to_string(), Value::String("user".to_string())),
                (
                    "content".to_string(),
                    Value::Array(content.iter().map(|block| block.to_value()).collect()),
                ),
            ]),
            Self::AssistantMessage(message) => {
                let mut entries = vec![
                    ("type".to_string(), Value::String("message".to_string())),
                    ("role".to_string(), Value::String("assistant".to_string())),
                    (
                        "content".to_string(),
                        Value::Array(
                            message
                                .content
                                .iter()
                                .map(|text| {
                                    Value::Map(vec![
                                        ("type".to_string(), Value::String("output_text".to_string())),
                                        ("text".to_string(), Value::String(text.text.clone())),
                                        ("annotations".to_string(), Value::Array(Vec::new())),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    ("status".to_string(), Value::String("completed".to_string())),
                    ("id".to_string(), Value::String(message.id.clone())),
                ];
                if let Some(phase) = &message.phase {
                    entries.push(("phase".to_string(), Value::String(phase.clone())));
                }
                Value::Map(entries)
            }
            Self::Reasoning(item) => item.clone(),
            Self::FunctionCall {
                id,
                call_id,
                name,
                arguments,
                namespace,
            } => {
                let mut entries = vec![
                    ("type".to_string(), Value::String("function_call".to_string())),
                    ("call_id".to_string(), Value::String(call_id.clone())),
                    ("name".to_string(), Value::String(name.clone())),
                    ("arguments".to_string(), Value::String(arguments.clone())),
                ];
                if let Some(id) = id {
                    entries.push(("id".to_string(), Value::String(id.clone())));
                }
                if let Some(namespace) = namespace {
                    entries.push(("namespace".to_string(), Value::String(namespace.clone())));
                }
                Value::Map(entries)
            }
            Self::CustomToolCall {
                id,
                call_id,
                name,
                input,
                namespace,
            } => {
                let mut entries = vec![
                    ("type".to_string(), Value::String("custom_tool_call".to_string())),
                    ("call_id".to_string(), Value::String(call_id.clone())),
                    ("name".to_string(), Value::String(name.clone())),
                    ("input".to_string(), Value::String(input.clone())),
                ];
                if let Some(id) = id {
                    entries.push(("id".to_string(), Value::String(id.clone())));
                }
                if let Some(namespace) = namespace {
                    entries.push(("namespace".to_string(), Value::String(namespace.clone())));
                }
                Value::Map(entries)
            }
            Self::FunctionCallOutput { call_id, output } => Value::Map(vec![
                ("type".to_string(), Value::String("function_call_output".to_string())),
                ("call_id".to_string(), Value::String(call_id.clone())),
                ("output".to_string(), output.to_value()),
            ]),
            Self::CustomToolCallOutput { call_id, output } => Value::Map(vec![
                ("type".to_string(), Value::String("custom_tool_call_output".to_string())),
                ("call_id".to_string(), Value::String(call_id.clone())),
                ("output".to_string(), output.to_value()),
            ]),
            Self::AdditionalTools { tools } => Value::Map(vec![
                ("type".to_string(), Value::String("additional_tools".to_string())),
                ("role".to_string(), Value::String("developer".to_string())),
                (
                    "tools".to_string(),
                    Value::Array(tools.iter().map(|tool| tool.to_value()).collect()),
                ),
            ]),
            Self::ToolSearchCall { call_id, arguments } => Value::Map(vec![
                ("type".to_string(), Value::String("tool_search_call".to_string())),
                ("call_id".to_string(), Value::String(call_id.clone())),
                ("execution".to_string(), Value::String("client".to_string())),
                ("status".to_string(), Value::String("completed".to_string())),
                ("arguments".to_string(), arguments.clone()),
            ]),
            Self::ToolSearchOutput { call_id, tools } => Value::Map(vec![
                ("type".to_string(), Value::String("tool_search_output".to_string())),
                ("call_id".to_string(), Value::String(call_id.clone())),
                ("execution".to_string(), Value::String("client".to_string())),
                ("status".to_string(), Value::String("completed".to_string())),
                (
                    "tools".to_string(),
                    Value::Array(tools.iter().map(|tool| tool.to_value()).collect()),
                ),
            ]),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResponseToolResultOutput {
    Text(String),
    Blocks(Vec<ResponseInputContent>),
}

impl ResponseToolResultOutput {
    pub fn to_value(&self) -> Value {
        match self {
            Self::Text(text) => Value::String(text.clone()),
            Self::Blocks(blocks) => Value::Array(blocks.iter().map(|block| block.to_value()).collect()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum OpenAITool {
    Function {
        name: String,
        description: String,
        parameters: Value,
        strict: Option<bool>,
        defer_loading: Option<bool>,
    },
    Custom {
        name: String,
        description: String,
        format: String,
        definition: String,
        defer_loading: Option<bool>,
    },
}

impl OpenAITool {
    pub fn to_value(&self) -> Value {
        match self {
            Self::Function {
                name,
                description,
                parameters,
                strict,
                defer_loading,
            } => {
                let mut entries = vec![
                    ("type".to_string(), Value::String("function".to_string())),
                    ("name".to_string(), Value::String(name.clone())),
                    ("description".to_string(), Value::String(description.clone())),
                    ("parameters".to_string(), parameters.clone()),
                ];
                if let Some(strict) = strict {
                    entries.push(("strict".to_string(), Value::Bool(*strict)));
                }
                if let Some(defer_loading) = defer_loading {
                    entries.push(("defer_loading".to_string(), Value::Bool(*defer_loading)));
                }
                Value::Map(entries)
            }
            Self::Custom {
                name,
                description,
                format,
                definition,
                defer_loading,
            } => {
                let mut entries = vec![
                    ("type".to_string(), Value::String("custom".to_string())),
                    ("name".to_string(), Value::String(name.clone())),
                    ("description".to_string(), Value::String(description.clone())),
                    (
                        "format".to_string(),
                        Value::Map(vec![
                            ("type".to_string(), Value::String("grammar".to_string())),
                            ("syntax".to_string(), Value::String(format.clone())),
                            ("definition".to_string(), Value::String(definition.clone())),
                        ]),
                    ),
                ];
                if let Some(defer_loading) = defer_loading {
                    entries.push(("defer_loading".to_string(), Value::Bool(*defer_loading)));
                }
                Value::Map(entries)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Text signatures
// ---------------------------------------------------------------------------

/// Mirrors `encodeTextSignatureV1`.
pub fn encode_text_signature_v1(id: &str, phase: Option<&str>) -> String {
    let payload = match phase {
        Some(phase) => format!("{{\"v\":1,\"id\":\"{}\",\"phase\":\"{}\"}}", json_escape(id), json_escape(phase)),
        None => format!("{{\"v\":1,\"id\":\"{}\"}}", json_escape(id)),
    };
    payload
}

fn json_escape(s: &str) -> String {
    let mut result = String::new();
    for c in s.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            c if (c as u32) < 0x20 => result.push_str(&format!("\\u{:04x}", c as u32)),
            c => result.push(c),
        }
    }
    result
}

/// Mirrors `parseTextSignature`.
pub fn parse_text_signature(signature: Option<&str>) -> Option<(String, Option<String>)> {
    let signature = signature?;
    if signature.starts_with('{') {
        if let Ok(parsed) = crate::utils::json::parse_json_with_repair::<Value>(signature) {
            if let Value::Map(entries) = &parsed {
                let v = entries.iter().find(|(k, _)| k == "v").and_then(|(_, v)| v.as_number());
                let id = entries.iter().find(|(k, _)| k == "id").and_then(|(_, v)| v.as_str());
                if v == Some(1.0) && id.is_some() {
                    let phase = entries
                        .iter()
                        .find(|(k, _)| k == "phase")
                        .and_then(|(_, v)| v.as_str());
                    let phase = match phase {
                        Some(phase) if phase == "commentary" || phase == "final_answer" => Some(phase.to_string()),
                        _ => None,
                    };
                    return Some((id.unwrap().to_string(), phase));
                }
            }
        }
        // Fall through to legacy plain-string handling.
    }
    Some((signature.to_string(), None))
}

// ---------------------------------------------------------------------------
// Tool result output
// ---------------------------------------------------------------------------

fn sanitize_surrogates(text: &str) -> String {
    crate::utils::sanitize::sanitize_surrogates(text)
}

fn convert_tool_result_output(model: &Model, content: &[Content]) -> ResponseToolResultOutput {
    let text_result: Vec<&str> = content
        .iter()
        .filter_map(|block| match block {
            Content::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    let images: Vec<&crate::types::ImageContent> = content
        .iter()
        .filter_map(|block| match block {
            Content::Image(image) => Some(image),
            _ => None,
        })
        .collect();
    let has_text = !text_result.is_empty() || text_result.iter().any(|text| !text.is_empty());

    if images.is_empty() || !model.input.iter().any(|kind| kind == "image") {
        let text = if has_text {
            text_result.join("\n")
        } else if !images.is_empty() {
            "(see attached image)".to_string()
        } else {
            "(no tool output)".to_string()
        };
        return ResponseToolResultOutput::Text(sanitize_surrogates(&text));
    }

    let mut output: Vec<ResponseInputContent> = Vec::new();
    if has_text {
        output.push(ResponseInputContent::InputText(ResponseInputText {
            text: sanitize_surrogates(&text_result.join("\n")),
        }));
    }
    for image in images {
        output.push(ResponseInputContent::InputImage(ResponseInputImage {
            detail: "auto".to_string(),
            image_url: format!("data:{};base64,{}", image.mime_type, image.data),
        }));
    }
    ResponseToolResultOutput::Blocks(output)
}

// ---------------------------------------------------------------------------
// convertResponsesMessages
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct ConvertResponsesMessagesOptions {
    pub include_system_prompt: Option<bool>,
    pub grammar_tool_input_properties: Option<Vec<(String, String)>>,
    pub deferred_tools: Option<Vec<(String, Tool)>>,
    pub deferred_tools_mode: Option<String>,
    pub tool_options: Option<ConvertResponsesToolsOptions>,
}

#[derive(Clone, Debug, Default)]
pub struct ConvertResponsesToolsOptions {
    pub strict: Option<bool>,
    pub supports_strict_mode: Option<bool>,
    pub supports_openai_grammar_tools: Option<bool>,
    pub defer_loading: Option<bool>,
}

pub type ResponseInput = Vec<ResponseInputItem>;

pub fn normalize_id_part(part: &str) -> String {
    let sanitized: String = part
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    let normalized: String = if sanitized.chars().count() > 64 {
        sanitized.chars().take(64).collect()
    } else {
        sanitized
    };
    normalized.trim_end_matches('_').to_string()
}

fn build_foreign_responses_item_id(item_id: &str) -> String {
    let normalized = format!("fc_{}", short_hash(item_id));
    if normalized.chars().count() > 64 {
        normalized.chars().take(64).collect()
    } else {
        normalized
    }
}

fn normalize_tool_call_id(id: &str, model: &Model, source: &AssistantMessage, allowed: &std::collections::HashSet<String>) -> String {
    if !allowed.contains(&model.provider) {
        return normalize_id_part(id);
    }
    if !id.contains('|') {
        return normalize_id_part(id);
    }
    let (call_id, item_id) = id.split_once('|').expect("contains |");
    let normalized_call_id = normalize_id_part(call_id);
    let is_foreign_tool_call = source.provider != model.provider || source.api != model.api;
    let mut normalized_item_id = if is_foreign_tool_call {
        build_foreign_responses_item_id(item_id)
    } else {
        normalize_id_part(item_id)
    };
    // OpenAI Responses API requires item id to start with "fc".
    if !normalized_item_id.starts_with("fc_") {
        normalized_item_id = normalize_id_part(&format!("fc_{normalized_item_id}"));
    }
    format!("{normalized_call_id}|{normalized_item_id}")
}

fn get_grammar_tool_input_property(properties: &[(String, String)], tool_name: &str) -> Option<String> {
    properties
        .iter()
        .find(|(name, _)| name == tool_name)
        .map(|(_, property)| property.clone())
}

pub fn convert_responses_messages(
    model: &Model,
    context: &Context,
    allowed_tool_call_providers: &std::collections::HashSet<String>,
    options: Option<&ConvertResponsesMessagesOptions>,
) -> ResponseInput {
    let messages: ResponseInput = Vec::new();
    let mut loaded_tool_names = std::collections::HashSet::new();

    let transformed_messages = transform_messages(
        context.messages.clone(),
        model,
        Some(&|id: &str, model: &Model, source: &AssistantMessage| {
            normalize_tool_call_id(id, model, source, allowed_tool_call_providers)
        }),
    );

    let include_system_prompt = options.and_then(|o| o.include_system_prompt).unwrap_or(true);
    let mut messages = messages;
    if include_system_prompt {
        if let Some(system_prompt) = &context.system_prompt {
            let supports_developer_role = match &model.compat {
                Some(crate::types::ModelCompat::OpenAiCompletions(_)) => None,
                Some(crate::types::ModelCompat::OpenAiResponses(compat)) => compat.supports_developer_role,
                _ => None,
            };
            let role = if model.reasoning && supports_developer_role != Some(false) {
                "developer"
            } else {
                "system"
            };
            messages.push(ResponseInputItem::Developer {
                role: role.to_string(),
                content: vec![ResponseInputText {
                    text: sanitize_surrogates(system_prompt),
                }],
            });
        }
    }

    let mut msg_index = 0u64;
    for msg in transformed_messages {
        match msg {
            crate::types::Message::User(user) => {
                match user.content {
                    crate::types::UserMessageContent::Text(text) => {
                        messages.push(ResponseInputItem::User {
                            content: vec![ResponseInputContent::InputText(ResponseInputText {
                                text: sanitize_surrogates(&text),
                            })],
                        });
                    }
                    crate::types::UserMessageContent::Blocks(blocks) => {
                        let content: Vec<ResponseInputContent> = blocks
                            .iter()
                            .filter_map(|item| match item {
                                Content::Text(text) => Some(ResponseInputContent::InputText(ResponseInputText {
                                    text: sanitize_surrogates(&text.text),
                                })),
                                Content::Image(image) => Some(ResponseInputContent::InputImage(
                                    ResponseInputImage {
                                        detail: "auto".to_string(),
                                        image_url: format!("data:{};base64,{}", image.mime_type, image.data),
                                    },
                                )),
                                _ => None,
                            })
                            .collect();
                        if content.is_empty() {
                            continue;
                        }
                        messages.push(ResponseInputItem::User { content });
                    }
                }
            }
            crate::types::Message::Assistant(assistant) => {
                let mut output: ResponseInput = Vec::new();
                let is_same_provider_and_api =
                    assistant.provider == model.provider && assistant.api == model.api;
                let is_same_model = is_same_provider_and_api && assistant.model == model.id;
                let is_different_model = is_same_provider_and_api && assistant.model != model.id;
                let mut text_block_index = 0u64;

                for block in &assistant.content {
                    match block {
                        Content::Thinking(thinking) => {
                            if let Some(signature) = &thinking.thinking_signature {
                                if let Ok(reasoning_item) =
                                    crate::utils::json::parse_json_with_repair::<Value>(signature)
                                {
                                    output.push(ResponseInputItem::Reasoning(reasoning_item));
                                }
                            }
                        }
                        Content::Text(text_block) => {
                            let parsed_signature = parse_text_signature(text_block.text_signature.as_deref());
                            let fallback_message_id = if text_block_index == 0 {
                                format!("msg_pi_{msg_index}")
                            } else {
                                format!("msg_pi_{msg_index}_{text_block_index}")
                            };
                            text_block_index += 1;
                            // OpenAI requires id to be max 64 characters.
                            let msg_id = match &parsed_signature {
                                Some((id, _)) if !id.is_empty() => {
                                    if id.chars().count() > 64 {
                                        format!("msg_{}", short_hash(id))
                                    } else {
                                        id.clone()
                                    }
                                }
                                _ => fallback_message_id,
                            };
                            output.push(ResponseInputItem::AssistantMessage(ResponseOutputMessage {
                                id: msg_id,
                                content: vec![ResponseOutputText {
                                    text: sanitize_surrogates(&text_block.text),
                                }],
                                phase: parsed_signature.and_then(|(_, phase)| phase),
                            }));
                        }
                        Content::ToolCall(tool_call) => {
                            let (call_id, item_id_raw) = match tool_call.id.split_once('|') {
                                Some((call_id, item_id)) => (call_id.to_string(), Some(item_id.to_string())),
                                None => (tool_call.id.clone(), None),
                            };
                            let custom_input_property = options
                                .and_then(|o| o.grammar_tool_input_properties.as_ref())
                                .and_then(|properties| get_grammar_tool_input_property(properties, &tool_call.name));
                            let mut item_id = item_id_raw;

                            // For different-model messages, set id to undefined to avoid
                            // pairing validation; also drop non-fc_* ids for function calls.
                            if (is_different_model
                                && item_id.as_deref().is_some_and(|id| id.starts_with("fc_")))
                                || (custom_input_property.is_none()
                                    && !item_id.as_deref().is_some_and(|id| id.starts_with("fc_")))
                            {
                                item_id = None;
                            }

                            let can_replay_namespace = is_same_model
                                || options
                                    .and_then(|o| o.deferred_tools.as_ref())
                                    .is_some_and(|deferred| deferred.iter().any(|(name, _)| name == &tool_call.name));

                            if let Some(property) = custom_input_property {
                                let input = get_grammar_tool_input(&tool_call.name, &tool_call.arguments, &property)
                                    .unwrap_or_default();
                                output.push(ResponseInputItem::CustomToolCall {
                                    id: item_id,
                                    call_id,
                                    name: tool_call.name.clone(),
                                    input: sanitize_surrogates(&input),
                                    namespace: if can_replay_namespace {
                                        tool_call.namespace.clone()
                                    } else {
                                        None
                                    },
                                });
                            } else {
                                output.push(ResponseInputItem::FunctionCall {
                                    id: item_id,
                                    call_id,
                                    name: tool_call.name.clone(),
                                    arguments: crate::utils::json::json_stringify(&tool_call.arguments),
                                    namespace: if can_replay_namespace {
                                        tool_call.namespace.clone()
                                    } else {
                                        None
                                    },
                                });
                            }
                        }
                        Content::Image(_) => {}
                    }
                }
                if output.is_empty() {
                    continue;
                }
                messages.extend(output);
            }
            crate::types::Message::ToolResult(tool_result) => {
                let call_id = match tool_result.tool_call_id.split_once('|') {
                    Some((call_id, _)) => call_id.to_string(),
                    None => tool_result.tool_call_id.clone(),
                };
                let output = convert_tool_result_output(model, &tool_result.content);

                let is_grammar_tool = options
                    .and_then(|o| o.grammar_tool_input_properties.as_ref())
                    .is_some_and(|properties| properties.iter().any(|(name, _)| name == &tool_result.tool_name));
                if is_grammar_tool {
                    messages.push(ResponseInputItem::CustomToolCallOutput { call_id, output });
                } else {
                    messages.push(ResponseInputItem::FunctionCallOutput { call_id, output });
                }

                let mut deferred_tools: Vec<Tool> = Vec::new();
                if let Some(added_tool_names) = &tool_result.added_tool_names {
                    for name in added_tool_names {
                        let tool = options
                            .and_then(|o| o.deferred_tools.as_ref())
                            .and_then(|deferred| deferred.iter().find(|(n, _)| n == name))
                            .map(|(_, tool)| tool.clone());
                        let Some(tool) = tool else { continue };
                        if loaded_tool_names.contains(name) {
                            continue;
                        }
                        loaded_tool_names.insert(name.clone());
                        deferred_tools.push(tool);
                    }
                }
                if !deferred_tools.is_empty() {
                    if let Some(mode) = options.and_then(|o| o.deferred_tools_mode.as_deref()) {
                        if mode == "additional-tools" {
                            messages.push(ResponseInputItem::AdditionalTools {
                                tools: convert_responses_tools(
                                    &deferred_tools,
                                    options.and_then(|o| o.tool_options.as_ref()),
                                ),
                            });
                        } else if mode == "tool-search" {
                            let names: Vec<String> = deferred_tools.iter().map(|tool| tool.name.clone()).collect();
                            let search_call_id = format!(
                                "pi_tool_load_{}",
                                short_hash(&format!("{}:{}", tool_result.tool_call_id, names.join(",")))
                            );
                            messages.push(ResponseInputItem::ToolSearchCall {
                                call_id: search_call_id.clone(),
                                arguments: Value::Map(vec![
                                    ("query".to_string(), Value::String(names.join(" "))),
                                    ("limit".to_string(), Value::Number(names.len() as f64)),
                                ]),
                            });
                            let mut tool_options = options.and_then(|o| o.tool_options.clone());
                            if let Some(options) = tool_options.as_mut() {
                                options.defer_loading = Some(true);
                            }
                            messages.push(ResponseInputItem::ToolSearchOutput {
                                call_id: search_call_id,
                                tools: convert_responses_tools(&deferred_tools, tool_options.as_ref()),
                            });
                        }
                    }
                }
            }
        }
        msg_index += 1;
    }

    messages
}

// ---------------------------------------------------------------------------
// convertResponsesTools
// ---------------------------------------------------------------------------

pub fn convert_responses_tools(tools: &[Tool], options: Option<&ConvertResponsesToolsOptions>) -> Vec<OpenAITool> {
    let default_strict = match options.and_then(|o| o.strict) {
        Some(strict) => strict,
        None => false,
    };
    let supports_strict_mode = options.and_then(|o| o.supports_strict_mode).unwrap_or(true);
    let supports_openai_grammar_tools = options.and_then(|o| o.supports_openai_grammar_tools).unwrap_or(false);
    let defer_loading = options.and_then(|o| o.defer_loading);

    tools
        .iter()
        .map(|tool| {
            if let Ok(Some(grammar)) = resolve_grammar_constrained_sampling(tool, supports_openai_grammar_tools) {
                return OpenAITool::Custom {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    format: grammar.format,
                    definition: grammar.definition,
                    defer_loading,
                };
            }

            let constrained_strict =
                resolve_json_schema_strict_sampling(tool, supports_strict_mode).unwrap_or(None);
            let strict = if supports_strict_mode {
                constrained_strict.or(Some(default_strict))
            } else {
                None
            };
            OpenAITool::Function {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.to_value(),
                strict,
                defer_loading,
            }
        })
        .collect()
}

/// Mirrors `calculateCost` for responses usage when a service tier applies.
pub fn calculate_usage_cost(usage: &Usage) -> f64 {
    usage.cost.total
}
