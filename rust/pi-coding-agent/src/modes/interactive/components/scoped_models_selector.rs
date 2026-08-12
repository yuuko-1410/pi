//! Scoped models selector, port of `components/scoped-models-selector.ts`.
//!
//! ponytail: reordering/enable-all/clear-all/toggle-provider keybindings are
//! simplified to toggle on Enter; persistence is a callback the host wires.

use std::sync::Arc;

use pi_ai::types::Model;
use pi_tui::components::input::Input;
use pi_tui::fuzzy::fuzzy_filter;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::Component;

use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::keybinding_hints::key_text;
use crate::modes::interactive::theme::theme::theme;

pub struct ModelsConfig {
    pub all_models: Vec<Model>,
    pub enabled_model_ids: Option<Vec<String>>,
    pub refresh_status: Option<String>,
}

pub struct ModelsCallbacks {
    pub on_change: Arc<dyn Fn(Option<Vec<String>>) + Send + Sync>,
    pub on_persist: Arc<dyn Fn(Option<Vec<String>>) + Send + Sync>,
    pub on_cancel: Arc<dyn Fn() + Send + Sync>,
}

/// EnabledIds: None = all enabled, Some(list) = explicit ordered list.
fn is_enabled(enabled_ids: &Option<Vec<String>>, id: &str) -> bool {
    match enabled_ids {
        None => true,
        Some(list) => list.iter().any(|existing| existing == id),
    }
}

fn toggle(enabled_ids: &Option<Vec<String>>, id: &str) -> Option<Vec<String>> {
    match enabled_ids {
        None => Some(vec![id.to_string()]),
        Some(list) => {
            if list.iter().any(|existing| existing == id) {
                let mut result = list.clone();
                result.retain(|existing| existing != id);
                Some(result)
            } else {
                let mut result = list.clone();
                result.push(id.to_string());
                Some(result)
            }
        }
    }
}

fn get_sorted_ids(enabled_ids: &Option<Vec<String>>, all_ids: &[String]) -> Vec<String> {
    match enabled_ids {
        None => all_ids.to_vec(),
        Some(list) => {
            let mut result = list.clone();
            for id in all_ids {
                if !result.contains(id) {
                    result.push(id.clone());
                }
            }
            result
        }
    }
}

pub struct ScopedModelsSelectorComponent {
    models_by_id: std::collections::HashMap<String, Model>,
    all_ids: Vec<String>,
    enabled_ids: Option<Vec<String>>,
    filtered_items: Vec<(String, Option<Model>, bool)>,
    selected_index: usize,
    search_input: Arc<Input>,
    callbacks: ModelsCallbacks,
    is_dirty: bool,
    focused: bool,
    max_visible: usize,
}

impl ScopedModelsSelectorComponent {
    pub fn new(config: ModelsConfig, callbacks: ModelsCallbacks) -> Self {
        let mut models_by_id = std::collections::HashMap::new();
        let mut all_ids: Vec<String> = Vec::new();
        for model in &config.all_models {
            let full_id = format!("{}/{}", model.provider, model.id);
            models_by_id.insert(full_id.clone(), model.clone());
            all_ids.push(full_id);
        }
        let mut component = Self {
            models_by_id,
            all_ids,
            enabled_ids: config.enabled_model_ids.clone(),
            filtered_items: Vec::new(),
            selected_index: 0,
            search_input: Arc::new(Input::new()),
            callbacks,
            is_dirty: false,
            focused: false,
            max_visible: 10,
        };
        component.refresh();
        component
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

    fn build_items(&self) -> Vec<(String, Option<Model>, bool)> {
        get_sorted_ids(&self.enabled_ids, &self.all_ids)
            .into_iter()
            .map(|id| {
                let model = self.models_by_id.get(&id).cloned();
                let enabled = is_enabled(&self.enabled_ids, &id);
                (id, model, enabled)
            })
            .collect()
    }

    fn refresh(&mut self) {
        let query = self.search_input.get_value().to_string();
        let items = self.build_items();
        self.filtered_items = if query.is_empty() {
            items
        } else {
            fuzzy_filter(&items, &query, |(id, model, _)| match model {
                Some(model) => format!("{} {} {}/{}", model.id, model.provider, model.provider, model.id),
                None => id.clone(),
            })
        };
        self.selected_index = self.selected_index.min(self.filtered_items.len().saturating_sub(1));
    }

    #[allow(dead_code)]
    fn notify_change(&self) {
        (self.callbacks.on_change)(self.enabled_ids.clone());
    }

    fn get_footer_text(&self) -> String {
        let enabled_count = self
            .enabled_ids
            .as_ref()
            .map(|list| list.iter().filter(|id| self.models_by_id.contains_key(*id)).count())
            .unwrap_or(self.all_ids.len());
        let all_enabled = self.enabled_ids.is_none();
        let count_text = if all_enabled {
            "all enabled".to_string()
        } else {
            format!("{enabled_count}/{} enabled", self.all_ids.len())
        };
        let parts = [
            format!("{} toggle", key_text("tui.select.confirm")),
            format!("{} save", key_text("app.models.save")),
            count_text,
        ];
        let base = format!("  {}", parts.join(" · "));
        if self.is_dirty {
            format!(
                "{}{}",
                theme().as_ref().map(|t| t.fg("dim", &base)).unwrap_or(base),
                theme()
                    .as_ref()
                    .map(|t| t.fg("warning", " (unsaved)"))
                    .unwrap_or_else(|| " (unsaved)".to_string())
            )
        } else {
            theme().as_ref().map(|t| t.fg("dim", &base)).unwrap_or(base)
        }
    }
}

impl Component for ScopedModelsSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        let accent = |text: &str| t.map(|t| t.fg("accent", text)).unwrap_or_else(|| text.to_string());
        let muted = |text: &str| t.map(|t| t.fg("muted", text)).unwrap_or_else(|| text.to_string());
        let bold = |text: &str| t.map(|t| t.bold(text)).unwrap_or_else(|| text.to_string());

