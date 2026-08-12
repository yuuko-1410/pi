//! Simple text input component for extensions, port of
//! `components/extension-input.ts`.

use std::sync::Arc;

use pi_tui::components::input::Input;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::Component;

use crate::modes::interactive::components::countdown_timer::CountdownTimer;
use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::keybinding_hints::key_hint;
use crate::modes::interactive::theme::theme::theme;

pub struct ExtensionInputComponent {
    input: Arc<Input>,
    on_submit: Arc<dyn Fn(&str) + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
    title_text: String,
    #[allow(dead_code)]
    base_title: String,
    countdown: Option<CountdownTimer>,
    focused: bool,
}

impl ExtensionInputComponent {
    pub fn new(
        title: &str,
        on_submit: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        timeout_ms: Option<f64>,
    ) -> Self {
        let mut component = Self {
            input: Arc::new(Input::new()),
            on_submit,
            on_cancel,
            title_text: title.to_string(),
            base_title: title.to_string(),
            countdown: None,
            focused: false,
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

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if let Some(input) = Arc::get_mut(&mut self.input) {
            input.focused = focused;
        }
    }
}

impl Component for ExtensionInputComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        let title = t
            .map(|t| t.fg("accent", &self.title_text))
            .unwrap_or_else(|| self.title_text.clone());

        let mut lines: Vec<String> = Vec::new();
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines.push(format!(" {title}"));
        lines.extend(self.input.render(width));
        let hint = format!(
            "{}  {}",
            key_hint("tui.select.confirm", "submit"),
            key_hint("tui.select.cancel", "cancel")
        );
        lines.push(format!(" {hint}"));
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines
    }

    fn handle_input(&mut self, key_data: &str) {
        let kb = get_keybindings();
        let manager = match &*kb {
            Some(manager) => manager,
            None => return,
        };
        if manager.matches(key_data, "tui.select.confirm") || key_data == "\n" {
            let value = self.input.get_value().to_string();
            (self.on_submit)(&value);
        } else if manager.matches(key_data, "tui.select.cancel") {
            (self.on_cancel)();
        } else if let Some(input) = Arc::get_mut(&mut self.input) {
            input.handle_input(key_data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_with_title() {
        let component = ExtensionInputComponent::new(
            "Enter value",
            Arc::new(|_| {}),
            Arc::new(|| {}),
            None,
        );
        let lines = component.render(60);
        assert!(lines.iter().any(|line| line.contains("Enter value")));
    }
}
