//! Custom message component, port of `components/custom-message.ts`.
//!
//! ponytail: extension MessageRenderer closures are not ported; the default
//! rendering path (label + text content) is used.

use std::sync::Arc;

use pi_ai::types::Content;
use pi_tui::components::basic::{Box, Text};
use pi_tui::tui::Container;
use pi_tui::components::markdown::{DefaultTextStyle, Markdown, MarkdownTheme};
use pi_tui::tui::Component;

use crate::core::messages::ContentOrText;
use crate::modes::interactive::theme::theme::{get_markdown_theme, theme};

/// Component that renders a custom message entry.
pub struct CustomMessageComponent {
    container: Container,
    custom_type: String,
    content: ContentOrText,
    expanded: bool,
    #[allow(dead_code)]
    output_pad: usize,
    markdown_theme: MarkdownTheme,
}

impl CustomMessageComponent {
    pub fn new(custom_type: &str, content: ContentOrText, output_pad: usize) -> Self {
        let mut container = Container::new();
        container.add_child(Arc::new(pi_tui::components::basic::Spacer::new(1)));
        let mut component = Self {
            container,
            custom_type: custom_type.to_string(),
            content,
            expanded: false,
            output_pad,
            markdown_theme: get_markdown_theme(),
        };
        component.rebuild();
        component
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        if self.expanded != expanded {
            self.expanded = expanded;
            self.rebuild();
        }
    }

    fn extract_text(&self) -> String {
        match &self.content {
            ContentOrText::Text(text) => text.clone(),
            ContentOrText::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    Content::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    fn rebuild(&mut self) {
        self.container.clear();
        self.container.add_child(Arc::new(pi_tui::components::basic::Spacer::new(1)));

        let bg_ansi = theme()
            .as_ref()
            .map(|t| t.get_bg_ansi("customMessageBg"))
            .unwrap_or_default();
        let mut boxed = Box::new(
            1,
            1,
            if bg_ansi.is_empty() {
                None
            } else {
                Some(Arc::new(move |text: &str| format!("{bg_ansi}{text}\x1b[49m")) as Arc<dyn Fn(&str) -> String + Send + Sync>)
            },
        );

        let label = theme()
            .as_ref()
            .map(|t| t.fg("customMessageLabel", &format!("\x1b[1m[{}]\x1b[22m", self.custom_type)))
            .unwrap_or_else(|| format!("[{}]", self.custom_type));
        boxed.add_child(Arc::new(Text::new(&label, 0, 0, None)));

        let text = self.extract_text();
        if !text.is_empty() {
            let text_ansi = theme()
                .as_ref()
                .map(|t| t.get_fg_ansi("customMessageText"))
                .unwrap_or_default();
            let default_style = if text_ansi.is_empty() {
                None
            } else {
                Some(DefaultTextStyle {
                    color: Some(Arc::new(move |text: &str| format!("{text_ansi}{text}\x1b[39m"))),
                    bold: false,
                    italic: false,
                    strikethrough: false,
                    underline: false,
                    bg_color: None,
                })
            };
            boxed.add_child(Arc::new(Markdown::new(
                &text,
                0,
                0,
                self.markdown_theme.clone(),
                default_style,
                None,
            )));
        }
        self.container.add_child(Arc::new(boxed));
    }
}

impl Component for CustomMessageComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn invalidate(&mut self) {
        self.rebuild();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_label_and_text() {
        let component = CustomMessageComponent::new(
            "customType",
            ContentOrText::Text("hello world".to_string()),
            1,
        );
        let lines = component.render(60);
        assert!(lines.iter().any(|line| line.contains("customType")));
        assert!(lines.iter().any(|line| line.contains("hello world")));
    }
}
