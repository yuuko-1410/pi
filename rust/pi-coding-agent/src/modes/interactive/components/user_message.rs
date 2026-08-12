//! User message component, port of `components/user-message.ts`.

use std::sync::Arc;

use pi_tui::components::basic::Box;
use pi_tui::tui::Container;
use pi_tui::components::markdown::{DefaultTextStyle, Markdown, MarkdownOptions, MarkdownTheme};
use pi_tui::tui::Component;

use crate::modes::interactive::components::markdown_transform::{create_markdown_transform, MarkdownTransformer, MessageType};
use crate::modes::interactive::theme::theme::{get_markdown_theme, theme};

const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";

/// Component that renders a user message.
pub struct UserMessageComponent {
    container: Container,
    text: String,
    output_pad: usize,
}

impl UserMessageComponent {
    pub fn new(text: &str, markdown_theme: Option<MarkdownTheme>, output_pad: usize, transformers: Vec<MarkdownTransformer>) -> Self {
        let mut component = Self {
            container: Container::new(),
            text: text.to_string(),
            output_pad,
        };
        component.rebuild(markdown_theme, &transformers);
        component
    }

    pub fn set_output_pad(&mut self, padding: usize, markdown_theme: &MarkdownTheme, transformers: &[MarkdownTransformer]) {
        self.output_pad = padding;
        self.rebuild(Some(markdown_theme.clone()), transformers);
    }

    fn rebuild(&mut self, markdown_theme: Option<MarkdownTheme>, transformers: &[MarkdownTransformer]) {
        self.container.clear();
        let markdown_theme = markdown_theme.unwrap_or_else(get_markdown_theme);
        let text_ansi = theme()
            .as_ref()
            .map(|t| t.get_fg_ansi("userMessageText"))
            .unwrap_or_default();
        let default_style = if text_ansi.is_empty() {
            Some(DefaultTextStyle {
                color: None,
                bold: false,
                italic: false,
                strikethrough: false,
                underline: false,
                bg_color: None,
            })
        } else {
            Some(DefaultTextStyle {
                color: Some(Arc::new(move |content: &str| format!("{text_ansi}{content}\x1b[39m"))),
                bold: false,
                italic: false,
                strikethrough: false,
                underline: false,
                bg_color: None,
            })
        };
        let transform = create_markdown_transform(MessageType::User, false, transformers);
        let options = MarkdownOptions {
            preserve_ordered_list_markers: true,
            preserve_backslash_escapes: true,
            transform: Some(Arc::new(move |text: &str, width: f64| transform(text, width))),
            ..Default::default()
        };
        let bg_ansi = theme()
            .as_ref()
            .map(|t| t.get_bg_ansi("userMessageBg"))
            .unwrap_or_default();
        let mut content_box = Box::new(
            self.output_pad,
            1,
            if bg_ansi.is_empty() {
                None
            } else {
                Some(Arc::new(move |content: &str| format!("{bg_ansi}{content}\x1b[49m")) as Arc<dyn Fn(&str) -> String + Send + Sync>)
            },
        );
        content_box.add_child(Arc::new(Markdown::new(
            &self.text,
            0,
            0,
            markdown_theme,
            default_style,
            Some(options),
        )));
        self.container.add_child(Arc::new(content_box));
    }
}

impl Component for UserMessageComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines = self.container.render(width);
        if lines.is_empty() {
            return lines;
        }
        lines[0] = format!("{OSC133_ZONE_START}{}", lines[0]);
        let last = lines.len() - 1;
        lines[last] = format!("{}{OSC133_ZONE_END}{OSC133_ZONE_FINAL}", lines[last]);
        lines
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_with_osc133_zones() {
        let component = UserMessageComponent::new("hello", None, 1, Vec::new());
        let lines = component.render(40);
        assert!(!lines.is_empty());
        assert!(lines[0].starts_with(OSC133_ZONE_START));
        assert!(lines[lines.len() - 1].contains(OSC133_ZONE_FINAL));
    }
}
