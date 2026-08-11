//! GitHub Copilot dynamic headers, port of
//! `packages/ai/src/api/github-copilot-headers.ts`.

use crate::types::{Content, Message};

/// Copilot expects X-Initiator to indicate whether the request is
/// user-initiated or agent-initiated (e.g. follow-up after assistant/tool
/// messages).
pub fn infer_copilot_initiator(messages: &[Message]) -> &'static str {
    let last = messages.last();
    match last {
        Some(message) if !matches!(message, Message::User(_)) => "agent",
        _ => "user",
    }
}

/// Copilot requires Copilot-Vision-Request header when sending images.
pub fn has_copilot_vision_input(messages: &[Message]) -> bool {
    messages.iter().any(|message| match message {
        Message::User(user) => match &user.content {
            crate::types::UserMessageContent::Blocks(content) => content
                .iter()
                .any(|block| matches!(block, Content::Image(_))),
            _ => false,
        },
        Message::ToolResult(tool) => tool.content.iter().any(|block| matches!(block, Content::Image(_))),
        _ => false,
    })
}

pub struct CopilotDynamicHeadersParams<'a> {
    pub messages: &'a [Message],
    pub has_images: bool,
}

pub fn build_copilot_dynamic_headers(params: CopilotDynamicHeadersParams<'_>) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = vec![
        ("X-Initiator".to_string(), infer_copilot_initiator(params.messages).to_string()),
        ("Openai-Intent".to_string(), "conversation-edits".to_string()),
    ];

    if params.has_images {
        headers.push(("Copilot-Vision-Request".to_string(), "true".to_string()));
    }

    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AssistantMessage, StopReason, TextContent, Usage, UsageCost, UserMessage, UserMessageContent};

    fn user_message() -> Message {
        Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_string()),
            timestamp: 1.0,
        })
    }

    fn assistant_message() -> Message {
        Message::Assistant(AssistantMessage {
            content: vec![Content::Text(TextContent {
                text: "hi".to_string(),
                text_signature: None,
            })],
            api: "test".to_string(),
            provider: "p".to_string(),
            model: "m".to_string(),
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
                cost: UsageCost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    total: 0.0,
                },
            },
            stop_reason: StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 1.0,
        })
    }

    #[test]
    fn infers_initiator_from_last_message() {
        assert_eq!(infer_copilot_initiator(&[]), "user");
        assert_eq!(infer_copilot_initiator(&[user_message()]), "user");
        assert_eq!(infer_copilot_initiator(&[user_message(), assistant_message()]), "agent");
    }

    #[test]
    fn detects_vision_input() {
        let image_user = Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![Content::Image(crate::types::ImageContent {
                data: "abc".to_string(),
                mime_type: "image/png".to_string(),
            })]),
            timestamp: 1.0,
        });
        assert!(has_copilot_vision_input(&[image_user]));
        assert!(!has_copilot_vision_input(&[user_message()]));
    }
}
