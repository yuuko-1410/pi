//! Config selector (package resources), port of `components/config-selector.ts`.
//!
//! ponytail: resource toggling is simplified to a flat list with Enter to
//! toggle; the JS group/subgroup tree and write-scope switching are
//! condensed.

use std::sync::Arc;

use pi_tui::tui::Container;
use pi_tui::components::input::Input;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::Component;

use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::keybinding_hints::raw_key_hint;
use crate::modes::interactive::theme::theme::theme;

#[derive(Clone, Debug)]
pub struct ConfigResourceItem {
    pub path: String,
    pub enabled: bool,
    pub display_name: String,
    pub resource_type: String,
    pub group_label: String,
}

pub struct ConfigSelectorCallbacks {
    pub on_toggle: Arc<dyn Fn(&str, bool) + Send + Sync>,
    pub on_close: Arc<dyn Fn() + Send + Sync>,
}

/// Config selector: list package resources with toggle state.
pub struct ConfigSelectorComponent {
    #[allow(dead_code)]
    container: Container,
    items: Vec<ConfigResourceItem>,
    selected_index: usize,
    search_input: Arc<Input>,
    callbacks: ConfigSelectorCallbacks,
    focused: bool,
}

impl ConfigSelectorComponent {
    pub fn new(items: Vec<ConfigResourceItem>, callbacks: ConfigSelectorCallbacks) -> Self {
        Self {
            container: Container::new(),
            items,
            selected_index: 0,
            search_input: Arc::new(Input::new()),
            callbacks,
            focused: false,
        }
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if let Some(input) = Arc::get_mut(&mut self.search_input) {
            input.focused = focused;
        }
    }

    fn filtered_items(&self) -> Vec<&ConfigResourceItem> {
        let query = self.search_input.get_value().to_lowercase();
        if query.is_empty() {
            self.items.iter().collect()
        } else {
            self.items
                .iter()
                .filter(|item| {
                    item.display_name.to_lowercase().contains(&query) || item.group_label.to_lowercase().contains(&query)
                })
                .collect()
        }
    }
}

impl Component for ConfigSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        let accent = |text: &str| t.map(|t| t.fg("accent", text)).unwrap_or_else(|| text.to_string());
        let muted = |text: &str| t.map(|t| t.fg("muted", text)).unwrap_or_else(|| text.to_string());
        let bold = |text: &str| t.map(|t| t.bold(text)).unwrap_or_else(|| text.to_string());

        let items = self.filtered_items();
        let mut lines: Vec<String> = Vec::new();
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines.push(format!(" {}", bold(&accent("Resources"))));
        lines.extend(self.search_input.render(width));

        if items.is_empty() {
            lines.push(format!(" {}", muted("No resources found")));
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
                let name = if is_selected { accent(&item.display_name) } else { item.display_name.clone() };
                let group = muted(&format!(" ({})", item.group_label));
                let state = if item.enabled {
                    t.map(|t| t.fg("success", " ✓")).unwrap_or_else(|| " ✓".to_string())
                } else {
                    t.map(|t| t.fg("dim", " ✗")).unwrap_or_else(|| " ✗".to_string())
                };
                lines.push(format!(" {prefix}{name}{group}{state}"));
            }

            if start_index > 0 || end_index < items.len() {
                let scroll_info = format!("  ({}/{})", self.selected_index + 1, items.len());
                lines.push(format!(" {}", muted(&scroll_info)));
            }
        }

        let hint = format!(
            "{} navigate · {} toggle · {} close",
            raw_key_hint("↑↓", ""),
            raw_key_hint("enter", ""),
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
        } else if manager.matches(data, "tui.select.confirm") || data == "\n" {
            let items = self.filtered_items();
            if let Some(item) = items.get(self.selected_index) {
                let path = item.path.clone();
                let enabled = !item.enabled;
                (self.callbacks.on_toggle)(&path, enabled);
            }
        } else if manager.matches(data, "tui.select.cancel") {
            (self.callbacks.on_close)();
        } else if let Some(input) = Arc::get_mut(&mut self.search_input) {
            input.handle_input(data);
            self.selected_index = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_resources() {
        let component = ConfigSelectorComponent::new(
            vec![ConfigResourceItem {
                path: "/tmp/ext.js".to_string(),
                enabled: true,
                display_name: "ext.js".to_string(),
                resource_type: "extensions".to_string(),
                group_label: "User (config/)".to_string(),
            }],
            ConfigSelectorCallbacks {
                on_toggle: Arc::new(|_, _| {}),
                on_close: Arc::new(|| {}),
            },
        );
        let lines = component.render(60);
        assert!(lines.iter().any(|line| line.contains("ext.js")));
    }
}
