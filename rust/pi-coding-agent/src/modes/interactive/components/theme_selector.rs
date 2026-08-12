//! Theme selector component, port of `components/theme-selector.ts`.

use std::sync::Arc;

use pi_tui::tui::Container;
use pi_tui::components::select_list::{SelectItem, SelectList, SelectListLayoutOptions};
use pi_tui::tui::Component;

use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::theme::theme::{get_available_themes, get_select_list_theme};

const THEME_SELECT_LIST_LAYOUT: SelectListLayoutOptions = SelectListLayoutOptions {
    min_primary_column_width: Some(12.0),
    max_primary_column_width: Some(32.0),
    truncate_primary: None,
};

/// Component that renders a theme selector.
pub struct ThemeSelectorComponent {
    container: Container,
    select_list: Arc<SelectList>,
}

impl ThemeSelectorComponent {
    pub fn new(
        current_theme: &str,
        on_select: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        on_preview: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Self {
        let mut container = Container::new();
        container.add_child(Arc::new(DynamicBorder::new(None)));

        let themes = get_available_themes();
        let theme_items: Vec<SelectItem> = themes
            .iter()
            .map(|name| SelectItem {
                value: name.clone(),
                label: name.clone(),
                description: if *name == current_theme { Some("(current)".to_string()) } else { None },
            })
            .collect();

        let mut select_list = SelectList::new(
            theme_items,
            10,
            get_select_list_theme(),
            THEME_SELECT_LIST_LAYOUT,
        );

        if let Some(current_index) = themes.iter().position(|name| name == current_theme) {
            select_list.set_selected_index(current_index);
        }

        select_list.on_select = Some(Arc::new(move |item| on_select(&item.value)));
        select_list.on_cancel = Some(on_cancel.clone());
        let preview = on_preview.clone();
        select_list.on_selection_change = Some(Arc::new(move |item| preview(&item.value)));

        let select_list_arc = Arc::new(select_list);
        container.add_child(select_list_arc.clone());
        container.add_child(Arc::new(DynamicBorder::new(None)));

        Self { container, select_list: select_list_arc }
    }

    pub fn get_select_list(&self) -> &SelectList {
        &self.select_list
    }
}

impl Component for ThemeSelectorComponent {
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
    fn renders_theme_selector() {
        let component = ThemeSelectorComponent::new(
            "dark",
            Arc::new(|_| {}),
            Arc::new(|| {}),
            Arc::new(|_| {}),
        );
        let lines = component.render(60);
        assert!(!lines.is_empty());
        // Border lines at top and bottom.
        assert!(lines[0].contains('─'));
    }
}
