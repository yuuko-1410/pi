//! User message selector for branching, port of
//! `components/user-message-selector.ts`.

use std::sync::Arc;

use pi_tui::components::basic::Text;
use pi_tui::tui::Container;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::Component;
use pi_tui::utils::truncate_to_width;

use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::theme::theme::theme;

pub struct UserMessageItem {
    pub id: String,
    pub text: String,
    pub timestamp: Option<String>,
}

/// Custom user message list component with selection.
pub struct UserMessageList {
    messages: Vec<UserMessageItem>,
    selected_index: usize,
    pub on_select: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub on_cancel: Option<Arc<dyn Fn() + Send + Sync>>,
    max_visible: usize,
}

impl UserMessageList {
    pub fn new(messages: Vec<UserMessageItem>, initial_selected_id: Option<&str>) -> Self {
        let selected_index = initial_selected_id
            .and_then(|id| messages.iter().position(|message| message.id == id))
            .unwrap_or(messages.len().saturating_sub(1));
        Self {
            messages,
            selected_index,
            on_select: None,
            on_cancel: None,
            max_visible: 10,
        }
    }
}

impl Component for UserMessageList {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let t = theme();
        let t = t.as_ref();
        let accent = |text: &str| t.map(|t| t.fg("accent", text)).unwrap_or_else(|| text.to_string());
        let muted = |text: &str| t.map(|t| t.fg("muted", text)).unwrap_or_else(|| text.to_string());

        if self.messages.is_empty() {
            lines.push(muted("  No user messages found"));
            return lines;
        }

        let start_index = (self.selected_index as isize - (self.max_visible as isize / 2))
            .max(0)
            .min((self.messages.len() - self.max_visible) as isize)
            .max(0) as usize;
        let end_index = (start_index + self.max_visible).min(self.messages.len());

        for i in start_index..end_index {
            let message = &self.messages[i];
            let is_selected = i == self.selected_index;

            let normalized = message.text.replace('\n', " ").trim().to_string();
            let cursor = if is_selected { accent("› ") } else { "  ".to_string() };
            let max_msg_width = (width as f64) - 2.0;
            let truncated = truncate_to_width(&normalized, max_msg_width, "", false);
            let message_line = if is_selected {
                let bold = t.map(|t| t.bold(&truncated)).unwrap_or(truncated.clone());
                cursor + &bold
            } else {
                cursor + &truncated
            };
            lines.push(message_line);

            let position = i + 1;
            let metadata = muted(&format!("  Message {position} of {}", self.messages.len()));
            lines.push(metadata);
            lines.push(String::new());
        }

        if start_index > 0 || end_index < self.messages.len() {
            let scroll_info = muted(&format!("  ({}/{})", self.selected_index + 1, self.messages.len()));
            lines.push(scroll_info);
        }

        lines
    }

    fn handle_input(&mut self, key_data: &str) {
        let kb = get_keybindings();
        let manager = match &*kb {
            Some(manager) => manager,
            None => return,
        };
        if manager.matches(key_data, "tui.select.up") {
            self.selected_index = if self.selected_index == 0 {
                self.messages.len().saturating_sub(1)
            } else {
                self.selected_index - 1
            };
        } else if manager.matches(key_data, "tui.select.down") {
            self.selected_index = if self.selected_index + 1 >= self.messages.len() {
                0
            } else {
                self.selected_index + 1
            };
        } else if manager.matches(key_data, "tui.select.confirm") {
            if let Some(selected) = self.messages.get(self.selected_index) {
                if let Some(on_select) = &self.on_select {
                    on_select(&selected.id);
                }
            }
        } else if manager.matches(key_data, "tui.select.cancel") {
            if let Some(on_cancel) = &self.on_cancel {
                on_cancel();
            }
        }
    }
}

/// Component that renders a user message selector for branching.
pub struct UserMessageSelectorComponent {
    container: Container,
    message_list: Arc<UserMessageList>,
}

impl UserMessageSelectorComponent {
    pub fn new(
        messages: Vec<UserMessageItem>,
        on_select: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        initial_selected_id: Option<&str>,
    ) -> Self {
        let mut container = Container::new();
        let t = theme();
        let t = t.as_ref();
        let bold = |text: &str| t.map(|t| t.bold(text)).unwrap_or_else(|| text.to_string());
        let muted = |text: &str| t.map(|t| t.fg("muted", text)).unwrap_or_else(|| text.to_string());

        let mut list = UserMessageList::new(messages, initial_selected_id);
        list.on_select = Some(on_select);
        list.on_cancel = Some(on_cancel);
        let list_arc = Arc::new(list);

        container.add_child(Arc::new(pi_tui::components::basic::Spacer::new(1)));
        container.add_child(Arc::new(Text::new(&bold("Fork from Message"), 1, 0, None)));
        container.add_child(Arc::new(Text::new(
            &muted("Select a user message to copy the active path up to that point into a new session"),
            1,
            0,
            None,
        )));
        container.add_child(Arc::new(pi_tui::components::basic::Spacer::new(1)));
        container.add_child(Arc::new(DynamicBorder::new(None)));
        container.add_child(Arc::new(pi_tui::components::basic::Spacer::new(1)));
        container.add_child(list_arc.clone());
        container.add_child(Arc::new(pi_tui::components::basic::Spacer::new(1)));
        container.add_child(Arc::new(DynamicBorder::new(None)));

        Self { container, message_list: list_arc }
    }

    pub fn get_message_list(&self) -> &UserMessageList {
        &self.message_list
    }
}

impl Component for UserMessageSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        if let Some(list) = Arc::get_mut(&mut self.message_list) {
            list.handle_input(data);
        }
    }
}
