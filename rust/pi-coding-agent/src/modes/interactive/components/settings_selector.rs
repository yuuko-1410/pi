//! Settings selector, port of `components/settings-selector.ts`.
//!
//! ponytail: submenus (theme/thinking/warnings) are simplified to a
//! flat value list navigated with left/right; the full pi-tui SettingsList
//! is not required. Host callbacks are wired through SettingsCallbacks.

use std::sync::Arc;

use pi_tui::tui::Container;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::Component;

use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::keybinding_hints::raw_key_hint;
use crate::modes::interactive::theme::theme::theme;

pub struct SettingsItem {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub current_value: String,
    pub values: Vec<String>,
}

pub struct SettingsCallbacks {
    pub on_change: Arc<dyn Fn(&str, &str) + Send + Sync>,
    pub on_cancel: Arc<dyn Fn() + Send + Sync>,
}

/// Settings selector with a flat scrollable list and left/right value
/// cycling (ponytail: JS uses nested submenus).
pub struct SettingsSelectorComponent {
    #[allow(dead_code)]
    container: Container,
    items: Vec<SettingsItem>,
    selected_index: usize,
    callbacks: SettingsCallbacks,
    search_query: String,
}

impl SettingsSelectorComponent {
    pub fn new(items: Vec<SettingsItem>, callbacks: SettingsCallbacks) -> Self {
        Self {
            container: Container::new(),
            items,
            selected_index: 0,
            callbacks,
            search_query: String::new(),
        }
    }

    fn cycle_value(&mut self, delta: isize) {
        let Some(item) = self.items.get_mut(self.selected_index) else { return };
        if item.values.is_empty() {
            return;
        }
        let current = item
            .values
            .iter()
            .position(|value| *value == item.current_value)
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(item.values.len() as isize) as usize;
        item.current_value = item.values[next].clone();
        let value = item.current_value.clone();
        let id = item.id.to_string();
        (self.callbacks.on_change)(&id, &value);
    }
}

impl Component for SettingsSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        let accent = |text: &str| t.map(|t| t.fg("accent", text)).unwrap_or_else(|| text.to_string());
        let muted = |text: &str| t.map(|t| t.fg("muted", text)).unwrap_or_else(|| text.to_string());

        let mut lines: Vec<String> = Vec::new();
        lines.push(DynamicBorder::new(None).render(width).join("\n"));

        let items: Vec<&SettingsItem> = if self.search_query.is_empty() {
            self.items.iter().collect()
        } else {
            self.items
                .iter()
                .filter(|item| item.label.to_lowercase().contains(&self.search_query))
                .collect()
        };

        if items.is_empty() {
            lines.push(format!(" {}", muted("No settings match")));
        } else {
            let max_visible = 10;
            let start_index = (self.selected_index as isize - (max_visible as isize / 2))
                .max(0)
                .min((items.len() as isize - max_visible as isize).max(0))
                .max(0) as usize;
            let end_index = (start_index + max_visible).min(items.len());

            for i in start_index..end_index {
                let item = items[i];
                let is_selected = i == self.selected_index;
                let prefix = if is_selected { accent("→ ") } else { "  ".to_string() };
                let label = if is_selected { accent(item.label) } else { item.label.to_string() };
                let value = muted(&format!(" [{}]", item.current_value));
                lines.push(format!(" {prefix}{label}{value}"));
                if is_selected {
                    lines.push(format!("   {}", muted(item.description)));
                }
            }

            if start_index > 0 || end_index < items.len() {
                let scroll_info = format!("  ({}/{})", self.selected_index + 1, items.len());
                lines.push(format!(" {}", muted(&scroll_info)));
            }
        }

        let hint = format!(
            "{} navigate · {} cycle value · {} cancel",
            raw_key_hint("↑↓", ""),
            raw_key_hint("←→", ""),
            raw_key_hint("esc", "")
        );
        lines.push(format!(" {}", muted(&hint)));
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
            self.selected_index = self.selected_index.saturating_sub(1);
        } else if manager.matches(data, "tui.select.down") || data == "j" {
            self.selected_index = (self.selected_index + 1).min(self.items.len().saturating_sub(1));
        } else if manager.matches(data, "tui.select.pageUp") {
            self.selected_index = self.selected_index.saturating_sub(10);
        } else if manager.matches(data, "tui.select.pageDown") {
            self.selected_index = (self.selected_index + 10).min(self.items.len().saturating_sub(1));
        } else if manager.matches(data, "tui.select.cancel") {
            (self.callbacks.on_cancel)();
        } else if data == "\x1b[C" || data == "l" {
            self.cycle_value(1);
        } else if data == "\x1b[D" || data == "h" {
            self.cycle_value(-1);
        } else if !data.is_empty() {
            // Search: accumulate printable characters (simplified).
            if data.chars().all(|c| c.is_alphanumeric() || c == '-' || c == ' ') {
                self.search_query.push_str(data);
                self.selected_index = 0;
            } else if data == "\u{8}" || data == "\x7f" {
                self.search_query.pop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_items() -> Vec<SettingsItem> {
        vec![
            SettingsItem {
                id: "autocompact",
                label: "Auto-compact",
                description: "Automatically compact context",
                current_value: "true".to_string(),
                values: vec!["true".to_string(), "false".to_string()],
            },
            SettingsItem {
                id: "transport",
                label: "Transport",
                description: "Preferred transport",
                current_value: "sse".to_string(),
                values: vec!["sse".to_string(), "websocket".to_string(), "auto".to_string()],
            },
        ]
    }

    #[test]
    fn renders_items() {
        let component = SettingsSelectorComponent::new(make_items(), SettingsCallbacks {
            on_change: Arc::new(|_, _| {}),
            on_cancel: Arc::new(|| {}),
        });
        let lines = component.render(60);
        assert!(lines.iter().any(|line| line.contains("Auto-compact")));
        assert!(lines.iter().any(|line| line.contains("Transport")));
    }

    #[test]
    fn cycles_values() {
        let changed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let changed_clone = changed.clone();
        let mut component = SettingsSelectorComponent::new(make_items(), SettingsCallbacks {
            on_change: Arc::new(move |id, value| changed_clone.lock().unwrap().push((id.to_string(), value.to_string()))),
            on_cancel: Arc::new(|| {}),
        });
        component.cycle_value(1);
        assert_eq!(component.items[0].current_value, "false");
        component.cycle_value(1);
        assert_eq!(component.items[0].current_value, "true");
        assert_eq!(changed.lock().unwrap().len(), 2);
    }
}
