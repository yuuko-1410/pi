//! Skill invocation message component, port of
//! `components/skill-invocation-message.ts`.
//!
//! ponytail: takes the skill name/content directly (the core
//! ParsedSkillBlock type arrives with agent-session-runtime).

use std::sync::Arc;

use pi_tui::components::basic::{Box, Text};
use pi_tui::components::markdown::{DefaultTextStyle, Markdown, MarkdownTheme};
use pi_tui::tui::Component;

use crate::modes::interactive::components::keybinding_hints::key_text;
use crate::modes::interactive::theme::theme::{get_markdown_theme, theme};

pub struct ParsedSkillBlock {
    pub name: String,
    pub content: String,
}

/// Component that renders a skill invocation message with
/// collapsed/expanded state.
pub struct SkillInvocationMessageComponent {
    inner: Box,
    expanded: bool,
    skill_block: ParsedSkillBlock,
}

impl SkillInvocationMessageComponent {
    pub fn new(skill_block: ParsedSkillBlock, markdown_theme: Option<MarkdownTheme>) -> Self {
        let bg_ansi = theme()
            .as_ref()
            .map(|t| t.get_bg_ansi("customMessageBg"))
            .unwrap_or_default();
        let inner = Box::new(
            1,
            1,
            if bg_ansi.is_empty() {
                None
            } else {
                Some(Arc::new(move |text: &str| format!("{bg_ansi}{text}\x1b[49m")) as Arc<dyn Fn(&str) -> String + Send + Sync>)
            },
        );
        let mut component = Self {
            inner,
            expanded: false,
            skill_block,
        };
        component.update_display(markdown_theme.unwrap_or_else(get_markdown_theme));
        component
    }

    pub fn set_expanded(&mut self, expanded: bool, markdown_theme: &MarkdownTheme) {
        self.expanded = expanded;
        self.update_display(markdown_theme.clone());
    }

    fn update_display(&mut self, markdown_theme: MarkdownTheme) {
        self.inner.clear();

        if self.expanded {
            let label = theme()
                .as_ref()
                .map(|t| t.fg("customMessageLabel", "\x1b[1m[skill]\x1b[22m"))
                .unwrap_or_else(|| "[skill]".to_string());
            self.inner.add_child(Arc::new(Text::new(&label, 0, 0, None)));
            let header = format!("**{}**\n\n", self.skill_block.name);
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
            self.inner.add_child(Arc::new(Markdown::new(
                &(header + &self.skill_block.content),
                0,
                0,
                markdown_theme,
                default_style,
                None,
            )));
        } else {
            let hint = key_text("app.tools.expand");
            let line = theme()
                .as_ref()
                .map(|t| {
                    t.fg("customMessageLabel", "\x1b[1m[skill]\x1b[22m ")
                        + &t.fg("customMessageText", &self.skill_block.name)
                        + &t.fg("dim", &format!(" ({hint} to expand)"))
                })
                .unwrap_or_else(|| format!("[skill] {} ({hint} to expand)", self.skill_block.name));
            self.inner.add_child(Arc::new(Text::new(&line, 0, 0, None)));
        }
    }
}

impl Component for SkillInvocationMessageComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.render(width)
    }

    fn invalidate(&mut self) {
        self.inner.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_collapsed_and_expanded() {
        let theme = get_markdown_theme();
        let block = ParsedSkillBlock {
            name: "my-skill".to_string(),
            content: "do the thing".to_string(),
        };
        let mut component = SkillInvocationMessageComponent::new(block, Some(theme.clone()));
        let collapsed = component.render(40);
        assert!(!collapsed.is_empty());
        assert!(collapsed.iter().any(|line| line.contains("my-skill")));
        component.set_expanded(true, &theme);
        let expanded = component.render(40);
        assert!(!expanded.is_empty());
    }
}
