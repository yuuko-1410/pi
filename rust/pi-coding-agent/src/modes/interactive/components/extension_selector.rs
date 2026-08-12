//! Generic selector component for extensions, port of
//! `components/extension-selector.ts`.

use std::sync::Arc;

use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::Component;

use crate::modes::interactive::components::countdown_timer::CountdownTimer;
use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::keybinding_hints::{key_hint, raw_key_hint};
use crate::modes::interactive::theme::theme::theme;

pub struct ExtensionSelectorComponent {
    options: Vec<String>,
    selected_index: usize,
    on_select: Arc<dyn Fn(&str) + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
    title: String,
    countdown: Option<CountdownTimer>,
    on_toggle_tools_expanded: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl ExtensionSelectorComponent {
    pub fn new(
        title: &str,
        options: Vec<String>,
        on_select: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        timeout_ms: Option<f64>,
        on_toggle_tools_expanded: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let mut component = Self {
            options,
            selected_index: 0,
            on_select,
            on_cancel,
            title: title.to_string(),
            countdown: None,
            on_toggle_tools_expanded,
        };
        if let Some(timeout) = timeout_ms {
            if timeout > 0.0 {
                let on_cancel = component.on_cancel.clone();
                component.countdown = Some(CountdownTimer::new(
                    timeout,
                    Box::new(|_| {}),
                    Box::new(move || (on_cancel)()),
                ));
            }
        }
        component
    }

    fn render_option(&self, option: &str, is_selected: bool) -> String {
        let t = theme();
        let t = t.as_ref();
        if is_selected {
            let prefix = t.map(|t| t.fg("accent", "→ ")).unwrap_or_else(|| "→ ".to_string());
            let text = t.map(|t| t.fg("accent", option)).unwrap_or_else(|| option.to_string());
            format!("{prefix}{text}")
        } else {
            let text = t.map(|t| t.fg("text", option)).unwrap_or_else(|| option.to_string());
            format!("  {text}")
        }
    }
}

impl Component for ExtensionSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        let title = t
            .map(|t| t.bold(&t.fg("accent", &self.title)))
            .unwrap_or_else(|| self.title.clone());

        let mut lines: Vec<String> = Vec::new();
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines.push(format!(" {title}"));
        for (i, option) in self.options.iter().enumerate() {
            lines.push(format!(" {}", self.render_option(option, i == self.selected_index)));
        }
        let hint = format!(
            "{}  {}  {}",
            raw_key_hint("↑↓", "navigate"),
            key_hint("tui.select.confirm", "select"),
            key_hint("tui.select.cancel", "cancel")
        );
        lines.push(format!(" {hint}"));
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = get_keybindings();
        let manager = match &*kb {
            Some(manager) => manager,
            None => return,
        };
        if manager.matches(data, "app.tools.expand") {
            if let Some(toggle) = &self.on_toggle_tools_expanded {
                toggle();
            }
        } else if manager.matches(data, "tui.select.up") || data == "k" {
            self.selected_index = self.selected_index.saturating_sub(1);
        } else if manager.matches(data, "tui.select.down") || data == "j" {
            self.selected_index = (self.selected_index + 1).min(self.options.len().saturating_sub(1));
        } else if manager.matches(data, "tui.select.confirm") || data == "\n" {
            if let Some(selected) = self.options.get(self.selected_index) {
                (self.on_select)(selected);
            }
        } else if manager.matches(data, "tui.select.cancel") {
            (self.on_cancel)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_options() {
        let component = ExtensionSelectorComponent::new(
            "Pick one",
            vec!["a".to_string(), "b".to_string()],
            Arc::new(|_| {}),
            Arc::new(|| {}),
            None,
            None,
        );
        let lines = component.render(60);
        assert!(lines.iter().any(|line| line.contains("Pick one")));
        assert!(lines.iter().any(|line| line.contains('a')));
    }
}
