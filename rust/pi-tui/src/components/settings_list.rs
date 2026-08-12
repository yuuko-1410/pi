//! Settings list component, port of `packages/tui/src/components/settings-list.ts`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::fuzzy::fuzzy_filter;
use crate::keybindings::{get_keybindings, KeybindingsManager};
use crate::tui::Component;
use crate::utils::{truncate_to_width, visible_width, wrap_text_with_ansi};

#[derive(Clone)]
pub struct SettingItem {
    pub id: String,
    pub label: String,
    pub current_value: String,
    pub description: Option<String>,
    pub values: Option<Vec<String>>,
    pub submenu: Option<Arc<dyn Fn(&str, Arc<dyn Fn(Option<&str>) + Send + Sync>) -> Arc<dyn Fn(usize) -> Vec<String> + Send + Sync> + Send + Sync>>,
}

impl std::fmt::Debug for SettingItem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SettingItem")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("current_value", &self.current_value)
            .field("description", &self.description)
            .field("values", &self.values)
            .finish_non_exhaustive()
    }
}

impl PartialEq for SettingItem {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.label == other.label && self.current_value == other.current_value
    }
}

pub struct SettingsListTheme {
    pub cursor: String,
    pub label: Arc<dyn Fn(&str, bool) -> String + Send + Sync>,
    pub value: Arc<dyn Fn(&str, bool) -> String + Send + Sync>,
    pub description: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub hint: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

pub struct SettingsListOptions {
    pub enable_search: bool,
}

impl Default for SettingsListOptions {
    fn default() -> Self {
        Self { enable_search: false }
    }
}

pub struct SettingsList {
    items: Vec<SettingItem>,
    filtered_items: Vec<SettingItem>,
    theme: SettingsListTheme,
    selected_index: usize,
    max_visible: usize,
    search_enabled: bool,
    search_value: String,
    submenu_component: Option<Arc<dyn Fn(usize) -> Vec<String> + Send + Sync>>,
    submenu_item_index: Option<usize>,
    submenu_done: Option<Arc<AtomicBool>>,
    pub on_change: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
    pub on_cancel: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl SettingsList {
    pub fn new(
        items: Vec<SettingItem>,
        max_visible: usize,
        theme: SettingsListTheme,
        options: SettingsListOptions,
    ) -> Self {
        Self {
            filtered_items: items.clone(),
            items,
            theme,
            selected_index: 0,
            max_visible,
            search_enabled: options.enable_search,
            search_value: String::new(),
            submenu_component: None,
            submenu_item_index: None,
            submenu_done: None,
            on_change: None,
            on_cancel: None,
        }
    }

    pub fn update_value(&mut self, id: &str, new_value: &str) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.current_value = new_value.to_string();
        }
    }

    fn display_items(&self) -> &[SettingItem] {
        if self.search_enabled {
            &self.filtered_items
        } else {
            &self.items
        }
    }

    fn add_hint_line(&self, lines: &mut Vec<String>, width: f64) {
        lines.push(String::new());
        let hint = if self.search_enabled {
            "  Type to search · Enter/Space to change · Esc to cancel"
        } else {
            "  Enter/Space to change · Esc to cancel"
        };
        lines.push(truncate_to_width(&(self.theme.hint)(hint), width, "...", false));
    }

    fn activate_item(&mut self) {
        let index = self.selected_index;
        let item = self.display_items().get(index).cloned();
        let Some(item) = item else { return };

        if let Some(submenu) = item.submenu {
            self.submenu_item_index = Some(self.selected_index);
            let on_change = self.on_change.clone();
            let done_flag = Arc::new(AtomicBool::new(false));
            let done = {
                let flag = done_flag.clone();
                Arc::new(move |selected_value: Option<&str>| {
                    if let Some(selected_value) = selected_value {
                        if let Some(on_change) = &on_change {
                            on_change(&item.id, selected_value);
                        }
                    }
                    flag.store(true, Ordering::SeqCst);
                })
            };
            self.submenu_done = Some(done_flag);
            self.submenu_component = Some(submenu(&item.current_value, done));
        } else if let Some(values) = item.values {
            if !values.is_empty() {
                let current_index = values.iter().position(|value| *value == item.current_value).unwrap_or(0);
                let next_index = (current_index + 1) % values.len();
                let new_value = values[next_index].clone();
                if let Some(entry) = self.items.iter_mut().find(|entry| entry.id == item.id) {
                    entry.current_value = new_value.clone();
                }
                if let Some(on_change) = &self.on_change {
                    on_change(&item.id, &new_value);
                }
            }
        }
    }

