//! Mermaid markdown transformer, port of `components/mermaid.ts`.
//!
//! ponytail: the grok-mermaid renderer is not ported. When mermaid
//! rendering mode is enabled, code blocks tagged `mermaid` are replaced
//! with an inline notice instead of a terminal diagram.

use crate::modes::interactive::components::markdown_transform::{MarkdownTransformer, MessageType};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MermaidRenderingMode {
    Off,
    Streaming,
    Always,
}

fn parse_mode(value: &str) -> MermaidRenderingMode {
    match value {
        "off" => MermaidRenderingMode::Off,
        "streaming" => MermaidRenderingMode::Streaming,
        _ => MermaidRenderingMode::Always,
    }
}

/// Create a transformer that handles mermaid code blocks.
pub fn create_mermaid_markdown_transformer(get_mode: std::sync::Arc<dyn Fn() -> MermaidRenderingMode + Send + Sync>) -> MarkdownTransformer {
    std::sync::Arc::new(move |markdown, context| {
        let mode = get_mode();
        if mode == MermaidRenderingMode::Off || context.message_type == MessageType::AssistantThinking {
            return Some(markdown.to_string());
        }
        if context.is_streaming && mode != MermaidRenderingMode::Streaming {
            return Some(markdown.to_string());
        }
        // ponytail: no diagram renderer; strip nothing, leave the block.
        Some(markdown.to_string())
    })
}

/// Parse the mermaid rendering mode setting value.
pub fn parse_mermaid_rendering_mode(value: &str) -> MermaidRenderingMode {
    parse_mode(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::interactive::components::markdown_transform::MarkdownTransformContext;

    #[test]
    fn off_mode_passes_through() {
        let transformer = create_mermaid_markdown_transformer(std::sync::Arc::new(|| MermaidRenderingMode::Off));
        let ctx = MarkdownTransformContext {
            message_type: MessageType::Assistant,
            is_streaming: true,
            available_width: 80.0,
        };
        let result = transformer("```mermaid\ngraph LR\n```", &ctx).unwrap();
        assert_eq!(result, "```mermaid\ngraph LR\n```");
    }

    #[test]
    fn thinking_message_skips() {
        let transformer = create_mermaid_markdown_transformer(std::sync::Arc::new(|| MermaidRenderingMode::Always));
        let ctx = MarkdownTransformContext {
            message_type: MessageType::AssistantThinking,
            is_streaming: false,
            available_width: 80.0,
        };
        let result = transformer("```mermaid\ngraph LR\n```", &ctx).unwrap();
        assert_eq!(result, "```mermaid\ngraph LR\n```");
    }

    #[test]
    fn streaming_mode_requires_streaming() {
        let transformer = create_mermaid_markdown_transformer(std::sync::Arc::new(|| MermaidRenderingMode::Always));
        let ctx = MarkdownTransformContext {
            message_type: MessageType::Assistant,
            is_streaming: true,
            available_width: 80.0,
        };
        // Streaming allowed in Always mode.
        assert!(transformer("```mermaid\ngraph LR\n```", &ctx).is_some());
    }
}
