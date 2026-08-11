//! Shared utilities for Google Generative AI and Google Vertex providers.
//!
//! Port of `packages/ai/src/api/google-shared.ts`.

use pi_protocol::Value;

use crate::api::constrained_sampling::resolve_json_schema_strict_sampling;
use crate::api::transform_messages::transform_messages;
use crate::types::{Context, Model, StopReason, Tool};
use crate::utils::sanitize::sanitize_surrogates;

pub const GOOGLE_THINKING_LEVELS: [&str; 5] =
    ["THINKING_LEVEL_UNSPECIFIED", "MINIMAL", "LOW", "MEDIUM", "HIGH"];


/// A Gemini `Content` (role + parts).
#[derive(Clone, Debug, PartialEq)]
pub struct GoogleContent {
    pub role: String,
    pub parts: Vec<GooglePart>,
}

impl GoogleContent {
    pub fn to_value(&self) -> Value {
        Value::Map(vec![
            ("role".to_string(), Value::String(self.role.clone())),
            (
                "parts".to_string(),
                Value::Array(self.parts.iter().map(|part| part.to_value()).collect()),
            ),
        ])
    }
}

/// A Gemini `Part`.
#[derive(Clone, Debug, PartialEq)]
pub enum GooglePart {
    Text {
        text: String,
        thought: bool,
        thought_signature: Option<String>,
    },
    InlineData {
        mime_type: String,
        data: String,
    },
    FunctionCall {
        name: String,
        args: Value,
        id: Option<String>,
        thought_signature: Option<String>,
    },
    FunctionResponse {
        name: String,
        response: Value,
        parts: Vec<GooglePart>,
        id: Option<String>,
    },
}

impl GooglePart {
    pub fn to_value(&self) -> Value {
        match self {
            Self::Text {
                text,
                thought,
                thought_signature,
            } => {
                let mut entries = vec![
                    ("text".to_string(), Value::String(text.clone())),
                    ("thought".to_string(), Value::Bool(*thought)),
                ];
                if let Some(signature) = thought_signature {
                    entries.push(("thoughtSignature".to_string(), Value::String(signature.clone())));
                }
                Value::Map(entries)
            }
            Self::InlineData { mime_type, data } => Value::Map(vec![(
                "inlineData".to_string(),
                Value::Map(vec![
                    ("mimeType".to_string(), Value::String(mime_type.clone())),
                    ("data".to_string(), Value::String(data.clone())),
                ]),
            )]),
            Self::FunctionCall {
                name,
                args,
                id,
                thought_signature,
            } => {
                let mut function_call_entries = vec![
                    ("name".to_string(), Value::String(name.clone())),
                    ("args".to_string(), args.clone()),
                ];
                if let Some(id) = id {
                    function_call_entries.push(("id".to_string(), Value::String(id.clone())));
                }
                let mut entries = vec![("functionCall".to_string(), Value::Map(function_call_entries))];
                if let Some(signature) = thought_signature {
                    entries.push(("thoughtSignature".to_string(), Value::String(signature.clone())));
                }
                Value::Map(entries)
            }
            Self::FunctionResponse {
                name,
                response,
                parts,
                id,
            } => {
                let mut function_response_entries = vec![
                    ("name".to_string(), Value::String(name.clone())),
                    ("response".to_string(), response.clone()),
                ];
                if !parts.is_empty() {
                    function_response_entries.push((
                        "parts".to_string(),
                        Value::Array(parts.iter().map(|part| part.to_value()).collect()),
                    ));
                }
                if let Some(id) = id {
                    function_response_entries.push(("id".to_string(), Value::String(id.clone())));
                }
                Value::Map(vec![(
                    "functionResponse".to_string(),
                    Value::Map(function_response_entries),
                )])
            }
        }
    }
}

/// Mirrors `isThinkingPart`: `thought: true` is the definitive marker for
/// thinking content.
pub fn is_thinking_part(part: &Value) -> bool {
    part.as_map()
        .and_then(|entries| {
            entries
                .iter()
                .find(|(key, _)| key == "thought")
                .and_then(|(_, value)| value.as_bool())
        })
        .unwrap_or(false)
}

/// Mirrors `retainThoughtSignature`: keeps the last non-empty signature for
/// the current block.
pub fn retain_thought_signature(existing: Option<String>, incoming: Option<&str>) -> Option<String> {
    match incoming {
        Some(incoming) if !incoming.is_empty() => Some(incoming.to_string()),
        _ => existing,
    }
}

/// Thought signatures must be base64 for Google APIs (TYPE_BYTES).
fn is_valid_thought_signature(signature: &str) -> bool {
    if signature.is_empty() || signature.len() % 4 != 0 {
        return false;
    }
    signature
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/' || byte == b'=')
}

