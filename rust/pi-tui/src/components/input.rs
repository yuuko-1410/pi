//! Input component, port of `packages/tui/src/components/input.ts`.
//!
//! Single-line text input with horizontal scrolling, Emacs-style kill/yank,
//! undo, bracketed paste, and word navigation.

use std::sync::Arc;

use crate::keybindings::{get_keybindings, KeybindingsManager};
use crate::keys::decode_kitty_printable;
use crate::kill_ring::KillRing;
use crate::tui::{Component, CURSOR_MARKER};
use crate::undo_stack::UndoStack;
use crate::utils::{graphemes, is_whitespace_char, slice_by_column, visible_width};
use crate::word_navigation::{find_word_backward, find_word_forward};

#[derive(Clone, Debug, PartialEq)]
struct InputState {
    value: String,
    cursor: usize,
}

/// Single-line text input with horizontal scrolling.
pub struct Input {
    value: String,
    cursor: usize,
    pub focused: bool,
    pub on_submit: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub on_escape: Option<Arc<dyn Fn() + Send + Sync>>,
    paste_buffer: String,
    is_in_paste: bool,
    kill_ring: KillRing,
    last_action: Option<String>,
    undo_stack: UndoStack<InputState>,
}

impl Input {
    pub fn new() -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            focused: false,
            on_submit: None,
            on_escape: None,
            paste_buffer: String::new(),
            is_in_paste: false,
            kill_ring: KillRing::new(),
            last_action: None,
            undo_stack: UndoStack::new(),
        }
    }

    pub fn get_value(&self) -> &str {
        &self.value
    }

    pub fn set_value(&mut self, value: &str) {
        self.value = value.to_string();
        self.cursor = self.cursor.min(value.chars().count());
    }

    fn insert_character(&mut self, char: &str) {
        if is_whitespace_char(char.chars().next().unwrap_or(' ')) || self.last_action.as_deref() != Some("type-word") {
            self.push_undo();
        }
        self.last_action = Some("type-word".to_string());
        let cursor = self.cursor;
        self.value = format!("{}{}{}", &self.value[..byte_at(&self.value, cursor)], char, &self.value[byte_at(&self.value, cursor)..]);
        self.cursor = cursor + char.chars().count();
    }

    fn handle_backspace(&mut self) {
        self.last_action = None;
        if self.cursor > 0 {
            self.push_undo();
            let before_cursor = self.value.chars().take(self.cursor).collect::<String>();
            let graphemes = graphemes(&before_cursor);
            let grapheme_length = graphemes.last().map(|g| g.chars().count()).unwrap_or(1);
            let start = self.value.chars().take(self.cursor - grapheme_length).collect::<String>();
            let end = self.value.chars().skip(self.cursor).collect::<String>();
            self.value = format!("{start}{end}");
            self.cursor -= grapheme_length;
        }
    }

    fn handle_forward_delete(&mut self) {
        self.last_action = None;
        if self.cursor < self.value.chars().count() {
            self.push_undo();
            let after_cursor = self.value.chars().skip(self.cursor).collect::<String>();
            let graphemes = graphemes(&after_cursor);
            let grapheme_length = graphemes.first().map(|g| g.chars().count()).unwrap_or(1);
            let start = self.value.chars().take(self.cursor).collect::<String>();
            let end = self.value.chars().skip(self.cursor + grapheme_length).collect::<String>();
            self.value = format!("{start}{end}");
        }
    }

    fn delete_to_line_start(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.push_undo();
        let deleted_text = self.value.chars().take(self.cursor).collect::<String>();
        let accumulate = self.last_action.as_deref() == Some("kill");
        self.kill_ring.push(&deleted_text, true, Some(accumulate));
        self.last_action = Some("kill".to_string());
        self.value = self.value.chars().skip(self.cursor).collect::<String>();
        self.cursor = 0;
    }

    fn delete_to_line_end(&mut self) {
        if self.cursor >= self.value.chars().count() {
            return;
        }
        self.push_undo();
        let deleted_text = self.value.chars().skip(self.cursor).collect::<String>();
        let accumulate = self.last_action.as_deref() == Some("kill");
        self.kill_ring.push(&deleted_text, false, Some(accumulate));
        self.last_action = Some("kill".to_string());
        self.value = self.value.chars().take(self.cursor).collect::<String>();
    }

    fn delete_word_backwards(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let was_kill = self.last_action.as_deref() == Some("kill");
        self.push_undo();
        let old_cursor = self.cursor;
        self.move_word_backwards();
        let delete_from = self.cursor;
        self.cursor = old_cursor;
        let deleted_text = self.value.chars().skip(delete_from).take(self.cursor - delete_from).collect::<String>();
        self.kill_ring.push(&deleted_text, true, Some(was_kill));
        self.last_action = Some("kill".to_string());
        let start = self.value.chars().take(delete_from).collect::<String>();
        let end = self.value.chars().skip(self.cursor).collect::<String>();
        self.value = format!("{start}{end}");
        self.cursor = delete_from;
    }

    fn delete_word_forward(&mut self) {
        if self.cursor >= self.value.chars().count() {
            return;
        }
        let was_kill = self.last_action.as_deref() == Some("kill");
        self.push_undo();
        let old_cursor = self.cursor;
        self.move_word_forwards();
        let delete_to = self.cursor;
        self.cursor = old_cursor;
        let deleted_text = self.value.chars().skip(self.cursor).take(delete_to - self.cursor).collect::<String>();
        self.kill_ring.push(&deleted_text, false, Some(was_kill));
        self.last_action = Some("kill".to_string());
        let start = self.value.chars().take(self.cursor).collect::<String>();
        let end = self.value.chars().skip(delete_to).collect::<String>();
        self.value = format!("{start}{end}");
    }

    fn yank(&mut self) {
        let text = match self.kill_ring.peek() {
            Some(text) => text.to_string(),
            None => return,
        };
        self.push_undo();
        let cursor = self.cursor;
        let before = self.value.chars().take(cursor).collect::<String>();
        let after = self.value.chars().skip(cursor).collect::<String>();
        self.value = format!("{before}{text}{after}");
        self.cursor = cursor + text.chars().count();
        self.last_action = Some("yank".to_string());
    }

    fn yank_pop(&mut self) {
        if self.last_action.as_deref() != Some("yank") || self.kill_ring.len() <= 1 {
            return;
        }
        self.push_undo();
        let prev_text = self.kill_ring.peek().unwrap_or("").to_string();
        let prev_len = prev_text.chars().count();
        let start = self.value.chars().take(self.cursor - prev_len).collect::<String>();
        let end = self.value.chars().skip(self.cursor).collect::<String>();
        self.value = format!("{start}{end}");
        self.cursor -= prev_len;
        self.kill_ring.rotate();
        let text = self.kill_ring.peek().unwrap_or("").to_string();
        let before = self.value.chars().take(self.cursor).collect::<String>();
        let after = self.value.chars().skip(self.cursor).collect::<String>();
        self.value = format!("{before}{text}{after}");
        self.cursor += text.chars().count();
        self.last_action = Some("yank".to_string());
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(&InputState {
            value: self.value.clone(),
            cursor: self.cursor,
        });
    }

    fn undo(&mut self) {
        if let Some(snapshot) = self.undo_stack.pop() {
            self.value = snapshot.value;
            self.cursor = snapshot.cursor;
            self.last_action = None;
        }
    }

    fn move_word_backwards(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.last_action = None;
        let chars: Vec<char> = self.value.chars().collect();
        let bytes = self.value.clone();
        self.cursor = find_word_backward(&bytes, byte_at(&bytes, self.cursor), None);
        let _ = chars;
    }

    fn move_word_forwards(&mut self) {
        if self.cursor >= self.value.chars().count() {
            return;
        }
        self.last_action = None;
        let bytes = self.value.clone();
        self.cursor = find_word_forward(&bytes, byte_at(&bytes, self.cursor), None);
    }

    fn handle_paste(&mut self, pasted_text: &str) {
        self.last_action = None;
        self.push_undo();
        let clean_text = pasted_text
            .replace("\r\n", "")
            .replace('\r', "")
            .replace('\n', "")
            .replace('\t', "    ");
        let cursor = self.cursor;
        let before = self.value.chars().take(cursor).collect::<String>();
        let after = self.value.chars().skip(cursor).collect::<String>();
        self.value = format!("{before}{clean_text}{after}");
        self.cursor = cursor + clean_text.chars().count();
    }

    fn handle_input_with(&mut self, keybindings: &KeybindingsManager, data: &str) {
        if data.contains("\x1b[200~") {
            self.is_in_paste = true;
            self.paste_buffer.clear();
            let data = data.replace("\x1b[200~", "");
            if !data.is_empty() {
                self.process_paste_chunk(&data);
            }
            return;
        }
        if self.is_in_paste {
            self.process_paste_chunk(data);
            return;
        }

        if keybindings.matches(data, "tui.select.cancel") {
            if let Some(on_escape) = &self.on_escape {
                on_escape();
            }
            return;
        }
        if keybindings.matches(data, "tui.editor.undo") {
            self.undo();
            return;
        }
        if keybindings.matches(data, "tui.input.submit") || data == "\n" {
            if let Some(on_submit) = &self.on_submit {
                on_submit(&self.value);
            }
            return;
        }
        if keybindings.matches(data, "tui.editor.deleteCharBackward") {
            self.handle_backspace();
            return;
        }
        if keybindings.matches(data, "tui.editor.deleteCharForward") {
            self.handle_forward_delete();
            return;
        }
        if keybindings.matches(data, "tui.editor.deleteWordBackward") {
            self.delete_word_backwards();
            return;
        }
        if keybindings.matches(data, "tui.editor.deleteWordForward") {
            self.delete_word_forward();
            return;
        }
        if keybindings.matches(data, "tui.editor.deleteToLineStart") {
            self.delete_to_line_start();
            return;
        }
        if keybindings.matches(data, "tui.editor.deleteToLineEnd") {
            self.delete_to_line_end();
            return;
        }
        if keybindings.matches(data, "tui.editor.yank") {
            self.yank();
            return;
        }
        if keybindings.matches(data, "tui.editor.yankPop") {
            self.yank_pop();
            return;
        }
        if keybindings.matches(data, "tui.editor.cursorLeft") {
            self.last_action = None;
            if self.cursor > 0 {
                let before_cursor = self.value.chars().take(self.cursor).collect::<String>();
                let graphemes = graphemes(&before_cursor);
                self.cursor -= graphemes.last().map(|g| g.chars().count()).unwrap_or(1);
            }
            return;
        }
        if keybindings.matches(data, "tui.editor.cursorRight") {
            self.last_action = None;
            if self.cursor < self.value.chars().count() {
                let after_cursor = self.value.chars().skip(self.cursor).collect::<String>();
                let graphemes = graphemes(&after_cursor);
                self.cursor += graphemes.first().map(|g| g.chars().count()).unwrap_or(1);
            }
            return;
        }
        if keybindings.matches(data, "tui.editor.cursorLineStart") {
            self.last_action = None;
            self.cursor = 0;
            return;
        }
        if keybindings.matches(data, "tui.editor.cursorLineEnd") {
            self.last_action = None;
            self.cursor = self.value.chars().count();
            return;
        }
        if keybindings.matches(data, "tui.editor.cursorWordLeft") {
            self.move_word_backwards();
            return;
        }
        if keybindings.matches(data, "tui.editor.cursorWordRight") {
            self.move_word_forwards();
            return;
        }

        if let Some(kitty_printable) = decode_kitty_printable(data) {
            self.insert_character(&kitty_printable);
            return;
        }

        let has_control_chars = data.chars().any(|ch| {
            let code = ch as u32;
            code < 32 || code == 0x7f || (0x80..=0x9f).contains(&code)
        });
        if !has_control_chars {
            self.insert_character(data);
        }
    }

    fn process_paste_chunk(&mut self, data: &str) {
        self.paste_buffer.push_str(data);
        if let Some(end_index) = self.paste_buffer.find("\x1b[201~") {
            let paste_content = self.paste_buffer[..end_index].to_string();
            self.handle_paste(&paste_content);
            self.is_in_paste = false;
            let remaining = self.paste_buffer[end_index + 6..].to_string();
            self.paste_buffer.clear();
            if !remaining.is_empty() {
                let keybindings = get_keybindings();
                if let Some(manager) = &*keybindings {
                    self.handle_input_with(manager, &remaining);
                }
            }
        }
    }
}

