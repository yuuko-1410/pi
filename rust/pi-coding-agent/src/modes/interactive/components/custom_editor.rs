//! Custom editor component for extensions, port of `components/custom-editor.ts`.

use std::sync::Arc;

use pi_tui::components::editor::{Editor, EditorOptions, EditorTheme};
use pi_tui::tui::Component;

use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::keybinding_hints::key_hint;
use crate::modes::interactive::theme::theme::theme;

pub struct CustomEditor {
    editor: Arc<Editor>,
    on_submit: Arc<dyn Fn(&str) + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
    title: String,
}

impl CustomEditor {
    pub fn new(
        title: &str,
        prefill: Option<&str>,
        on_submit: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        options: Option<EditorOptions>,
    ) -> Self {
        let border_color = theme()
            .as_ref()
            .map(|t| {
                let ansi = t.get_fg_ansi("borderMuted");
                Arc::new(move |text: &str| format!("{ansi}{text}\x1b[39m")) as Arc<dyn Fn(&str) -> String + Send + Sync>
            })
            .unwrap_or_else(|| Arc::new(|text: &str| text.to_string()));
        let mut editor = Editor::new(EditorTheme { border_color }, options.unwrap_or_default(), Arc::new(|| {}));
        if let Some(prefill) = prefill {
            editor.set_text(prefill);
        }
        Self {
            editor: Arc::new(editor),
            on_submit,
            on_cancel,
            title: title.to_string(),
        }
    }

    pub fn get_text(&self) -> String {
        self.editor.get_text()
    }
}

impl Component for CustomEditor {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        let title = t
            .map(|t| t.fg("accent", &self.title))
            .unwrap_or_else(|| self.title.clone());

        let mut lines: Vec<String> = Vec::new();
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines.push(format!(" {title}"));
        lines.extend(self.editor.render(width));
        let hint = format!(
            "{}  {}",
            key_hint("tui.select.confirm", "submit"),
            key_hint("tui.select.cancel", "cancel")
        );
        lines.push(format!(" {hint}"));
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = pi_tui::keybindings::get_keybindings();
        let manager = match &*kb {
            Some(manager) => manager,
            None => return,
        };
        if manager.matches(data, "tui.select.cancel") {
            (self.on_cancel)();
            return;
        }
        if manager.matches(data, "tui.select.confirm") {
            let text = self.editor.get_text();
            (self.on_submit)(&text);
            return;
        }
        if let Some(editor) = Arc::get_mut(&mut self.editor) {
            editor.handle_input(data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_and_holds_text() {
        let editor = CustomEditor::new(
            "Edit",
            Some("prefill"),
            Arc::new(|_| {}),
            Arc::new(|| {}),
            None,
        );
        assert_eq!(editor.get_text(), "prefill");
        let lines = editor.render(60);
        assert!(lines.iter().any(|line| line.contains("Edit")));
    }
}