/// Only keep signatures from the same provider/model and with valid base64.
fn resolve_thought_signature(is_same_provider_and_model: bool, signature: Option<&str>) -> Option<String> {
    if is_same_provider_and_model && signature.is_some_and(is_valid_thought_signature) {
        Some(signature.unwrap().to_string())
    } else {
        None
    }
}

/// Models via Google APIs that require explicit tool call IDs.
pub fn requires_tool_call_id(model_id: &str) -> bool {
    let gemini_major_version = get_gemini_major_version(model_id);
    model_id.starts_with("claude-")
        || model_id.starts_with("gpt-oss-")
        || gemini_major_version.is_some_and(|version| version >= 3)
}

fn get_gemini_major_version(model_id: &str) -> Option<u32> {
    let lower = model_id.to_ascii_lowercase();
    let rest = if let Some(rest) = lower.strip_prefix("gemini-live-") {
        rest
    } else {
        lower.strip_prefix("gemini-")?
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn supports_multimodal_function_response(model_id: &str) -> bool {
    if let Some(version) = get_gemini_major_version(model_id) {
        return version >= 3;
    }
    true
}

fn part_text(value: &Value) -> Option<String> {
    value
        .as_map()
        .and_then(|entries| {
            entries
                .iter()
                .find(|(key, _)| key == "text")
                .and_then(|(_, value)| value.as_str())
        })
        .map(|s| s.to_string())
}

/// Converts internal messages to Gemini `Content[]`.
pub fn convert_messages(model: &Model, context: &Context) -> Vec<GoogleContent> {
    let mut contents: Vec<GoogleContent> = Vec::new();
    let normalize_tool_call_id = |id: &str, _model: &Model, _source: &crate::types::AssistantMessage| -> String {
        if !requires_tool_call_id(&model.id) {
            return id.to_string();
        }
        let sanitized: String = id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
            .collect();
        sanitized.chars().take(64).collect()
    };

    let transformed_messages = transform_messages(context.messages.clone(), model, Some(&normalize_tool_call_id));

    for msg in transformed_messages {
        match msg {
            crate::types::Message::User(user) => {
                match user.content {
                    crate::types::UserMessageContent::Text(text) => {
                        contents.push(GoogleContent {
                            role: "user".to_string(),
                            parts: vec![GooglePart::Text {
                                text: sanitize_surrogates(&text),
                                thought: false,
                                thought_signature: None,
                            }],
                        });
                    }
                    crate::types::UserMessageContent::Blocks(blocks) => {
                        let mut parts: Vec<GooglePart> = Vec::new();
                        for item in &blocks {
                            match item {
                                crate::types::Content::Text(text) => {
                                    parts.push(GooglePart::Text {
                                        text: sanitize_surrogates(&text.text),
                                        thought: false,
                                        thought_signature: None,
                                    });
                                }
                                crate::types::Content::Image(image) => {
                                    parts.push(GooglePart::InlineData {
                                        mime_type: image.mime_type.clone(),
                                        data: image.data.clone(),
                                    });
                                }
                                _ => {}
                            }
                        }
                        if parts.is_empty() {
                            continue;
                        }
                        contents.push(GoogleContent {
                            role: "user".to_string(),
                            parts,
                        });
                    }
                }
            }
            crate::types::Message::Assistant(assistant) => {
                let mut parts: Vec<GooglePart> = Vec::new();
                // Check if message is from same provider and model — only then
                // keep thinking blocks.
                let is_same_provider_and_model =
                    assistant.provider == model.provider && assistant.model == model.id;

                for block in &assistant.content {
                    match block {
                        crate::types::Content::Text(text_block) => {
                            let thought_signature =
                                resolve_thought_signature(is_same_provider_and_model, text_block.text_signature.as_deref());
                            // Skip empty text blocks — unless they carry a
                            // thought signature.
                            if (text_block.text.is_empty() || text_block.text.trim().is_empty())
                                && thought_signature.is_none()
                            {
                                continue;
                            }
                            parts.push(GooglePart::Text {
                                text: sanitize_surrogates(&text_block.text),
                                thought: false,
                                thought_signature,
                            });
                        }
                        crate::types::Content::Thinking(thinking) => {
                            if is_same_provider_and_model {
                                let thought_signature = resolve_thought_signature(
                                    is_same_provider_and_model,
                                    thinking.thinking_signature.as_deref(),
                                );
                                if (thinking.thinking.is_empty() || thinking.thinking.trim().is_empty())
                                    && thought_signature.is_none()
                                {
                                    continue;
                                }
                                parts.push(GooglePart::Text {
                                    text: sanitize_surrogates(&thinking.thinking),
                                    thought: true,
                                    thought_signature,
                                });
                            } else {
                                // Cross-provider/model: the signature is unusable,
                                // empty blocks stay dropped.
                                if thinking.thinking.trim().is_empty() {
                                    continue;
                                }
                                parts.push(GooglePart::Text {
                                    text: sanitize_surrogates(&thinking.thinking),
                                    thought: false,
                                    thought_signature: None,
                                });
                            }
                        }
                        crate::types::Content::ToolCall(tool_call) => {
                            let thought_signature =
                                resolve_thought_signature(is_same_provider_and_model, tool_call.thought_signature.as_deref());
                            parts.push(GooglePart::FunctionCall {
                                name: tool_call.name.clone(),
                                args: tool_call.arguments.clone(),
                                id: if requires_tool_call_id(&model.id) {
                                    Some(tool_call.id.clone())
                                } else {
                                    None
                                },
                                thought_signature,
                            });
                        }
                        crate::types::Content::Image(_) => {}
                    }
                }

                if parts.is_empty() {
                    continue;
                }
                contents.push(GoogleContent {
                    role: "model".to_string(),
                    parts,
                });
            }
            crate::types::Message::ToolResult(tool_result) => {
                // Extract text and image content.
                let text_result: Vec<&str> = tool_result
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        crate::types::Content::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect();
                let image_content: Vec<&crate::types::ImageContent> = if model.input.iter().any(|kind| kind == "image") {
                    tool_result
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            crate::types::Content::Image(image) => Some(image),
                            _ => None,
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                let has_text = !text_result.is_empty();
                let has_images = !image_content.is_empty();

                // Gemini 3+ supports multimodal function responses; Claude and
                // other models behind Cloud Code Assist / Gemini < 3 still need
                // a separate user image turn.
                let model_supports_multimodal_function_response =
                    supports_multimodal_function_response(&model.id);

                // Use "output" key for success, "error" key for errors.
                let response_value = if has_text {
                    sanitize_surrogates(&text_result.join("\n"))
                } else if has_images {
                    "(see attached image)".to_string()
                } else {
                    String::new()
                };

                let image_parts: Vec<GooglePart> = image_content
                    .iter()
                    .map(|image_block| GooglePart::InlineData {
                        mime_type: image_block.mime_type.clone(),
                        data: image_block.data.clone(),
                    })
                    .collect();

                let include_id = requires_tool_call_id(&model.id);
                let function_response_part = GooglePart::FunctionResponse {
                    name: tool_result.tool_name.clone(),
                    response: Value::Map(vec![(
                        if tool_result.is_error { "error" } else { "output" }.to_string(),
                        Value::String(response_value),
                    )]),
                    parts: if has_images && model_supports_multimodal_function_response {
                        image_parts.clone()
                    } else {
                        Vec::new()
                    },
                    id: if include_id {
                        Some(tool_result.tool_call_id.clone())
                    } else {
                        None
                    },
                };

                // Cloud Code Assist API requires all function responses in a
                // single user turn; merge when the last content is a user turn
                // with function responses.
                let merged = match contents.last_mut() {
                    Some(content) if content.role == "user" => {
                        let has_function_response = content
                            .parts
                            .iter()
                            .any(|part| matches!(part, GooglePart::FunctionResponse { .. }));
                        if has_function_response {
                            content.parts.push(function_response_part.clone());
                            true
                        } else {
                            false
                        }
                    }
                    _ => false,
                };
                if !merged {
                    contents.push(GoogleContent {
                        role: "user".to_string(),
                        parts: vec![function_response_part],
                    });
                }

                // For Gemini < 3, add images in a separate user message.
                if has_images && !model_supports_multimodal_function_response {
                    let mut parts = vec![GooglePart::Text {
                        text: "Tool result image:".to_string(),
                        thought: false,
                        thought_signature: None,
                    }];
                    parts.extend(image_parts);
                    contents.push(GoogleContent {
                        role: "user".to_string(),
                        parts,
                    });
                }
            }
        }
    }

    contents
}

const JSON_SCHEMA_META_DECLARATIONS: [&str; 8] = [
    "$schema",
    "$id",
    "$anchor",
    "$dynamicAnchor",
    "$vocabulary",
    "$comment",
    "$defs",
    "definitions",
];

/// Strip meta-declarations from a schema value (recursive).
fn sanitize_for_openapi(value: &Value) -> Value {
    match value {
        Value::Map(entries) => Value::Map(
            entries
                .iter()
                .filter(|(key, _)| !JSON_SCHEMA_META_DECLARATIONS.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), sanitize_for_openapi(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(sanitize_for_openapi).collect()),
        other => other.clone(),
    }
}

/// Converts tools to Gemini function declarations format.
pub fn convert_tools(tools: &[Tool], use_parameters: bool) -> Option<Value> {
    if tools.is_empty() {
        return None;
    }
    let declarations: Vec<Value> = tools
        .iter()
        .map(|tool| {
            let mut entries = vec![
                ("name".to_string(), Value::String(tool.name.clone())),
                ("description".to_string(), Value::String(tool.description.clone())),
            ];
            if use_parameters {
                entries.push((
                    "parameters".to_string(),
                    sanitize_for_openapi(&tool.parameters.to_value()),
                ));
            } else {
                entries.push(("parametersJsonSchema".to_string(), tool.parameters.to_value()));
            }
            Value::Map(entries)
        })
        .collect();
    Some(Value::Array(vec![Value::Map(vec![(
        "functionDeclarations".to_string(),
        Value::Array(declarations),
    )])]))
}

/// Gemini 3+ enforces required function parameters in validated tool-calling
/// modes.
pub fn supports_google_strict_tool_sampling(model_id: &str) -> bool {
    get_gemini_major_version(model_id).is_some_and(|version| version >= 3)
}

/// Map tool choice string to Gemini FunctionCallingConfigMode.
pub fn map_tool_choice(choice: &str) -> &'static str {
    match choice {
        "none" => "NONE",
        "any" => "ANY",
        _ => "AUTO",
    }
}

pub fn resolve_google_function_calling_mode(
    tools: &[Tool],
    tool_choice: Option<&str>,
    supports_strict_mode: bool,
) -> Option<String> {
    let use_strict_mode = tools
        .iter()
        .any(|tool| resolve_json_schema_strict_sampling(tool, supports_strict_mode).ok().flatten() == Some(true));
    if tool_choice == Some("none") || tool_choice == Some("any") {
        return Some(map_tool_choice(tool_choice.unwrap()).to_string());
    }
    if use_strict_mode {
        return Some("VALIDATED".to_string());
    }
    tool_choice.map(|choice| map_tool_choice(choice).to_string())
}

/// Maps a Gemini FinishReason string to the pi StopReason.
pub fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::Stop,
        "MAX_TOKENS" => StopReason::Length,
        // All safety/error reasons map to error.
        "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "SAFETY" | "IMAGE_SAFETY" | "IMAGE_PROHIBITED_CONTENT"
        | "IMAGE_RECITATION" | "IMAGE_OTHER" | "RECITATION" | "FINISH_REASON_UNSPECIFIED" | "OTHER" | "LANGUAGE"
        | "MALFORMED_FUNCTION_CALL" | "UNEXPECTED_TOOL_CALL" | "NO_IMAGE" => StopReason::Error,
        // JS throws for unhandled reasons; keep the stream usable instead.
        _ => StopReason::Error,
    }
}

/// Maps a string finish reason (for raw API responses).
pub fn map_stop_reason_string(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::Stop,
        "MAX_TOKENS" => StopReason::Length,
        _ => StopReason::Error,
    }
}