/// Byte offset for a char index in a string.
fn byte_at(text: &str, char_index: usize) -> usize {
    text.char_indices().nth(char_index).map(|(index, _)| index).unwrap_or(text.len())
}

impl Component for Input {
    fn render(&self, width: usize) -> Vec<String> {
        let prompt = "> ";
        let available_width = width.saturating_sub(prompt.chars().count()) as f64;
        if available_width <= 0.0 {
            return vec![prompt.to_string()];
        }

        let mut cursor_display = self.cursor;
        let total_width = visible_width(&self.value);

        let visible_text = if total_width < available_width {
            self.value.clone()
        } else {
            let scroll_width = if self.cursor == self.value.chars().count() {
                available_width - 1.0
            } else {
                available_width
            };
            let cursor_col = visible_width(&self.value.chars().take(self.cursor).collect::<String>());

            if scroll_width > 0.0 {
                let half_width = (scroll_width / 2.0).floor();
                let start_col = if cursor_col < half_width {
                    0.0
                } else if cursor_col > total_width - half_width {
                    (total_width - scroll_width).max(0.0)
                } else {
                    (cursor_col - half_width).max(0.0)
                };
                let visible = slice_by_column(&self.value, start_col, scroll_width, true);
                let before_cursor = slice_by_column(&self.value, start_col, (cursor_col - start_col).max(0.0), true);
                cursor_display = before_cursor.chars().count();
                visible
            } else {
                cursor_display = 0;
                String::new()
            }
        };

        let visible_graphemes = graphemes(&visible_text);
        let before_cursor: String = visible_graphemes
            .iter()
            .take(cursor_display.min(visible_graphemes.len()))
            .cloned()
            .collect();
        let at_cursor = visible_graphemes.get(cursor_display.min(visible_graphemes.len())).cloned().unwrap_or_else(|| " ".to_string());
        let after_cursor: String = visible_graphemes
            .iter()
            .skip(cursor_display.min(visible_graphemes.len()) + 1)
            .cloned()
            .collect();

        let marker = if self.focused { CURSOR_MARKER.to_string() } else { String::new() };
        let cursor_char = format!("\x1b[7m{at_cursor}\x1b[27m");
        let text_with_cursor = format!("{before_cursor}{marker}{cursor_char}{after_cursor}");

        let visual_length = visible_width(&text_with_cursor);
        let padding = " ".repeat(((available_width - visual_length).max(0.0)) as usize);
        vec![format!("{prompt}{text_with_cursor}{padding}")]
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
    use crate::keybindings::KeybindingsManager;

    fn keybindings() -> KeybindingsManager {
        KeybindingsManager::new(crate::keybindings::tui_keybindings())
    }

    #[test]
    fn inserts_characters() {
        let mut input = Input::new();
        let keybindings = keybindings();
        input.handle_input_with(&keybindings, "h");
        input.handle_input_with(&keybindings, "i");
        assert_eq!(input.get_value(), "hi");
        assert_eq!(input.cursor, 2);
    }

    #[test]
    fn backspace_and_forward_delete() {
        let mut input = Input::new();
        let keybindings = keybindings();
        for ch in ["a", "b", "c"] {
            input.handle_input_with(&keybindings, ch);
        }
        input.handle_input_with(&keybindings, "\x7f"); // backspace
        assert_eq!(input.get_value(), "ab");
        assert_eq!(input.cursor, 2);
        input.handle_input_with(&keybindings, "\x1b[3~"); // delete forward
        assert_eq!(input.get_value(), "ab");
        input.cursor = 0;
        input.handle_input_with(&keybindings, "\x1b[3~");
        assert_eq!(input.get_value(), "b");
    }

    #[test]
    fn cursor_movement() {
        let mut input = Input::new();
        let keybindings = keybindings();
        for ch in ["a", "b", "c"] {
            input.handle_input_with(&keybindings, ch);
        }
        input.handle_input_with(&keybindings, "\x1b[D"); // left
        assert_eq!(input.cursor, 2);
        input.handle_input_with(&keybindings, "\x1b[C"); // right
        assert_eq!(input.cursor, 3);
        input.handle_input_with(&keybindings, "\x1bOH"); // home
        assert_eq!(input.cursor, 0);
        input.handle_input_with(&keybindings, "\x1bOF"); // end
        assert_eq!(input.cursor, 3);
    }

    #[test]
    fn line_start_end_deletion_uses_kill_ring() {
        let mut input = Input::new();
        let keybindings = keybindings();
        for ch in ["a", "b", "c"] {
            input.handle_input_with(&keybindings, ch);
        }
        input.handle_input_with(&keybindings, "\x15"); // ctrl+u delete to line start
        // JS keeps the suffix after the cursor; cursor was at the end, so the
        // value becomes empty and "abc" moves to the kill ring.
        assert_eq!(input.get_value(), "");
        assert_eq!(input.cursor, 0);
        assert_eq!(input.kill_ring.peek(), Some("abc"));
        input.handle_input_with(&keybindings, "\x19"); // ctrl+y yank
        assert_eq!(input.get_value(), "abc");
        assert_eq!(input.cursor, 3);
    }

    #[test]
    fn undo_restores_state() {
        let mut input = Input::new();
        let keybindings = keybindings();
        input.handle_input_with(&keybindings, "h");
        input.handle_input_with(&keybindings, "i");
        assert_eq!(input.get_value(), "hi");
        input.handle_input_with(&keybindings, "\x1b[27;5;45~"); // ctrl+- undo
        // Coalesced typing: one undo unit restores the pre-typing state.
        assert_eq!(input.get_value(), "");
    }

    #[test]
    fn submit_and_escape() {
        let mut input = Input::new();
        let submitted = Arc::new(std::sync::Mutex::new(None::<String>));
        let submitted_clone = submitted.clone();
        input.on_submit = Some(Arc::new(move |value| {
            *submitted_clone.lock().unwrap() = Some(value.to_string());
        }));
        let escaped = Arc::new(std::sync::Mutex::new(false));
        let escaped_clone = escaped.clone();
        input.on_escape = Some(Arc::new(move || {
            *escaped_clone.lock().unwrap() = true;
        }));
        let keybindings = keybindings();
        input.handle_input_with(&keybindings, "x");
        input.handle_input_with(&keybindings, "\r");
        assert_eq!(submitted.lock().unwrap().as_deref(), Some("x"));
        input.handle_input_with(&keybindings, "\x1b");
        assert!(*escaped.lock().unwrap());
    }

    #[test]
    fn bracketed_paste_inserts_cleaned_text() {
        let mut input = Input::new();
        let keybindings = keybindings();
        input.handle_input_with(&keybindings, "\x1b[200~line1\nline2\r\n\tx\x1b[201~");
        assert_eq!(input.get_value(), "line1line2    x");
    }

    #[test]
    fn renders_with_cursor_marker_when_focused() {
        let mut input = Input::new();
        input.set_value("hello");
        input.focused = true;
        let lines = input.render(20);
        assert!(lines[0].contains(CURSOR_MARKER));
        assert!(lines[0].starts_with("> "));
    }

    #[test]
    fn word_movement() {
        let mut input = Input::new();
        let keybindings = keybindings();
        for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
            input.handle_input_with(&keybindings, ch);
        }
        input.handle_input_with(&keybindings, "\x1bOd"); // ctrl+left
        assert_eq!(input.cursor, 6);
        input.handle_input_with(&keybindings, "\x1bOc"); // ctrl+right
        assert_eq!(input.cursor, 11);
    }
}
