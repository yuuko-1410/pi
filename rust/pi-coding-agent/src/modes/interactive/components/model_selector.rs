//! Model selector with search, port of `components/model-selector.ts`.

use std::sync::Arc;

use pi_ai::types::Model;
use pi_tui::components::input::Input;
use pi_tui::fuzzy::fuzzy_filter;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::Component;

use crate::core::model_runtime::ModelRuntime;
use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::keybinding_hints::key_hint;
use crate::modes::interactive::theme::theme::theme;

fn model_search_text(model: &Model) -> String {
    format!("{} {}/{} {} {}", model.provider, model.provider, model.id, model.provider, model.id)
}

pub struct ModelSelectorComponent {
    search_input: Arc<Input>,
    all_models: Vec<Model>,
    active_models: Vec<Model>,
    filtered_models: Vec<Model>,
    selected_index: usize,
    current_model: Option<Model>,
    on_select: Arc<dyn Fn(Model) + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
    error_message: Option<String>,
    focused: bool,
}

impl ModelSelectorComponent {
    pub fn new(
        current_model: Option<Model>,
        model_runtime: &ModelRuntime,
        scoped_models: &[crate::core::model_resolver::ScopedModel],
        on_select: Arc<dyn Fn(Model) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        initial_search_input: Option<&str>,
    ) -> Self {
        let mut all_models = model_runtime.get_available_snapshot();
        // Sort: current model first, then by provider.
        all_models.sort_by(|a, b| {
            let a_is_current = models_equal(current_model.as_ref(), Some(a));
            let b_is_current = models_equal(current_model.as_ref(), Some(b));
            if a_is_current && !b_is_current {
                return std::cmp::Ordering::Less;
            }
            if !a_is_current && b_is_current {
                return std::cmp::Ordering::Greater;
            }
            a.provider.cmp(&b.provider)
        });
        let active_models = if scoped_models.is_empty() {
            all_models.clone()
        } else {
            scoped_models.iter().map(|scoped| scoped.model.clone()).collect()
        };
        let mut component = Self {
            search_input: Arc::new(Input::new()),
            all_models,
            active_models,
            filtered_models: Vec::new(),
            selected_index: 0,
            current_model,
            on_select,
            on_cancel,
            error_message: model_runtime.get_error(),
            focused: false,
        };
        let initial = initial_search_input.unwrap_or("").to_string();
        if !initial.is_empty() {
            component.filter_models(&initial);
        } else {
            component.update_list();
        }
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

    fn filter_models(&mut self, query: &str) {
        self.filtered_models = if query.is_empty() {
            self.active_models.clone()
        } else {
            fuzzy_filter(&self.active_models, query, model_search_text)
        };
        self.selected_index = if query.is_empty() {
            self.selected_index.min(self.filtered_models.len().saturating_sub(1))
        } else {
            0
        };
    }

    fn update_list(&mut self) {
        // Filtered list mirrors active models when no query.
        self.filtered_models = self.active_models.clone();
    }

    fn handle_select(&mut self, model: &Model) {
        (self.on_select)(model.clone());
    }
}

fn models_equal(a: Option<&Model>, b: Option<&Model>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.id == b.id && a.provider == b.provider,
        _ => false,
    }
}

impl Component for ModelSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        let accent = |text: &str| t.map(|t| t.fg("accent", text)).unwrap_or_else(|| text.to_string());
        let muted = |text: &str| t.map(|t| t.fg("muted", text)).unwrap_or_else(|| text.to_string());
        let bold = |text: &str| t.map(|t| t.bold(text)).unwrap_or_else(|| text.to_string());

        let hint = if self.active_models.len() < self.all_models.len() {
            "Only showing models from configured providers. Use /login to add providers."
        } else {
            ""
        };

        let mut lines: Vec<String> = Vec::new();
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        if !hint.is_empty() {
            let styled = t.map(|t| t.fg("warning", hint)).unwrap_or_else(|| hint.to_string());
            lines.push(format!(" {styled}"));
        }
        lines.extend(self.search_input.render(width));

        if self.filtered_models.is_empty() {
            let message = if let Some(error) = &self.error_message {
                let styled = t.map(|t| t.fg("error", error)).unwrap_or_else(|| error.clone());
                format!(" {styled}")
            } else {
                let styled = muted("  No matching models");
                format!(" {styled}")
            };
            lines.push(message);
        } else {
            let max_visible = 10;
            let start_index = (self.selected_index as isize - (max_visible as isize / 2))
                .max(0)
                .min((self.filtered_models.len() as isize - max_visible as isize).max(0))
                .max(0) as usize;
            let end_index = (start_index + max_visible).min(self.filtered_models.len());

            for i in start_index..end_index {
                let model = &self.filtered_models[i];
                let is_selected = i == self.selected_index;
                let is_current = models_equal(self.current_model.as_ref(), Some(model));

                let prefix = if is_selected { accent("→ ") } else { "  ".to_string() };
                let model_text = if is_selected {
                    accent(&model.id)
                } else {
                    model.id.clone()
                };
                let provider_badge = muted(&format!("[{}]", model.provider));
                let checkmark = if is_current {
                    t.map(|t| t.fg("success", " ✓")).unwrap_or_else(|| " ✓".to_string())
                } else {
                    String::new()
                };
                lines.push(format!(" {prefix}{model_text} {provider_badge}{checkmark}"));
            }

            if start_index > 0 || end_index < self.filtered_models.len() {
                let scroll_info = format!("  ({}/{})", self.selected_index + 1, self.filtered_models.len());
                lines.push(format!(" {}", muted(&scroll_info)));
            }

            if let Some(selected) = self.filtered_models.get(self.selected_index) {
                let name = muted(&format!("  Model Name: {}", selected.name));
                lines.push(format!(" {name}"));
            }
        }

        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        let _ = bold;
        let _ = key_hint;
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = get_keybindings();
        let manager = match &*kb {
            Some(manager) => manager,
            None => return,
        };
        if manager.matches(data, "tui.select.up") {
            if self.filtered_models.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index == 0 {
                self.filtered_models.len() - 1
            } else {
                self.selected_index - 1
            };
        } else if manager.matches(data, "tui.select.down") {
            if self.filtered_models.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index + 1 >= self.filtered_models.len() {
                0
            } else {
                self.selected_index + 1
            };
        } else if manager.matches(data, "tui.select.confirm") {
            if let Some(selected) = self.filtered_models.get(self.selected_index).cloned() {
                self.handle_select(&selected);
            }
        } else if manager.matches(data, "tui.select.cancel") {
            (self.on_cancel)();
        } else if let Some(input) = Arc::get_mut(&mut self.search_input) {
            input.handle_input(data);
            let query = input.get_value().to_string();
            self.filter_models(&query);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_model(id: &str, provider: &str) -> Model {
        Model {
            id: id.to_string(),
            name: id.to_string(),
            api: "openai".to_string(),
            provider: provider.to_string(),
            base_url: String::new(),
            reasoning: false,
            thinking_level_map: None,
            input: Vec::new(),
            cost: pi_ai::types::ModelCost {
                rates: pi_ai::types::ModelCostRates {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                tiers: None,
            },
            context_window: 0.0,
            max_tokens: 0.0,
            sampling_params: None,
            headers: None,
            compat: None,
        }
    }

    #[test]
    fn search_text_ranks_provider_prefix() {
        let model = make_model("gpt-5", "openrouter/openai");
        assert!(model_search_text(&model).starts_with("openrouter/openai"));
    }
}