    fn close_submenu(&mut self) {
        self.submenu_component = None;
        self.submenu_done = None;
        if let Some(index) = self.submenu_item_index.take() {
            self.selected_index = index;
        }
    }

    fn apply_filter(&mut self, query: &str) {
        let items = self.items.clone();
        self.filtered_items = fuzzy_filter(&items, query, |item| item.label.clone());
        self.selected_index = 0;
    }

    fn handle_input_with(&mut self, keybindings: &KeybindingsManager, data: &str) {
        if self.submenu_component.is_some() {
            // ponytail: submenu input is not delegated (the JS version
            // forwards to the submenu component); Escape closes the submenu.
            if let Some(on_cancel) = &self.on_cancel {
                let _ = on_cancel;
            }
            if keybindings.matches(data, "tui.select.cancel") {
                self.close_submenu();
            }
            return;
        }

        let display_items = self.display_items();
        if keybindings.matches(data, "tui.select.up") {
            if display_items.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index == 0 {
                display_items.len() - 1
            } else {
                self.selected_index - 1
            };
        } else if keybindings.matches(data, "tui.select.down") {
            if display_items.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index == display_items.len() - 1 {
                0
            } else {
                self.selected_index + 1
            };
        } else if keybindings.matches(data, "tui.select.confirm")
            || (data == " " && (!self.search_enabled || self.search_value.is_empty()))
        {
            self.activate_item();
        } else if keybindings.matches(data, "tui.select.cancel") {
            if let Some(on_cancel) = &self.on_cancel {
                on_cancel();
            }
        } else if self.search_enabled {
            // Simple search input handling: append printable chars.
            if data.chars().count() == 1 {
                self.search_value.push_str(data);
            } else if data == "\x7f" {
                self.search_value.pop();
            } else if data == "\x1b[Z" || data == "\t" {
                // tab accepted as-is
            }
            let search_value = self.search_value.clone();
            self.apply_filter(&search_value);
        }
    }
}