        let mut lines: Vec<String> = Vec::new();
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines.push(format!(" {}", bold(&accent("Model Configuration"))));
        lines.push(format!(
            " {}",
            muted(&format!("Session-only. {} to save to settings.", key_text("app.models.save")))
        ));
        lines.extend(self.search_input.render(width));

        if self.filtered_items.is_empty() {
            lines.push(format!(" {}", muted("  No matching models")));
        } else {
            let start_index = (self.selected_index as isize - (self.max_visible as isize / 2))
                .max(0)
                .min((self.filtered_items.len() as isize - self.max_visible as isize).max(0))
                .max(0) as usize;
            let end_index = (start_index + self.max_visible).min(self.filtered_items.len());
            let all_enabled = self.enabled_ids.is_none();

            for i in start_index..end_index {
                let (id, model, enabled) = &self.filtered_items[i];
                let is_selected = i == self.selected_index;
                let prefix = if is_selected { accent("→ ") } else { "  ".to_string() };
                let display_id = model.as_ref().map(|m| m.id.clone()).unwrap_or_else(|| id.clone());
                let model_text = if is_selected { accent(&display_id) } else { display_id };
                let provider_badge = muted(&format!(" [{}]", model.as_ref().map(|m| m.provider.as_str()).unwrap_or("unavailable")));
                let status = if let Some(_model) = model {
                    if all_enabled {
                        String::new()
                    } else if *enabled {
                        t.map(|t| t.fg("success", " ✓")).unwrap_or_else(|| " ✓".to_string())
                    } else {
                        t.map(|t| t.fg("dim", " ✗")).unwrap_or_else(|| " ✗".to_string())
                    }
                } else {
                    t.map(|t| t.fg("dim", " ✗")).unwrap_or_else(|| " ✗".to_string())
                };
                lines.push(format!(" {prefix}{model_text}{provider_badge}{status}"));
            }

            if start_index > 0 || end_index < self.filtered_items.len() {
                let scroll_info = format!("  ({}/{})", self.selected_index + 1, self.filtered_items.len());
                lines.push(format!(" {}", muted(&scroll_info)));
            }
        }

        lines.push(format!(" {}", self.get_footer_text()));
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = get_keybindings();
        let manager = match &*kb {
            Some(manager) => manager,
            None => return,
        };
        if manager.matches(data, "tui.select.up") {
            if self.filtered_items.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index == 0 {
                self.filtered_items.len() - 1
            } else {
                self.selected_index - 1
            };
        } else if manager.matches(data, "tui.select.down") {
            if self.filtered_items.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index + 1 >= self.filtered_items.len() {
                0
            } else {
                self.selected_index + 1
            };
        } else if manager.matches(data, "tui.select.confirm") {
            if let Some((id, _, _)) = self.filtered_items.get(self.selected_index) {
                self.enabled_ids = toggle(&self.enabled_ids, id);
                self.is_dirty = true;
                self.refresh();
                self.notify_change();
            }
        } else if manager.matches(data, "app.models.save") {
            (self.callbacks.on_persist)(self.enabled_ids.clone());
            self.is_dirty = false;
        } else if manager.matches(data, "tui.select.cancel") {
            (self.callbacks.on_cancel)();
        } else if let Some(input) = Arc::get_mut(&mut self.search_input) {
            input.handle_input(data);
            self.refresh();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_logic() {
        assert_eq!(toggle(&None, "a/b"), Some(vec!["a/b".to_string()]));
        assert_eq!(toggle(&Some(vec!["a/b".to_string()]), "a/b"), Some(vec![]));
        assert_eq!(toggle(&Some(vec!["a/b".to_string()]), "c/d"), Some(vec!["a/b".to_string(), "c/d".to_string()]));
    }

    #[test]
    fn sorted_ids_puts_enabled_first() {
        let sorted = get_sorted_ids(&Some(vec!["b".to_string()]), &["a".to_string(), "b".to_string()]);
        assert_eq!(sorted, vec!["b", "a"]);
    }

    #[test]
    fn renders_config() {
        let component = ScopedModelsSelectorComponent::new(
            ModelsConfig {
                all_models: vec![],
                enabled_model_ids: None,
                refresh_status: None,
            },
            ModelsCallbacks {
                on_change: Arc::new(|_| {}),
                on_persist: Arc::new(|_| {}),
                on_cancel: Arc::new(|| {}),
            },
        );
        let lines = component.render(60);
        assert!(lines.iter().any(|line| line.contains("Model Configuration")));
    }
}