/// Extracts the `text` field of a streamed part.
pub fn part_text_value(part: &Value) -> Option<String> {
    part_text(part)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_major_version_detection() {
        assert_eq!(get_gemini_major_version("gemini-2.5-pro"), Some(2));
        assert_eq!(get_gemini_major_version("gemini-3-pro-preview"), Some(3));
        assert_eq!(get_gemini_major_version("gemini-live-2.0"), Some(2));
        assert_eq!(get_gemini_major_version("claude-3-5"), None);
        assert_eq!(get_gemini_major_version("gemini-flash-latest"), None);
    }

    #[test]
    fn tool_call_id_requirements() {
        assert!(requires_tool_call_id("gemini-3-pro-preview"));
        assert!(!requires_tool_call_id("gemini-2.5-pro"));
        assert!(requires_tool_call_id("claude-3-5-sonnet"));
        assert!(requires_tool_call_id("gpt-oss-120b"));
    }

    #[test]
    fn stop_reason_mapping() {
        assert_eq!(map_stop_reason("STOP"), StopReason::Stop);
        assert_eq!(map_stop_reason("MAX_TOKENS"), StopReason::Length);
        assert_eq!(map_stop_reason("SAFETY"), StopReason::Error);
        assert_eq!(map_stop_reason_string("STOP"), StopReason::Stop);
    }

    #[test]
    fn thinking_signature_validation() {
        assert!(is_valid_thought_signature("aGVsbG8="));
        assert!(is_valid_thought_signature("YWJj"));
        assert!(!is_valid_thought_signature("abc"));
        assert!(!is_valid_thought_signature("a b="));
    }
}
