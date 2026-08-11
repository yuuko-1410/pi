//! Message transformation for provider requests, port of
//! `packages/ai/src/api/transform-messages.ts`.

use crate::types::{
    AssistantMessage, Content, Message, Model, StopReason, TextContent, ToolCall, ToolResultMessage,
    UserMessageContent,
};

pub const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
pub const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str = "(tool image omitted: model does not support images)";

fn replace_images_with_placeholder(content: &[Content], placeholder: &str) -> Vec<Content> {
    let mut result: Vec<Content> = Vec::new();
    let mut previous_was_placeholder = false;

    for block in content {
        match block {
            Content::Image(_) => {
                if !previous_was_placeholder {
                    result.push(Content::Text(TextContent {
                        text: placeholder.to_string(),
                        text_signature: None,
                    }));
                }
                previous_was_placeholder = true;
            }
            _ => {
                result.push(block.clone());
                let is_placeholder = match block {
                    Content::Text(text) => text.text == placeholder,
                    _ => false,
                };
                previous_was_placeholder = is_placeholder;
            }
        }
    }

    result
}

fn downgrade_unsupported_images(messages: Vec<Message>, model: &Model) -> Vec<Message> {
    if model.input.iter().any(|kind| kind == "image") {
        return messages;
    }

    messages
        .into_iter()
        .map(|msg| match msg {
            Message::User(mut user) => {
                if let UserMessageContent::Blocks(content) = &user.content {
                    user.content =
                        UserMessageContent::Blocks(replace_images_with_placeholder(content, NON_VISION_USER_IMAGE_PLACEHOLDER));
                }
                Message::User(user)
            }
            Message::ToolResult(mut tool) => {
                tool.content = replace_images_with_placeholder(&tool.content, NON_VISION_TOOL_IMAGE_PLACEHOLDER);
                Message::ToolResult(tool)
            }
            other => other,
        })
        .collect()
}

/// Normalize tool call IDs and drop unsupported content for cross-provider
/// compatibility. Mirrors `transformMessages` (two passes; the Rust message
/// types cannot carry null content, so that normalization is skipped).
pub fn transform_messages(
    messages: Vec<Message>,
    model: &Model,
    normalize_tool_call_id: Option<&dyn Fn(&str, &Model, &AssistantMessage) -> String>,
) -> Vec<Message> {
    // Map of original tool call IDs to normalized IDs.
    let mut tool_call_id_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let image_aware_messages = downgrade_unsupported_images(messages, model);

    // First pass: transform messages.
    let transformed: Vec<Message> = image_aware_messages
        .into_iter()
        .map(|msg| match msg {
            // User messages pass through unchanged.
            Message::User(_) => msg,
            // Handle toolResult messages — normalize toolCallId if mapped.
            Message::ToolResult(mut tool) => {
                if let Some(normalized_id) = tool_call_id_map.get(&tool.tool_call_id) {
                    if normalized_id != &tool.tool_call_id {
                        tool.tool_call_id = normalized_id.clone();
                    }
                }
                Message::ToolResult(tool)
            }
            // Assistant messages need transformation.
            Message::Assistant(assistant) => {
                let is_same_model = assistant.provider == model.provider
                    && assistant.api == model.api
                    && assistant.model == model.id;

                let mut transformed_content: Vec<Content> = Vec::new();
                for block in &assistant.content {
                    match block {
                        Content::Thinking(thinking) => {
                            if thinking.redacted == Some(true) {
                                // Redacted thinking is opaque encrypted content,
                                // only valid for the same model.
                                if is_same_model {
                                    transformed_content.push(block.clone());
                                }
                            } else if is_same_model && thinking.thinking_signature.is_some() {
                                // Keep thinking blocks with signatures (needed
                                // for replay) even when the text is empty.
                                transformed_content.push(block.clone());
                            } else if thinking.thinking.trim().is_empty() {
                                // Skip empty thinking blocks.
                            } else if is_same_model {
                                transformed_content.push(block.clone());
                            } else {
                                // Cross-model: convert to plain text.
                                transformed_content.push(Content::Text(TextContent {
                                    text: thinking.thinking.clone(),
                                    text_signature: None,
                                }));
                            }
                        }
                        Content::Text(_) => {
                            transformed_content.push(block.clone());
                        }
                        Content::ToolCall(tool_call) => {
                            let mut normalized_tool_call = tool_call.clone();
                            if !is_same_model && tool_call.thought_signature.is_some() {
                                normalized_tool_call.thought_signature = None;
                            }
                            if !is_same_model {
                                if let Some(normalize) = normalize_tool_call_id {
                                    let normalized_id = normalize(&tool_call.id, model, &assistant);
                                    if normalized_id != tool_call.id {
                                        tool_call_id_map.insert(tool_call.id.clone(), normalized_id.clone());
                                        normalized_tool_call.id = normalized_id;
                                    }
                                }
                            }
                            transformed_content.push(Content::ToolCall(normalized_tool_call));
                        }
                        Content::Image(_) => {
                            transformed_content.push(block.clone());
                        }
                    }
                }

                Message::Assistant(AssistantMessage {
                    content: transformed_content,
                    ..assistant
                })
            }
        })
        .collect();

    // Second pass: insert synthetic empty tool results for orphaned tool calls.
    let mut result: Vec<Message> = Vec::new();
    let mut pending_tool_calls: Vec<ToolCall> = Vec::new();
    let mut existing_tool_result_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let insert_synthetic_tool_results = |result: &mut Vec<Message>,
                                             pending_tool_calls: &mut Vec<ToolCall>,
                                             existing_tool_result_ids: &mut std::collections::HashSet<String>| {
        if pending_tool_calls.is_empty() {
            return;
        }
        let pending = std::mem::take(pending_tool_calls);
        for tool_call in pending {
            if !existing_tool_result_ids.contains(&tool_call.id) {
                result.push(Message::ToolResult(ToolResultMessage {
                    tool_call_id: tool_call.id,
                    tool_name: tool_call.name,
                    content: vec![Content::Text(TextContent {
                        text: "No result provided".to_string(),
                        text_signature: None,
                    })],
                    details: None,
                    usage: None,
                    added_tool_names: None,
                    is_error: true,
                    timestamp: now_ms(),
                }));
            }
        }
        existing_tool_result_ids.clear();
    };

    for msg in transformed {
        match msg {
            Message::Assistant(assistant) => {
                // Insert synthetic results for pending orphaned tool calls.
                insert_synthetic_tool_results(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                );

                // Skip errored/aborted assistant messages entirely: incomplete
                // turns that should not be replayed.
                if assistant.stop_reason == StopReason::Error || assistant.stop_reason == StopReason::Aborted {
                    continue;
                }

                // Track tool calls from this assistant message.
                let tool_calls: Vec<ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        Content::ToolCall(tool_call) => Some(tool_call.clone()),
                        _ => None,
                    })
                    .collect();
                if !tool_calls.is_empty() {
                    pending_tool_calls = tool_calls;
                    existing_tool_result_ids.clear();
                }

                result.push(Message::Assistant(assistant));
            }
            Message::ToolResult(tool) => {
                existing_tool_result_ids.insert(tool.tool_call_id.clone());
                result.push(Message::ToolResult(tool));
            }
            Message::User(user) => {
                // User message interrupts tool flow — insert synthetic results.
                insert_synthetic_tool_results(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                );
                result.push(Message::User(user));
            }
        }
    }

    // If the conversation ends with unresolved tool calls, synthesize now.
    insert_synthetic_tool_results(&mut result, &mut pending_tool_calls, &mut existing_tool_result_ids);

    result
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as f64
}
