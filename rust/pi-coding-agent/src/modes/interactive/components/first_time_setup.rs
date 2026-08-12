//! First-time setup dialog, port of `components/first-time-setup.ts`.

use std::sync::Arc;

use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::Component;

use crate::config::APP_NAME;
use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::keybinding_hints::{key_hint, raw_key_hint};
use crate::modes::interactive::theme::theme::theme;

pub type TerminalTheme = String; // "dark" | "light"

pub struct FirstTimeSetupResult {
    pub theme: TerminalTheme,
    pub share_analytics: bool,
}

pub struct FirstTimeSetupOptions {
    pub detected_theme: TerminalTheme,
    pub on_theme_preview: Arc<dyn Fn(&str) + Send + Sync>,
    pub on_submit: Arc<dyn Fn(FirstTimeSetupResult) + Send + Sync>,
    pub on_cancel: Arc<dyn Fn() + Send + Sync>,
}

const THEME_OPTIONS: [(&str, &str); 2] = [("dark", "Dark"), ("light", "Light")];
const ANALYTICS_OPTIONS: [(bool, &str); 2] = [(true, "Share anonymous usage data"), (false, "Don't share")];
const SETUP_LOGO_LINES: [&str; 4] = ["██████", "██  ██", "████  ██", "██    ██"];

/// First-time setup dialog: theme choice and analytics opt-in.
pub struct FirstTimeSetupComponent {
    step: &'static str, // "theme" | "analytics"
    theme_index: usize,
    analytics_index: usize,
    options: FirstTimeSetupOptions,
}

impl FirstTimeSetupComponent {
    pub fn new(options: FirstTimeSetupOptions) -> Self {
        let theme_index = THEME_OPTIONS
            .iter()
            .position(|(value, _)| *value == options.detected_theme)
            .unwrap_or(0);
        Self {
            step: "theme",
            theme_index,
            analytics_index: 0,
            options,
        }
    }

    fn add_option_list(&self, labels: &[&str], selected_index: usize) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        let mut lines = Vec::new();
        for (i, label) in labels.iter().enumerate() {
            let is_selected = i == selected_index;
            let prefix = if is_selected {
                t.map(|t| t.fg("accent", "→ ")).unwrap_or_else(|| "→ ".to_string())
            } else {
                "  ".to_string()
            };
            let text = if is_selected {
                t.map(|t| t.fg("accent", label)).unwrap_or_else(|| label.to_string())
            } else {
                t.map(|t| t.fg("text", label)).unwrap_or_else(|| label.to_string())
            };
            lines.push(format!(" {prefix}{text}"));
        }
        lines
    }

    fn move_selection(&mut self, delta: isize) {
        if self.step == "theme" {
            let next = (self.theme_index as isize + delta).clamp(0, THEME_OPTIONS.len() as isize - 1) as usize;
            if next != self.theme_index {
                self.theme_index = next;
                (self.options.on_theme_preview)(THEME_OPTIONS[self.theme_index].0);
            }
        } else {
            self.analytics_index =
                (self.analytics_index as isize + delta).clamp(0, ANALYTICS_OPTIONS.len() as isize - 1) as usize;
        }
    }
}

impl Component for FirstTimeSetupComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        let accent = |text: &str| t.map(|t| t.fg("accent", text)).unwrap_or_else(|| text.to_string());
        let muted = |text: &str| t.map(|t| t.fg("muted", text)).unwrap_or_else(|| text.to_string());
        let text_color = |text: &str| t.map(|t| t.fg("text", text)).unwrap_or_else(|| text.to_string());

        let mut lines: Vec<String> = Vec::new();
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines.push(format!(" {}", accent(&SETUP_LOGO_LINES.join("\n "))));
        lines.push(format!(
            " {}",
            accent(&format!("Welcome to {APP_NAME}, the minimal coding agent."))
        ));

        if self.step == "theme" {
            lines.push(format!(" {}", text_color("Pick a theme.")));
            lines.push(format!(
                " {}",
                muted(&format!("Detected system appearance: {}", self.options.detected_theme))
            ));
            let labels: Vec<&str> = THEME_OPTIONS.iter().map(|(_, label)| *label).collect();
            lines.extend(self.add_option_list(&labels, self.theme_index));
        } else {
            lines.push(format!(" {}", text_color("Opt-in to anonymous usage data sharing?")));
            lines.push(format!(
                " {}",
                muted(
                    "Opting in stores a tracking identifier in settings.json and enables anonymous\nusage analytics. This helps us to better debug, reproduce, and resolve issues\nand bugs within Pi. You can observe what is shared using /privacy and make\nchanges anytime in settings.json."
                )
            ));
            let labels: Vec<&str> = ANALYTICS_OPTIONS.iter().map(|(_, label)| *label).collect();
            lines.extend(self.add_option_list(&labels, self.analytics_index));
        }

        let hint = format!(
            "{}  {}  {}",
            raw_key_hint("↑↓", "navigate"),
            key_hint("tui.select.confirm", if self.step == "theme" { "continue" } else { "finish" }),
            key_hint("tui.select.cancel", "skip setup")
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
        if manager.matches(data, "tui.select.up") || data == "k" {
            self.move_selection(-1);
        } else if manager.matches(data, "tui.select.down") || data == "j" {
            self.move_selection(1);
        } else if manager.matches(data, "tui.select.confirm") || data == "\n" {
            if self.step == "theme" {
                self.step = "analytics";
            } else {
                let result = FirstTimeSetupResult {
                    theme: THEME_OPTIONS[self.theme_index].0.to_string(),
                    share_analytics: ANALYTICS_OPTIONS[self.analytics_index].0,
                };
                (self.options.on_submit)(result);
            }
        } else if manager.matches(data, "tui.select.cancel") {
            (self.options.on_cancel)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_welcome() {
        let component = FirstTimeSetupComponent::new(FirstTimeSetupOptions {
            detected_theme: "dark".to_string(),
            on_theme_preview: Arc::new(|_| {}),
            on_submit: Arc::new(|_| {}),
            on_cancel: Arc::new(|| {}),
        });
        let lines = component.render(60);
        assert!(lines.iter().any(|line| line.contains("Welcome to pi")));
        assert!(lines.iter().any(|line| line.contains("Dark")));
    }
}