impl Component for SettingsList {
    fn render(&self, width: usize) -> Vec<String> {
        if let Some(submenu) = &self.submenu_component {
            return submenu(width);
        }
        let width = width as f64;
        let mut lines: Vec<String> = Vec::new();

        if self.search_enabled {
            lines.push(format!("  {}", self.search_value));
            lines.push(String::new());
        }

        if self.items.is_empty() {
            lines.push((self.theme.hint)("  No settings available"));
            self.add_hint_line(&mut lines, width);
            return lines;
        }

        let display_items = self.display_items();
        if display_items.is_empty() {
            lines.push(truncate_to_width(&(self.theme.hint)("  No matching settings"), width, "...", false));
            self.add_hint_line(&mut lines, width);
            return lines;
        }

        let max_visible = self.max_visible as f64;
        let start_index = (self.selected_index as f64 - (max_visible / 2.0).floor())
            .min(display_items.len() as f64 - max_visible)
            .max(0.0) as usize;
        let end_index = (start_index as f64 + max_visible).min(display_items.len() as f64) as usize;

        let max_label_width = self
            .items
            .iter()
            .map(|item| visible_width(&item.label))
            .fold(0.0, f64::max)
            .min(30.0);

        for index in start_index..end_index {
            let Some(item) = display_items.get(index) else { continue };
            let is_selected = index == self.selected_index;
            let prefix = if is_selected { &self.theme.cursor } else { "  " };
            let prefix_width = visible_width(prefix);

            let padding = ((max_label_width - visible_width(&item.label)).max(0.0)) as usize;
            let label_padded = format!("{}{}", item.label, " ".repeat(padding));
            let label_text = (self.theme.label)(&label_padded, is_selected);

            let separator = "  ";
            let used_width = prefix_width + max_label_width + visible_width(separator);
            let value_max_width = width - used_width - 2.0;
            let value_text = (self.theme.value)(&truncate_to_width(&item.current_value, value_max_width, "", false), is_selected);

            lines.push(truncate_to_width(&format!("{prefix}{label_text}{separator}{value_text}"), width, "...", false));
        }

        if start_index > 0 || end_index < display_items.len() {
            let scroll_text = format!("  ({}/{})", self.selected_index + 1, display_items.len());
            lines.push((self.theme.hint)(&truncate_to_width(&scroll_text, width - 2.0, "...", false)));
        }

        if let Some(description) = display_items.get(self.selected_index).and_then(|item| item.description.clone()) {
            lines.push(String::new());
            for line in wrap_text_with_ansi(&description, width - 4.0) {
                lines.push((self.theme.description)(&format!("  {line}")));
            }
        }

        self.add_hint_line(&mut lines, width);
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let keybindings = get_keybindings();
        let manager = match &*keybindings {
            Some(manager) => manager,
            None => {
                let manager = KeybindingsManager::new(crate::keybindings::tui_keybindings());
                self.handle_input_with(&manager, data);
                return;
            }
        };
        self.handle_input_with(manager, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Arc<dyn Fn(&str, bool) -> String + Send + Sync> {
        Arc::new(|text, _| text.to_string())
    }

    fn identity_text() -> Arc<dyn Fn(&str) -> String + Send + Sync> {
        Arc::new(|text| text.to_string())
    }

    fn theme() -> SettingsListTheme {
        SettingsListTheme {
            cursor: "→ ".to_string(),
            label: identity(),
            value: identity(),
            description: identity_text(),
            hint: identity_text(),
        }
    }

    fn items() -> Vec<SettingItem> {
        vec![
            SettingItem {
                id: "a".to_string(),
                label: "Alpha".to_string(),
                current_value: "1".to_string(),
                description: Some("first setting".to_string()),
                values: Some(vec!["1".to_string(), "2".to_string()]),
                submenu: None,
            },
            SettingItem {
                id: "b".to_string(),
                label: "Beta".to_string(),
                current_value: "x".to_string(),
                description: None,
                values: None,
                submenu: None,
            },
        ]
    }

    #[test]
    fn renders_items_aligned() {
        let list = SettingsList::new(items(), 5, theme(), SettingsListOptions::default());
        let lines = list.render(60);
        assert!(lines[0].starts_with("→ Alpha"));
        assert!(lines[1].starts_with("  Beta"));
    }

    #[test]
    fn empty_items_shows_hint() {
        let list = SettingsList::new(Vec::new(), 5, theme(), SettingsListOptions::default());
        let lines = list.render(60);
        assert!(lines.iter().any(|line| line.contains("No settings")));
    }

    #[test]
    fn cycles_values_on_confirm() {
        let mut list = SettingsList::new(items(), 5, theme(), SettingsListOptions::default());
        let changed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let changed_clone = changed.clone();
        list.on_change = Some(Arc::new(move |id, value| {
            changed_clone.lock().unwrap().push(format!("{id}={value}"));
        }));
        let keybindings = KeybindingsManager::new(crate::keybindings::tui_keybindings());
        list.handle_input_with(&keybindings, "\r");
        assert_eq!(changed.lock().unwrap().clone(), vec!["a=2".to_string()]);
        // Value updated in the item.
        assert_eq!(list.items[0].current_value, "2");
    }

    #[test]
    fn navigation_and_cancel() {
        let mut list = SettingsList::new(items(), 5, theme(), SettingsListOptions::default());
        let cancelled = Arc::new(std::sync::Mutex::new(false));
        let cancelled_clone = cancelled.clone();
        list.on_cancel = Some(Arc::new(move || {
            *cancelled_clone.lock().unwrap() = true;
        }));
        let keybindings = KeybindingsManager::new(crate::keybindings::tui_keybindings());
        list.handle_input_with(&keybindings, "\x1b[B");
        assert_eq!(list.selected_index, 1);
        list.handle_input_with(&keybindings, "\x1b");
        assert!(*cancelled.lock().unwrap());
    }

    #[test]
    fn search_filters() {
        let mut list = SettingsList::new(items(), 5, theme(), SettingsListOptions {
            enable_search: true,
        });
        let keybindings = KeybindingsManager::new(crate::keybindings::tui_keybindings());
        list.handle_input_with(&keybindings, "a");
        list.handle_input_with(&keybindings, "l");
        assert_eq!(list.filtered_items.len(), 1);
        assert_eq!(list.filtered_items[0].label, "Alpha");
    }
}
