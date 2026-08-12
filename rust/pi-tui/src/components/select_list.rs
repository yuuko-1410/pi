//! Select list component, port of `packages/tui/src/components/select-list.ts`.

use std::sync::Arc;

use crate::keybindings::{get_keybindings, KeybindingsManager};
use crate::tui::Component;
use crate::utils::{truncate_to_width, visible_width};

const DEFAULT_PRIMARY_COLUMN_WIDTH: f64 = 32.0;
const PRIMARY_COLUMN_GAP: f64 = 2.0;
const MIN_DESCRIPTION_WIDTH: f64 = 10.0;

fn normalize_to_single_line(text: &str) -> String {
    text.split(|c| c == '\r' || c == '\n')
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn clamp(value: f64, min: f64, max: f64) -> f64 {
    min.max(value.min(max))
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

pub struct SelectListTheme {
    pub selected_prefix: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub selected_text: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub description: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub scroll_info: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub no_match: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

pub struct SelectListLayoutOptions {
    pub min_primary_column_width: Option<f64>,
    pub max_primary_column_width: Option<f64>,
    pub truncate_primary: Option<Arc<dyn Fn(&str, f64, f64, bool) -> String + Send + Sync>>,
}

impl Default for SelectListLayoutOptions {
    fn default() -> Self {
        Self {
            min_primary_column_width: None,
            max_primary_column_width: None,
            truncate_primary: None,
        }
    }
}

pub struct SelectList {
    items: Vec<SelectItem>,
    filtered_items: Vec<SelectItem>,
    selected_index: usize,
    max_visible: usize,
    theme: SelectListTheme,
    layout: SelectListLayoutOptions,
    pub on_select: Option<Arc<dyn Fn(&SelectItem) + Send + Sync>>,
    pub on_cancel: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_selection_change: Option<Arc<dyn Fn(&SelectItem) + Send + Sync>>,
}

impl SelectList {
    pub fn new(items: Vec<SelectItem>, max_visible: usize, theme: SelectListTheme, layout: SelectListLayoutOptions) -> Self {
        Self {
            filtered_items: items.clone(),
            items,
            selected_index: 0,
            max_visible,
            theme,
            layout,
            on_select: None,
            on_cancel: None,
            on_selection_change: None,
        }
    }

    pub fn set_filter(&mut self, filter: &str) {
        self.filtered_items = self
            .items
            .iter()
            .filter(|item| item.value.to_lowercase().starts_with(&filter.to_lowercase()))
            .cloned()
            .collect();
        self.selected_index = 0;
    }

    pub fn set_selected_index(&mut self, index: usize) {
        let max = self.filtered_items.len().saturating_sub(1);
        self.selected_index = 0.max(index.min(max));
    }

    pub fn get_selected_item(&self) -> Option<&SelectItem> {
        self.filtered_items.get(self.selected_index)
    }

    fn notify_selection_change(&self) {
        if let Some(item) = self.get_selected_item() {
            if let Some(callback) = &self.on_selection_change {
                callback(item);
            }
        }
    }

    fn get_primary_column_width(&self) -> f64 {
        let (min, max) = self.get_primary_column_bounds();
        let widest_primary = self
            .filtered_items
            .iter()
            .map(|item| visible_width(&self.get_display_value(item)) + PRIMARY_COLUMN_GAP)
            .fold(0.0, f64::max);
        clamp(widest_primary, min, max)
    }

    fn get_primary_column_bounds(&self) -> (f64, f64) {
        let raw_min = self
            .layout
            .min_primary_column_width
            .or(self.layout.max_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        let raw_max = self
            .layout
            .max_primary_column_width
            .or(self.layout.min_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        (
            raw_min.min(raw_max).max(1.0),
            raw_max.max(raw_min).max(1.0),
        )
    }

    fn truncate_primary(&self, item: &SelectItem, is_selected: bool, max_width: f64, column_width: f64) -> String {
        let display_value = self.get_display_value(item);
        let truncated = match &self.layout.truncate_primary {
            Some(truncate) => truncate(&display_value, max_width, column_width, is_selected),
            None => truncate_to_width(&display_value, max_width, "", false),
        };
        truncate_to_width(&truncated, max_width, "", false)
    }

    fn get_display_value(&self, item: &SelectItem) -> String {
        if item.label.is_empty() {
            item.value.clone()
        } else {
            item.label.clone()
        }
    }

    fn render_item(
        &self,
        item: &SelectItem,
        is_selected: bool,
        width: f64,
        description_single_line: Option<&str>,
        primary_column_width: f64,
    ) -> String {
        let prefix = if is_selected { "→ " } else { "  " };
        let prefix_width = visible_width(prefix);

        if let Some(description_single_line) = description_single_line {
            if width > 40.0 {
                let effective_primary_column_width =
                    primary_column_width.min(width - prefix_width - 4.0).max(1.0);
                let max_primary_width = (effective_primary_column_width - PRIMARY_COLUMN_GAP).max(1.0);
                let truncated_value = self.truncate_primary(item, is_selected, max_primary_width, effective_primary_column_width);
                let truncated_value_width = visible_width(&truncated_value);
                let spacing = " ".repeat((effective_primary_column_width - truncated_value_width).max(1.0) as usize);
                let description_start = prefix_width + truncated_value_width + spacing.len() as f64;
                let remaining_width = width - description_start - 2.0;
                if remaining_width > MIN_DESCRIPTION_WIDTH {
                    let truncated_desc = truncate_to_width(description_single_line, remaining_width, "", false);
                    if is_selected {
                        return (self.theme.selected_text)(&format!("{prefix}{truncated_value}{spacing}{truncated_desc}"));
                    }
                    let desc_text = (self.theme.description)(&format!("{spacing}{truncated_desc}"));
                    return format!("{prefix}{truncated_value}{desc_text}");
                }
            }
        }

        let max_width = width - prefix_width - 2.0;
        let truncated_value = self.truncate_primary(item, is_selected, max_width, max_width);
        if is_selected {
            return (self.theme.selected_text)(&format!("{prefix}{truncated_value}"));
        }
        format!("{prefix}{truncated_value}")
    }
}

impl Component for SelectList {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        if self.filtered_items.is_empty() {
            lines.push((self.theme.no_match)("  No matching commands"));
            return lines;
        }
        let width = width as f64;
        let primary_column_width = self.get_primary_column_width();
        let max_visible = self.max_visible as f64;
        let start_index = (self.selected_index as f64 - (max_visible / 2.0).floor())
            .min(self.filtered_items.len() as f64 - max_visible)
            .max(0.0) as usize;
        let end_index = (start_index as f64 + max_visible).min(self.filtered_items.len() as f64) as usize;

        for index in start_index..end_index {
            let item = self.filtered_items.get(index).cloned();
            let Some(item) = item else { continue };
            let is_selected = index == self.selected_index;
            let description_single_line = item
                .description
                .as_ref()
                .map(|description| normalize_to_single_line(description));
            lines.push(self.render_item(
                &item,
                is_selected,
                width,
                description_single_line.as_deref(),
                primary_column_width,
            ));
        }

        if start_index > 0 || end_index < self.filtered_items.len() {
            let scroll_text = format!("  ({}/{})", self.selected_index + 1, self.filtered_items.len());
            lines.push((self.theme.scroll_info)(&truncate_to_width(&scroll_text, width - 2.0, "", false)));
        }
        lines
    }

    fn handle_input(&mut self, key_data: &str) {
        let keybindings = get_keybindings();
        let manager = match &*keybindings {
            Some(manager) => manager,
            None => {
                let manager = KeybindingsManager::new(crate::keybindings::tui_keybindings());
                let _ = manager;
                let manager = KeybindingsManager::new(crate::keybindings::tui_keybindings());
                // Temporary manager; matches evaluated below.
                self.handle_input_with(&manager, key_data);
                return;
            }
        };
        self.handle_input_with(manager, key_data);
    }
}

impl SelectList {
    fn handle_input_with(&mut self, keybindings: &KeybindingsManager, key_data: &str) {
        if keybindings.matches(key_data, "tui.select.up") {
            self.selected_index = if self.selected_index == 0 {
                self.filtered_items.len().saturating_sub(1)
            } else {
                self.selected_index - 1
            };
            self.notify_selection_change();
        } else if keybindings.matches(key_data, "tui.select.down") {
            self.selected_index = if self.selected_index == self.filtered_items.len().saturating_sub(1) {
                0
            } else {
                self.selected_index + 1
            };
            self.notify_selection_change();
        } else if keybindings.matches(key_data, "tui.select.confirm") {
            if let Some(item) = self.get_selected_item().cloned() {
                if let Some(callback) = &self.on_select {
                    callback(&item);
                }
            }
        } else if keybindings.matches(key_data, "tui.select.cancel") {
            if let Some(callback) = &self.on_cancel {
                callback();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Arc<dyn Fn(&str) -> String + Send + Sync> {
        Arc::new(|text| text.to_string())
    }

    fn theme() -> SelectListTheme {
        SelectListTheme {
            selected_prefix: identity(),
            selected_text: identity(),
            description: identity(),
            scroll_info: identity(),
            no_match: identity(),
        }
    }

    fn items() -> Vec<SelectItem> {
        vec![
            SelectItem {
                value: "alpha".to_string(),
                label: "alpha".to_string(),
                description: None,
            },
            SelectItem {
                value: "beta".to_string(),
                label: "beta".to_string(),
                description: Some("second command".to_string()),
            },
            SelectItem {
                value: "gamma".to_string(),
                label: "gamma".to_string(),
                description: None,
            },
        ]
    }

    #[test]
    fn renders_items_with_selection_prefix() {
        let list = SelectList::new(items(), 5, theme(), SelectListLayoutOptions::default());
        let lines = list.render(60);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("→ "));
        assert!(lines[1].starts_with("  "));
    }

    #[test]
    fn filter_filters_by_prefix() {
        let mut list = SelectList::new(items(), 5, theme(), SelectListLayoutOptions::default());
        list.set_filter("be");
        assert_eq!(list.filtered_items.len(), 1);
        assert_eq!(list.get_selected_item().unwrap().value, "beta");
        list.set_filter("zz");
        let lines = list.render(60);
        assert!(lines[0].contains("No matching"));
    }

    #[test]
    fn navigation_wraps() {
        let mut list = SelectList::new(items(), 5, theme(), SelectListLayoutOptions::default());
        let selected = Arc::new(std::sync::Mutex::new(Vec::new()));
        let selected_clone = selected.clone();
        list.on_selection_change = Some(Arc::new(move |item| {
            selected_clone.lock().unwrap().push(item.value.clone());
        }));
        let keybindings = KeybindingsManager::new(crate::keybindings::tui_keybindings());
        list.handle_input_with(&keybindings, "\x1b[B"); // down
        assert_eq!(list.selected_index, 1);
        list.handle_input_with(&keybindings, "\x1b[A"); // up
        assert_eq!(list.selected_index, 0);
        list.handle_input_with(&keybindings, "\x1b[A"); // up wraps to last
        assert_eq!(list.selected_index, 2);
    }

    #[test]
    fn confirm_and_cancel_callbacks() {
        let mut list = SelectList::new(items(), 5, theme(), SelectListLayoutOptions::default());
        let confirmed = Arc::new(std::sync::Mutex::new(None::<String>));
        let confirmed_clone = confirmed.clone();
        list.on_select = Some(Arc::new(move |item| {
            *confirmed_clone.lock().unwrap() = Some(item.value.clone());
        }));
        let cancelled = Arc::new(std::sync::Mutex::new(false));
        let cancelled_clone = cancelled.clone();
        list.on_cancel = Some(Arc::new(move || {
            *cancelled_clone.lock().unwrap() = true;
        }));
        let keybindings = KeybindingsManager::new(crate::keybindings::tui_keybindings());
        list.handle_input_with(&keybindings, "\r");
        assert_eq!(confirmed.lock().unwrap().as_deref(), Some("alpha"));
        list.handle_input_with(&keybindings, "\x1b");
        assert!(*cancelled.lock().unwrap());
    }

    #[test]
    fn scroll_info_shown_when_scrolling_needed() {
        let many: Vec<SelectItem> = (0..20)
            .map(|index| SelectItem {
                value: format!("item{index}"),
                label: format!("item{index}"),
                description: None,
            })
            .collect();
        let mut list = SelectList::new(many, 5, theme(), SelectListLayoutOptions::default());
        let lines = list.render(60);
        assert!(lines.iter().any(|line| line.contains('/')));
        list.set_selected_index(19);
        let lines = list.render(60);
        assert!(lines.iter().any(|line| line.contains("20/20")));
    }
}
