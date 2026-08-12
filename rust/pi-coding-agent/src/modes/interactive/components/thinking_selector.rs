//! Thinking level selector component, port of `components/thinking-selector.ts`.

use std::sync::Arc;

use pi_tui::tui::Container;
use pi_tui::components::select_list::{SelectItem, SelectList, SelectListLayoutOptions};
use pi_tui::tui::Component;

use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::theme::theme::get_select_list_theme;

const THINKING_SELECT_LIST_LAYOUT: SelectListLayoutOptions = SelectListLayoutOptions {
    min_primary_column_width: Some(12.0),
    max_primary_column_width: Some(32.0),
    truncate_primary: None,
};

const LEVEL_DESCRIPTIONS: &[(&str, &str)] = &[
    ("off", "No reasoning"),
    ("minimal", "Very brief reasoning (~1k tokens)"),
    ("low", "Light reasoning (~2k tokens)"),
    ("medium", "Moderate reasoning (~8k tokens)"),
    ("high", "Deep reasoning (~16k tokens)"),
    ("xhigh", "Extra-high reasoning (~32k tokens)"),
    ("max", "Maximum reasoning"),
];

fn level_description(level: &str) -> &str {
    LEVEL_DESCRIPTIONS
        .iter()
        .find(|(name, _)| *name == level)
        .map(|(_, description)| *description)
        .unwrap_or("")
}

/// Component that renders a thinking level selector with borders.
pub struct ThinkingSelectorComponent {
    container: Container,
    select_list: Arc<SelectList>,
}

impl ThinkingSelectorComponent {
    pub fn new(
        current_level: &str,
        available_levels: &[String],
        on_select: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let mut container = Container::new();
        container.add_child(Arc::new(DynamicBorder::new(None)));

        let thinking_levels: Vec<SelectItem> = available_levels
            .iter()
            .map(|level| SelectItem {
                value: level.clone(),
                label: level.clone(),
                description: Some(level_description(level).to_string()),
            })
            .collect();

        let item_count = thinking_levels.len();
        let mut select_list = SelectList::new(
            thinking_levels,
            item_count,
            get_select_list_theme(),
            THINKING_SELECT_LIST_LAYOUT,
        );

        if let Some(current_index) = available_levels.iter().position(|level| level == current_level) {
            select_list.set_selected_index(current_index);
        }

        let on_select = on_select.clone();
        select_list.on_select = Some(Arc::new(move |item| on_select(&item.value)));
        select_list.on_cancel = Some(on_cancel);

        let select_list_arc = Arc::new(select_list);
        container.add_child(select_list_arc.clone());
        container.add_child(Arc::new(DynamicBorder::new(None)));

        Self { container, select_list: select_list_arc }
    }

    pub fn get_select_list(&self) -> &SelectList {
        &self.select_list
    }
}

impl Component for ThinkingSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        if let Some(list) = Arc::get_mut(&mut self.select_list) {
            list.handle_input(data);
        }
    }

    fn invalidate(&mut self) {
        if let Some(list) = Arc::get_mut(&mut self.select_list) {
            list.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_thinking_selector() {
        let levels = vec![
            "off".to_string(),
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
        ];
        let component = ThinkingSelectorComponent::new(
            "medium",
            &levels,
            Arc::new(|_| {}),
            Arc::new(|| {}),
        );
        let lines = component.render(60);
        assert!(!lines.is_empty());
        assert!(lines[0].contains('─'));
    }
}
