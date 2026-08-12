//! Assistant message component, port of `components/assistant-message.ts`.

use std::sync::Arc;

use pi_ai::types::{AssistantMessage, Content, StopReason};
use pi_tui::components::basic::{Spacer, Text};
use pi_tui::tui::Container;
use pi_tui::components::markdown::{DefaultTextStyle, Markdown, MarkdownOptions, MarkdownTheme};
use pi_tui::tui::Component;

use crate::modes::interactive::components::markdown_transform::{create_markdown_transform, MarkdownTransformer, MessageType};
use crate::modes::interactive::theme::theme::{get_markdown_theme, theme};

const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";

/// Component that renders a complete assistant message.
pub struct AssistantMessageComponent {
    container: Container,
    hide_thinking_block: bool,
    hidden_thinking_label: String,
    output_pad: usize,
    last_message: Option<AssistantMessage>,
    has_tool_calls: bool,
    is_streaming: bool,
    markdown_theme: MarkdownTheme,
    transformers: Vec<MarkdownTransformer>,
}

impl AssistantMessageComponent {
    pub fn new(
        message: Option<AssistantMessage>,
        hide_thinking_block: bool,
        hidden_thinking_label: &str,
        output_pad: usize,
        transformers: Vec<MarkdownTransformer>,
    ) -> Self {
        let mut component = Self {
            container: Container::new(),
            hide_thinking_block,
            hidden_thinking_label: hidden_thinking_label.to_string(),
            output_pad,
            last_message: None,
            has_tool_calls: false,
            is_streaming: false,
            markdown_theme: get_markdown_theme(),
            transformers,
        };
        if let Some(message) = message {
            component.update_content(&message, false);
        }
        component
    }

    pub fn set_hide_thinking_block(&mut self, hide: bool) {
        self.hide_thinking_block = hide;
        if let Some(message) = self.last_message.clone() {
            let streaming = self.is_streaming;
            self.update_content(&message, streaming);
        }
    }

