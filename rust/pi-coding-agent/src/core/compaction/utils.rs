//! Compaction utilities, port of `core/compaction/utils.ts`.

use std::collections::HashSet;

use pi_ai::types::{Content, Message};

#[derive(Clone, Debug, Default)]
pub struct FileOperations {
    pub read: HashSet<String>,
    pub written: HashSet<String>,
    pub edited: HashSet<String>,
}

pub fn create_file_ops() -> FileOperations {
    FileOperations::default()
}

/// Extract file operations from tool calls in an assistant message.
pub fn extract_file_ops_from_message(message: &Message, file_ops: &mut FileOperations) {
    let Message::Assistant(assistant) = message else {
        return;
    };
    for block in &assistant.content {
        let Content::ToolCall(tool_call) = block else {
            continue;
        };
        let Some(arguments) = tool_call.arguments.as_map() else {
            continue;
        };
        let path = arguments
            .iter()
            .find(|(key, _)| key == "path")
            .and_then(|(_, value)| value.as_str());
        let Some(path) = path else {
            continue;
        };
        match tool_call.name.as_str() {
            "read" => {
                file_ops.read.insert(path.to_string());
            }
            "write" => {
                file_ops.written.insert(path.to_string());
            }
            "edit" => {
                file_ops.edited.insert(path.to_string());
            }
            _ => {}
        }
    }
}

/// Compute final file lists: readFiles (only read, not modified) and
/// modifiedFiles, both sorted.
pub fn compute_file_lists(file_ops: &FileOperations) -> (Vec<String>, Vec<String>) {
    let modified: HashSet<String> = file_ops.edited.union(&file_ops.written).cloned().collect();
    let mut read_only: Vec<String> = file_ops
        .read
        .iter()
        .filter(|file| !modified.contains(*file))
        .cloned()
        .collect();
    read_only.sort();
    let mut modified_files: Vec<String> = modified.into_iter().collect();
    modified_files.sort();
    (read_only, modified_files)
}

/// Format file operations as XML tags for the summary.
pub fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!("<read-files>\n{}\n</read-files>", read_files.join("\n")));
    }
    if !modified_files.is_empty() {
        sections.push(format!("<modified-files>\n{}\n</modified-files>", modified_files.join("\n")));
    }
    if sections.is_empty() {
        return String::new();
    }
    format!("\n\n{}", sections.join("\n\n"))
}

/// Maximum characters for a tool result in serialized summaries.
const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// Truncate text to a maximum character length, keeping the beginning and
/// appending a truncation marker.
fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let truncated_chars = text.len() - max_chars;
    format!("{}\n\n[... {truncated_chars} more characters truncated]", &text[..max_chars])
}

/// Serialize LLM messages to text for summarization. Callers run messages
/// through convert_to_llm first (custom types become user messages).
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();

    for message in messages {
        match message {
            Message::User(user) => {
                let content = pi_ai::utils::text::content_text(&user_content_blocks(user), "");
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Message::Assistant(assistant) => {
                let mut thinking_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<String> = Vec::new();

                for block in &assistant.content {
                    match block {
                        Content::Thinking(thinking) => thinking_parts.push(thinking.thinking.clone()),
                        Content::ToolCall(tool_call) => {
                            let args_str = match &tool_call.arguments {
                                pi_protocol::Value::Map(entries) => entries
                                    .iter()
                                    .map(|(key, value)| {
                                        format!("{key}={}", pi_ai::utils::json::json_stringify(value))
                                    })
                                    .collect::<Vec<_>>()
                                    .join(", "),
                                other => pi_ai::utils::json::json_stringify(other),
                            };
                            tool_calls.push(format!("{}({args_str})", tool_call.name));
                        }
                        _ => {}
                    }
                }

                if !thinking_parts.is_empty() {
                    parts.push(format!("[Assistant thinking]: {}", thinking_parts.join("\n")));
                }
                if assistant
                    .content
                    .iter()
                    .any(|block| matches!(block, Content::Text(_)))
                {
                    parts.push(format!(
                        "[Assistant]: {}",
                        pi_ai::utils::text::content_text(&assistant.content, " ")
                    ));
                }
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Message::ToolResult(tool_result) => {
                let content = pi_ai::utils::text::content_text(&tool_result.content, "");
                if !content.is_empty() {
                    parts.push(format!("[Tool result]: {}", truncate_for_summary(&content, TOOL_RESULT_MAX_CHARS)));
                }
            }
        }
    }

    parts.join("\n\n")
}

fn user_content_blocks(user: &pi_ai::types::UserMessage) -> Vec<Content> {
    match &user.content {
        pi_ai::types::UserMessageContent::Text(text) => vec![Content::Text(pi_ai::types::TextContent {
            text: text.clone(),
            text_signature: None,
        })],
        pi_ai::types::UserMessageContent::Blocks(blocks) => blocks.clone(),
    }
}

pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::{AssistantMessage, ToolCall, ToolResultMessage, Usage, UserMessage, UserMessageContent};

    fn tool_call(name: &str, path: &str) -> Content {
        Content::ToolCall(ToolCall {
            id: "1".into(),
            name: name.into(),
            arguments: pi_protocol::Value::Map(vec![("path".to_string(), pi_protocol::Value::String(path.into()))]),
            thought_signature: None,
            namespace: None,
        })
    }

    #[test]
    fn file_ops_extraction() {
        let message = Message::Assistant(AssistantMessage {
            content: vec![tool_call("read", "/a"), tool_call("edit", "/b"), tool_call("write", "/c")],
            api: "api".into(),
            provider: "p".into(),
            model: "m".into(),
            response_model: None,
            response_id: None,
            usage: Usage {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: 0.0,
                cost: pi_ai::types::UsageCost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    total: 0.0,
                },
            },
            stop_reason: pi_ai::types::StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0.0,
        });
        let mut ops = create_file_ops();
        extract_file_ops_from_message(&message, &mut ops);
        assert!(ops.read.contains("/a"));
        assert!(ops.edited.contains("/b"));
        assert!(ops.written.contains("/c"));
    }

    #[test]
    fn file_lists_split_read_vs_modified() {
        let mut ops = create_file_ops();
        ops.read.insert("/read-only".into());
        ops.read.insert("/both".into());
        ops.edited.insert("/both".into());
        ops.written.insert("/written".into());
        let (read_files, modified_files) = compute_file_lists(&ops);
        assert_eq!(read_files, vec!["/read-only"]);
        assert_eq!(modified_files, vec!["/both", "/written"]);
    }

    #[test]
    fn format_operations_xml() {
        let formatted = format_file_operations(&["/a".to_string()], &["/b".to_string()]);
        assert!(formatted.contains("<read-files>\n/a\n</read-files>"));
        assert!(formatted.contains("<modified-files>\n/b\n</modified-files>"));
        assert_eq!(format_file_operations(&[], &[]), "");
    }

    #[test]
    fn serialize_conversation_basic() {
        let messages = vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("hello".into()),
                timestamp: 0.0,
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "1".into(),
                tool_name: "read".into(),
                content: vec![Content::Text(pi_ai::types::TextContent {
                    text: "x".repeat(3000),
                    text_signature: None,
                })],
                details: None,
                usage: None,
                added_tool_names: None,
                is_error: false,
                timestamp: 0.0,
            }),
        ];
        let serialized = serialize_conversation(&messages);
        assert!(serialized.starts_with("[User]: hello"));
        assert!(serialized.contains("[Tool result]:"));
        assert!(serialized.contains("[... 1000 more characters truncated]"));
    }
}
