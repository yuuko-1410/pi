//! Markdown transform utilities, port of `components/markdown-transform.ts`.

pub type MarkdownTransformer =
    std::sync::Arc<dyn Fn(&str, &MarkdownTransformContext) -> Option<String> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MessageType {
    User,
    Assistant,
    AssistantThinking,
    ToolResult,
    Custom,
    Skill,
    BranchSummary,
    CompactionSummary,
    BashExecution,
}

#[derive(Clone, Debug)]
pub struct MarkdownTransformContext {
    pub message_type: MessageType,
    pub is_streaming: bool,
    pub available_width: f64,
}

/// Run all transformers in sequence (getMarkdownTransform equivalent).
pub fn create_markdown_transform(
    message_type: MessageType,
    is_streaming: bool,
    transformers: &[MarkdownTransformer],
) -> Box<dyn Fn(&str, f64) -> String + Send + Sync> {
    let transformers: Vec<MarkdownTransformer> = transformers.to_vec();
    Box::new(move |markdown, available_width| {
        apply_markdown_transformers(
            markdown,
            &MarkdownTransformContext {
                message_type,
                is_streaming,
                available_width,
            },
            &transformers,
        )
    })
}

pub fn apply_markdown_transformers(
    markdown: &str,
    context: &MarkdownTransformContext,
    transformers: &[MarkdownTransformer],
) -> String {
    let mut transformed = markdown.to_string();
    for transformer in transformers {
        match transformer(&transformed, context) {
            Some(value) => transformed = value,
            None => {} // keep current markdown, continue
        }
    }
    transformed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_transformers_in_order() {
        let t1: MarkdownTransformer = std::sync::Arc::new(|text, _| {
            Some(text.replace("a", "b"))
        });
        let t2: MarkdownTransformer = std::sync::Arc::new(|text, _| {
            Some(text.replace("b", "c"))
        });
        let ctx = MarkdownTransformContext {
            message_type: MessageType::User,
            is_streaming: false,
            available_width: 80.0,
        };
        assert_eq!(apply_markdown_transformers("aba", &ctx, &[t1, t2]), "ccc");
    }

    #[test]
    fn failing_transformer_keeps_text() {
        let t1: MarkdownTransformer = std::sync::Arc::new(|_, _| None);
        let ctx = MarkdownTransformContext {
            message_type: MessageType::User,
            is_streaming: false,
            available_width: 80.0,
        };
        assert_eq!(apply_markdown_transformers("keep", &ctx, &[t1]), "keep");
    }

    #[test]
    fn create_transform_closure_works() {
        let t: MarkdownTransformer = std::sync::Arc::new(|text, _| Some(text.to_uppercase()));
        let transform = create_markdown_transform(MessageType::User, false, &[t]);
        assert_eq!(transform("hello", 80.0), "HELLO");
    }
}