    pub fn update_content(&mut self, message: &AssistantMessage, is_streaming: bool) {
        self.last_message = Some(message.clone());
        self.is_streaming = is_streaming;
        self.container.clear();

        let t = theme();
        let t = t.as_ref();

        let has_visible_content = message.content.iter().any(|content| match content {
            Content::Text(text) => !text.text.trim().is_empty(),
            Content::Thinking(thinking) => !thinking.thinking.trim().is_empty(),
            _ => false,
        });

        if has_visible_content {
            self.container.add_child(Arc::new(Spacer::new(1)));
        }

        let mut i = 0;
        while i < message.content.len() {
            match &message.content[i] {
                Content::Text(text) if !text.text.trim().is_empty() => {
                    let transform = create_markdown_transform(
                        MessageType::Assistant,
                        is_streaming,
                        &self.transformers,
                    );
                    let options = MarkdownOptions {
                        transform: Some(Arc::new(move |text: &str, width: f64| transform(text, width))),
                        ..Default::default()
                    };
                    let markdown = Markdown::new(
                        text.text.trim(),
                        self.output_pad,
                        0,
                        self.markdown_theme.clone(),
                        None,
                        Some(options),
                    );
                    self.container.add_child(Arc::new(markdown));
                }
                Content::Thinking(_) => {
                    let mut thinking_blocks: Vec<String> = Vec::new();
                    while i < message.content.len() {
                        match &message.content[i] {
                            Content::Thinking(thinking) => {
                                let content = thinking.thinking.trim();
                                if !content.is_empty() {
                                    thinking_blocks.push(content.to_string());
                                }
                            }
                            _ => break,
                        }
                        i += 1;
                    }
                    i -= 1;

                    if thinking_blocks.is_empty() {
                        i += 1;
                        continue;
                    }

                    let has_visible_content_after = message.content[i + 1..].iter().any(|content| match content {
                        Content::Text(text) => !text.text.trim().is_empty(),
                        Content::Thinking(thinking) => !thinking.thinking.trim().is_empty(),
                        _ => false,
                    });

                    if self.hide_thinking_block {
                        let label = t
                            .map(|t| t.italic(&t.fg("thinkingText", &self.hidden_thinking_label)))
                            .unwrap_or_else(|| self.hidden_thinking_label.clone());
                        self.container.add_child(Arc::new(Text::new(&label, self.output_pad, 0, None)));
                    } else {
                        let text_ansi = t.map(|t| t.get_fg_ansi("thinkingText")).unwrap_or_default();
                        let default_style = if text_ansi.is_empty() {
                            None
                        } else {
                            Some(DefaultTextStyle {
                                color: Some(Arc::new(move |text: &str| format!("{text_ansi}{text}\x1b[39m"))),
                                bold: false,
                                italic: true,
                                strikethrough: false,
                                underline: false,
                                bg_color: None,
                            })
                        };
                        let transform = create_markdown_transform(
                            MessageType::AssistantThinking,
                            is_streaming,
                            &self.transformers,
                        );
                        let options = MarkdownOptions {
                            transform: Some(Arc::new(move |text: &str, width: f64| transform(text, width))),
                            ..Default::default()
                        };
                        let markdown = Markdown::new(
                            &thinking_blocks.join("\n\n"),
                            self.output_pad,
                            0,
                            self.markdown_theme.clone(),
                            default_style,
                            Some(options),
                        );
                        self.container.add_child(Arc::new(markdown));
                    }
                    if has_visible_content_after {
                        self.container.add_child(Arc::new(Spacer::new(1)));
                    }
                }
                _ => {}
            }
            i += 1;
        }

        let has_tool_calls = message.content.iter().any(|content| matches!(content, Content::ToolCall(_)));
        self.has_tool_calls = has_tool_calls;

        let error_text = |text: &str| {
            t.map(|t| t.fg("error", text)).unwrap_or_else(|| text.to_string())
        };

        if message.stop_reason == StopReason::Length {
            self.container.add_child(Arc::new(Spacer::new(1)));
            self.container.add_child(Arc::new(Text::new(
                &error_text("Response was truncated before completion."),
                self.output_pad,
                0,
                None,
            )));
        } else if !has_tool_calls {
            match message.stop_reason {
                StopReason::Aborted => {
                    let abort_message = if let Some(error) = &message.error_message {
                        if error == "Request was aborted" {
                            "Operation aborted".to_string()
                        } else {
                            error.clone()
                        }
                    } else {
                        "Operation aborted".to_string()
                    };
                    self.container.add_child(Arc::new(Spacer::new(1)));
                    self.container.add_child(Arc::new(Text::new(&error_text(&abort_message), self.output_pad, 0, None)));
                }
                StopReason::Error => {
                    let error_msg = message.error_message.clone().unwrap_or_else(|| "Unknown error".to_string());
                    self.container.add_child(Arc::new(Spacer::new(1)));
                    self.container.add_child(Arc::new(Text::new(
                        &error_text(&format!("Error: {error_msg}")),
                        self.output_pad,
                        0,
                        None,
                    )));
                }
                _ => {}
            }
        }
    }
}

impl Component for AssistantMessageComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines = self.container.render(width);
        if self.has_tool_calls || lines.is_empty() {
            return lines;
        }
        lines[0] = format!("{OSC133_ZONE_START}{}", lines[0]);
        let last = lines.len() - 1;
        lines[last] = format!("{}{OSC133_ZONE_END}{OSC133_ZONE_FINAL}", lines[last]);
        lines
    }

    fn invalidate(&mut self) {
        if let Some(message) = self.last_message.clone() {
            let streaming = self.is_streaming;
            self.update_content(&message, streaming);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_message(text: &str) -> AssistantMessage {
        AssistantMessage {
            content: vec![Content::Text(pi_ai::types::TextContent {
                text: text.to_string(),
                text_signature: None,
            })],
            api: "openai-responses".to_string(),
            provider: "test".to_string(),
            model: "m".to_string(),
            response_model: None,
            response_id: None,
            usage: pi_ai::types::Usage {
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
            stop_reason: StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0.0,
        }
    }

    #[test]
    fn renders_text_message_with_zones() {
        let message = make_message("hello world");
        let component = AssistantMessageComponent::new(Some(message), false, "Thinking...", 1, Vec::new());
        let lines = component.render(40);
        assert!(!lines.is_empty());
        assert!(lines[0].starts_with(OSC133_ZONE_START));
        assert!(lines[lines.len() - 1].contains(OSC133_ZONE_FINAL));
    }

    #[test]
    fn length_stop_shows_error() {
        let mut message = make_message("partial");
        message.stop_reason = StopReason::Length;
        let component = AssistantMessageComponent::new(Some(message), false, "Thinking...", 1, Vec::new());
        let lines = component.render(40);
        assert!(lines.iter().any(|line| line.contains("truncated")));
    }
}
