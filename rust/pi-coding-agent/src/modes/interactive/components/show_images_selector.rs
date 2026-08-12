//! Show-images selector component, port of `components/show-images-selector.ts`.

use std::sync::Arc;

use pi_tui::tui::Container;
use pi_tui::components::select_list::{SelectItem, SelectList, SelectListLayoutOptions};
use pi_tui::tui::Component;

use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::theme::theme::get_select_list_theme;

const SHOW_IMAGES_SELECT_LIST_LAYOUT: SelectListLayoutOptions = SelectListLayoutOptions {
    min_primary_column_width: Some(12.0),
    max_primary_column_width: Some(32.0),
    truncate_primary: None,
};

/// Component that renders a show-images selector with borders.
pub struct ShowImagesSelectorComponent {
    container: Container,
    select_list: Arc<SelectList>,
}

impl ShowImagesSelectorComponent {
    pub fn new(current_value: bool, on_select: Arc<dyn Fn(bool) + Send + Sync>, on_cancel: Arc<dyn Fn() + Send + Sync>) -> Self {
        let mut container = Container::new();
        container.add_child(Arc::new(DynamicBorder::new(None)));

        let items: Vec<SelectItem> = vec![
            SelectItem {
                value: "yes".to_string(),
                label: "Yes".to_string(),
                description: Some("Show images inline in terminal".to_string()),
            },
            SelectItem {
                value: "no".to_string(),
                label: "No".to_string(),
                description: Some("Show text placeholder instead".to_string()),
            },
        ];

        let mut select_list = SelectList::new(items, 5, get_select_list_theme(), SHOW_IMAGES_SELECT_LIST_LAYOUT);
        select_list.set_selected_index(if current_value { 0 } else { 1 });

        let on_select = on_select.clone();
        select_list.on_select = Some(Arc::new(move |item| on_select(item.value == "yes")));
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

impl Component for ShowImagesSelectorComponent {
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
    fn preselects_current_value() {
        let component = ShowImagesSelectorComponent::new(true, Arc::new(|_| {}), Arc::new(|| {}));
        assert_eq!(component.select_list.get_selected_item().unwrap().value, "yes");

        let component2 = ShowImagesSelectorComponent::new(false, Arc::new(|_| {}), Arc::new(|| {}));
        assert_eq!(component2.select_list.get_selected_item().unwrap().value, "no");
    }
}
