//! Port of `packages/ai/src/utils/text.ts`.

use crate::types::Content;

/// Extract and join text from message content.
pub fn content_text(content: &[Content], separator: &str) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            Content::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(separator)
}
