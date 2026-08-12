//! Tool execution component, port of `components/tool-execution.ts`.
//!
//! ponytail: extension tool renderers (renderCall/renderResult closures) are
//! not ported; the default rendering path is used (tool title + args JSON +
//! text output). Image blocks render as text placeholders.

use std::sync::Arc;

use pi_ai::utils::json::json_stringify_pretty;
use pi_tui::components::basic::Text;
use pi_tui::tui::Container;
use pi_tui::tui::Component;
use pi_tui::utils::strip_terminal_sequences;

use crate::modes::interactive::theme::theme::theme;

pub struct ToolExecutionOptions {
    pub show_images: bool,
    pub image_width_cells: usize,
}

impl Default for ToolExecutionOptions {
    fn default() -> Self {
        Self {
            show_images: true,
            image_width_cells: 60,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolResultContent {
    pub content: Vec<ContentBlock>,
    pub details: Option<pi_protocol::Value>,
    pub is_error: bool,
}

#[derive(Clone, Debug)]
pub enum ContentBlock {
    Text { text: String },
    Image { data: Option<String>, mime_type: Option<String> },
    Other,
}

/// Extract text output from a tool result.
pub fn get_rendered_text_output(result: &Option<ToolResultContent>, _show_images: bool) -> String {
    let Some(result) = result else {
        return String::new();
    };
    let mut output: Vec<String> = Vec::new();
    for block in &result.content {
        match block {
            ContentBlock::Text { text } => {
                let clean = strip_terminal_sequences(text).replace('\r', "");
                output.push(clean);
            }
            ContentBlock::Image { mime_type, .. } => {
                output.push(format!(
                    "[image: {}]",
                    mime_type.clone().unwrap_or_else(|| "image/unknown".to_string())
                ));
            }
            ContentBlock::Other => {}
        }
    }
    output.join("\n")
}

/// Component that renders a tool call and its result.
pub struct ToolExecutionComponent {
    container: Container,
    tool_name: String,
    args: pi_protocol::Value,
    expanded: bool,
    is_partial: bool,
    result: Option<ToolResultContent>,
}

impl ToolExecutionComponent {
    pub fn new(tool_name: &str, _tool_call_id: &str, args: pi_protocol::Value) -> Self {
        let mut component = Self {
            container: Container::new(),
            tool_name: tool_name.to_string(),
            args,
            expanded: false,
            is_partial: true,
            result: None,
        };
        component.update_display();
        component
    }

    pub fn update_args(&mut self, args: pi_protocol::Value) {
        self.args = args;
        self.update_display();
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
        self.update_display();
    }

    pub fn update_result(&mut self, result: ToolResultContent, is_partial: bool) {
        self.result = Some(result);
        self.is_partial = is_partial;
        self.update_display();
    }

    pub fn set_args_complete(&mut self) {
        self.update_display();
    }

    fn update_display(&mut self) {
        let t = theme();
        let t = t.as_ref();
        let bg_key = if self.is_partial {
            "toolPendingBg"
        } else if self.result.as_ref().is_some_and(|result| result.is_error) {
            "toolErrorBg"
        } else {
            "toolSuccessBg"
        };
        let bg_ansi = t.map(|t| t.get_bg_ansi(bg_key)).unwrap_or_default();

        let mut text = String::new();
        let title = t
            .map(|t| t.bold(&t.fg("toolTitle", &self.tool_name)))
            .unwrap_or_else(|| self.tool_name.clone());
        text.push_str(&title);
        let content = json_stringify_pretty(&self.args);
        if !content.is_empty() && content != "null" {
            text.push_str(&format!("\n\n{content}"));
        }
        let output = get_rendered_text_output(&self.result, true);
        if !output.is_empty() {
            text.push_str(&format!("\n{output}"));
        }

        // Rebuild children: Text is a value type, so a stored Arc would go
        // stale on updates.
        self.container.clear();
        self.container.add_child(Arc::new(pi_tui::components::basic::Spacer::new(1)));
        if bg_ansi.is_empty() {
            self.container.add_child(Arc::new(Text::new(&text, 1, 1, None)));
        } else {
            let styled: Vec<String> = text
                .split('\n')
                .map(|line| format!("{bg_ansi}{line}\x1b[49m"))
                .collect();
            self.container.add_child(Arc::new(Text::new(&styled.join("\n"), 1, 1, None)));
        }
    }
}

impl Component for ToolExecutionComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn invalidate(&mut self) {
        self.update_display();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_tool_name_and_args() {
        let component = ToolExecutionComponent::new(
            "read",
            "call-1",
            pi_protocol::Value::Map(vec![("path".to_string(), pi_protocol::Value::String("/tmp/x".to_string()))]),
        );
        let lines = component.render(60);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|line| line.contains("read")));
    }

    #[test]
    fn result_text_output_is_rendered() {
        let mut component = ToolExecutionComponent::new("bash", "call-1", pi_protocol::Value::Null);
        component.update_result(
            ToolResultContent {
                content: vec![ContentBlock::Text { text: "hello\nworld".to_string() }],
                details: None,
                is_error: false,
            },
            false,
        );
        let lines = component.render(60);
        assert!(lines.iter().any(|line| line.contains("hello")));
    }
}
