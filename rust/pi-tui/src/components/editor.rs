//! Multi-line editor component, port of
//! `packages/tui/src/components/editor.ts`.
//!
//! Core port: line buffer editing with visual-line cursor navigation,
//! word wrapping (wordWrapLine), history browsing, kill/yank with the kill
//! ring, undo with paste-registry snapshots, bracketed paste, character
//! jump mode, scrolling render with borders and hardware cursor marker,
//! and autocomplete integration over the CombinedAutocompleteProvider.
//!
//! Documented differences: the JS editor's async autocomplete debounce and
//! abort controller are synchronous here; terminal image rendering inside
//! the editor and the OSC-52 clipboard integration are not ported.

use std::sync::Arc;

use crate::autocomplete::{AutocompleteItem, CombinedAutocompleteProvider};
use crate::keybindings::{get_keybindings, KeybindingsManager};
use crate::keys::decode_printable_key;
use crate::kill_ring::KillRing;
use crate::tui::{Component, CURSOR_MARKER};
use crate::undo_stack::UndoStack;
use crate::utils::{graphemes, is_cjk_char, is_whitespace_char, visible_width};
use crate::word_navigation::{find_word_backward, find_word_forward};

const MAX_HISTORY: usize = 100;

#[derive(Clone, Debug, PartialEq)]
pub struct TextChunk {
    pub text: String,
    pub start_index: usize,
    pub end_index: usize,
}

/// Split a line into word-wrapped chunks, wrapping at word boundaries with
/// character-level fallback for long words.
pub fn word_wrap_line(line: &str, max_width: f64) -> Vec<TextChunk> {
    if line.is_empty() || max_width <= 0.0 {
        return vec![TextChunk {
            text: String::new(),
            start_index: 0,
            end_index: 0,
        }];
    }
    let line_width = visible_width(line);
    if line_width <= max_width {
        return vec![TextChunk {
            text: line.to_string(),
            start_index: 0,
            end_index: line.chars().count(),
        }];
    }

    let segments = graphemes(line);
    let mut chunks: Vec<TextChunk> = Vec::new();
    let mut current_width = 0.0f64;
    let mut chunk_start = 0usize;
    let mut wrap_opp_index = -1isize;
    let mut wrap_opp_width = 0.0f64;

    for (i, grapheme) in segments.iter().enumerate() {
        let g_width = visible_width(grapheme);
        let char_index = segments[..i].iter().map(|s| s.chars().count()).sum::<usize>();
        let is_ws = is_whitespace_char(grapheme.chars().next().unwrap_or(' '));

        // Overflow check before advancing.
        if current_width + g_width > max_width {
            if wrap_opp_index >= 0 && current_width - wrap_opp_width + g_width <= max_width {
                let end = wrap_opp_index as usize;
                chunks.push(TextChunk {
                    text: line[char_offset(line, chunk_start)..char_offset(line, end)].to_string(),
                    start_index: chunk_start,
                    end_index: end,
                });
                chunk_start = end;
                current_width -= wrap_opp_width;
            } else if chunk_start < char_index {
                chunks.push(TextChunk {
                    text: line[char_offset(line, chunk_start)..char_offset(line, char_index)].to_string(),
                    start_index: chunk_start,
                    end_index: char_index,
                });
                chunk_start = char_index;
                current_width = 0.0;
            }
            wrap_opp_index = -1;
        }

        if g_width > max_width {
            // Atomic segment wider than the limit: sub-wrap it.
            let sub_chunks = word_wrap_line(grapheme, max_width);
            for sub in &sub_chunks[..sub_chunks.len().saturating_sub(1)] {
                chunks.push(TextChunk {
                    text: sub.text.clone(),
                    start_index: char_index + sub.start_index,
                    end_index: char_index + sub.end_index,
                });
            }
            let last = sub_chunks.last().cloned().unwrap();
            chunk_start = char_index + last.start_index;
            current_width = visible_width(&last.text);
            wrap_opp_index = -1;
            continue;
        }

        current_width += g_width;

        let next = segments.get(i + 1);
        if is_ws {
            if let Some(next) = next {
                if !is_whitespace_char(next.chars().next().unwrap_or(' ')) {
                    wrap_opp_index = (segments[..=i].iter().map(|s| s.chars().count()).sum::<usize>()) as isize;
                    wrap_opp_width = current_width;
                }
            }
        } else if let Some(next) = next {
            if !is_whitespace_char(next.chars().next().unwrap_or(' ')) {
                let is_cjk = is_cjk_char(grapheme.chars().next().unwrap_or(' '));
                let next_is_cjk = is_cjk_char(next.chars().next().unwrap_or(' '));
                if is_cjk || next_is_cjk {
                    wrap_opp_index = (segments[..=i].iter().map(|s| s.chars().count()).sum::<usize>()) as isize;
                    wrap_opp_width = current_width;
                }
            }
        }
    }

    chunks.push(TextChunk {
        text: line[char_offset(line, chunk_start)..].to_string(),
        start_index: chunk_start,
        end_index: line.chars().count(),
    });
    chunks
}

fn char_offset(text: &str, char_index: usize) -> usize {
    text.char_indices().nth(char_index).map(|(index, _)| index).unwrap_or(text.len())
}

#[derive(Clone, Debug, PartialEq)]
pub struct EditorState {
    pub lines: Vec<String>,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

#[derive(Clone, Debug, PartialEq)]
struct EditorSnapshot {
    state: EditorState,
    pastes: Vec<(u64, String)>,
    paste_counter: u64,
}

pub struct EditorTheme {
    pub border_color: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

pub struct EditorOptions {
    pub padding_x: Option<f64>,
    pub autocomplete_max_visible: Option<f64>,
}

impl Default for EditorOptions {
    fn default() -> Self {
        Self {
            padding_x: None,
            autocomplete_max_visible: None,
        }
    }
}

struct LayoutLine {
    text: String,
    has_cursor: bool,
    cursor_pos: Option<usize>,
}

/// Multi-line text editor.
pub struct Editor {
    state: EditorState,
    pub focused: bool,
    pub border_color: Arc<dyn Fn(&str) -> String + Send + Sync>,
    padding_x: usize,
    autocomplete_max_visible: usize,
    scroll_offset: usize,
    last_width: f64,
    history: Vec<String>,
    history_index: isize,
    history_draft: Option<EditorState>,
    kill_ring: KillRing,
    last_action: Option<String>,
    jump_mode: Option<String>,
    preferred_visual_col: Option<f64>,
    pastes: Vec<(u64, String)>,
    paste_counter: u64,
    paste_buffer: String,
    is_in_paste: bool,
    undo_stack: UndoStack<EditorSnapshot>,
    autocomplete_provider: Option<Arc<CombinedAutocompleteProvider>>,
    autocomplete_prefix: String,
    autocomplete_state: Option<String>,
    autocomplete_list: Option<Vec<AutocompleteItem>>,
    autocomplete_selected: usize,
    pub on_submit: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub on_change: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub disable_submit: bool,
    request_render: Arc<dyn Fn() + Send + Sync>,
}

impl Editor {
    pub fn new(
        theme: EditorTheme,
        options: EditorOptions,
        request_render: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let padding_x = options.padding_x.unwrap_or(0.0);
        let autocomplete_max_visible = options.autocomplete_max_visible.unwrap_or(5.0);
        Self {
            state: EditorState {
                lines: vec![String::new()],
                cursor_line: 0,
                cursor_col: 0,
            },
            focused: false,
            border_color: theme.border_color,
            padding_x: if padding_x.is_finite() {
                padding_x.max(0.0).floor() as usize
            } else {
                0
            },
            autocomplete_max_visible: if autocomplete_max_visible.is_finite() {
                autocomplete_max_visible.max(3.0).min(20.0).floor() as usize
            } else {
                5
            },
            scroll_offset: 0,
            last_width: 80.0,
            history: Vec::new(),
            history_index: -1,
            history_draft: None,
            kill_ring: KillRing::new(),
            last_action: None,
            jump_mode: None,
            preferred_visual_col: None,
            pastes: Vec::new(),
            paste_counter: 0,
            paste_buffer: String::new(),
            is_in_paste: false,
            undo_stack: UndoStack::new(),
            autocomplete_provider: None,
            autocomplete_prefix: String::new(),
            autocomplete_state: None,
            autocomplete_list: None,
            autocomplete_selected: 0,
            on_submit: None,
            on_change: None,
            disable_submit: false,
            request_render,
        }
    }

    pub fn get_text(&self) -> String {
        self.state.lines.join("\n")
    }

    pub fn set_text(&mut self, text: &str) {
        let lines: Vec<String> = text.split('\n').map(|line| line.to_string()).collect();
        self.state.lines = if lines.is_empty() { vec![String::new()] } else { lines };
        self.state.cursor_line = self.state.lines.len().saturating_sub(1);
        self.state.cursor_col = self.state.lines[self.state.cursor_line].chars().count();
    }

    pub fn add_to_history(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.first().map(|entry| entry.as_str()) == Some(trimmed) {
            return;
        }
        self.history.insert(0, trimmed.to_string());
        if self.history.len() > MAX_HISTORY {
            self.history.pop();
        }
    }

    pub fn set_autocomplete_provider(&mut self, provider: Arc<CombinedAutocompleteProvider>) {
        self.autocomplete_provider = Some(provider);
    }

    fn set_cursor_col(&mut self, col: usize) {
        self.state.cursor_col = col;
    }

    fn current_line(&self) -> &str {
        &self.state.lines[self.state.cursor_line]
    }

    fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(&EditorSnapshot {
            state: self.state.clone(),
            pastes: self.pastes.clone(),
            paste_counter: self.paste_counter,
        });
    }

    fn undo(&mut self) {
        if let Some(snapshot) = self.undo_stack.pop() {
            self.state = snapshot.state;
            self.pastes = snapshot.pastes;
            self.paste_counter = snapshot.paste_counter;
            self.last_action = None;
            self.cancel_autocomplete();
            (self.request_render)();
        }
    }

    fn insert_character(&mut self, char: &str, skip_undo_coalescing: bool) {
        if !skip_undo_coalescing
            && (is_whitespace_char(char.chars().next().unwrap_or(' ')) || self.last_action.as_deref() != Some("type-word"))
        {
            self.push_undo_snapshot();
        }
        self.last_action = Some("type-word".to_string());
        let cursor_col = self.state.cursor_col;
        let line = &mut self.state.lines[self.state.cursor_line];
        let offset = char_offset(line, cursor_col);
        line.insert_str(offset, char);
        self.state.cursor_col = cursor_col + char.chars().count();
    }

    fn handle_backspace(&mut self) {
        self.last_action = None;
        if self.state.cursor_col > 0 {
            self.push_undo_snapshot();
            let line = &mut self.state.lines[self.state.cursor_line];
            let before: String = line.chars().take(self.state.cursor_col).collect();
            let graphemes = graphemes(&before);
            let grapheme_length = graphemes.last().map(|g| g.chars().count()).unwrap_or(1);
            let start: String = line.chars().take(self.state.cursor_col - grapheme_length).collect();
            let end: String = line.chars().skip(self.state.cursor_col).collect();
            *line = format!("{start}{end}");
            self.state.cursor_col -= grapheme_length;
        } else if self.state.cursor_line > 0 {
            // Join with the previous line.
            self.push_undo_snapshot();
            let previous_len = self.state.lines[self.state.cursor_line - 1].chars().count();
            let current = self.state.lines.remove(self.state.cursor_line);
            self.state.lines[self.state.cursor_line - 1].push_str(&current);
            self.state.cursor_line -= 1;
            self.state.cursor_col = previous_len;
        }
    }

    fn add_new_line(&mut self) {
        self.push_undo_snapshot();
        let line = &mut self.state.lines[self.state.cursor_line];
        let offset = char_offset(line, self.state.cursor_col);
        let rest = line[offset..].to_string();
        line.truncate(offset);
        self.state.cursor_line += 1;
        self.state.lines.insert(self.state.cursor_line, rest);
        self.state.cursor_col = 0;
        self.last_action = None;
    }

    fn submit_value(&mut self) {
        if let Some(on_submit) = &self.on_submit {
            on_submit(&self.get_text());
        }
    }

    fn move_to_visual_line(&mut self, direction: isize) {
        // Vertical movement on visual lines: compute the target row in the
        // wrapped layout, then map back to a logical line/col.
        let layout = self.layout_text(self.last_width);
        let cursor_index = layout
            .iter()
            .position(|line| line.has_cursor)
            .unwrap_or(0) as isize;
        let target_index = cursor_index + direction;
        if target_index < 0 || target_index >= layout.len() as isize {
            return;
        }
        let target = &layout[target_index as usize];
        let preferred = self.preferred_visual_col.unwrap_or(self.state.cursor_col as f64);
        // Map the visual column to a logical column on the target line.
        let mut col = 0usize;
        let mut line_col = 0usize;
        let chars: Vec<char> = target.text.chars().collect();
        for char in &chars {
            let char_width = if is_wide_char(*char) { 2.0 } else { 1.0 };
            if col as f64 >= preferred {
                break;
            }
            col += char_width as usize;
            line_col += 1;
        }
        self.state.cursor_col = line_col;
        self.preferred_visual_col = Some(preferred);
    }

    fn move_to_line_start(&mut self) {
        self.state.cursor_col = 0;
        self.preferred_visual_col = None;
    }

    fn move_to_line_end(&mut self) {
        self.state.cursor_col = self.current_line().chars().count();
        self.preferred_visual_col = None;
    }

    fn delete_to_start_of_line(&mut self) {
        if self.state.cursor_col == 0 {
            return;
        }
        self.push_undo_snapshot();
        let deleted: String = self.state.lines[self.state.cursor_line].chars().take(self.state.cursor_col).collect();
        self.kill_ring.push(&deleted, true, Some(self.last_action.as_deref() == Some("kill")));
        self.last_action = Some("kill".to_string());
        let rest: String = self.state.lines[self.state.cursor_line].chars().skip(self.state.cursor_col).collect();
        self.state.lines[self.state.cursor_line] = rest;
        self.state.cursor_col = 0;
    }

    fn delete_to_end_of_line(&mut self) {
        let line_len = self.current_line().chars().count();
        if self.state.cursor_col >= line_len {
            return;
        }
        self.push_undo_snapshot();
        let deleted: String = self.state.lines[self.state.cursor_line].chars().skip(self.state.cursor_col).collect();
        self.kill_ring.push(&deleted, false, Some(self.last_action.as_deref() == Some("kill")));
        self.last_action = Some("kill".to_string());
        let truncate_at = char_offset(&self.state.lines[self.state.cursor_line], self.state.cursor_col);
        self.state.lines[self.state.cursor_line].truncate(truncate_at);
    }

    fn delete_word_backwards(&mut self) {
        if self.state.cursor_col == 0 && self.state.cursor_line == 0 {
            return;
        }
        let was_kill = self.last_action.as_deref() == Some("kill");
        self.push_undo_snapshot();
        let old_cursor = self.state.cursor_col;
        self.move_word_backwards();
        let delete_from = self.state.cursor_col;
        self.state.cursor_col = old_cursor;
        let deleted: String = self.state.lines[self.state.cursor_line].chars().skip(delete_from).take(old_cursor - delete_from).collect();
        self.kill_ring.push(&deleted, true, Some(was_kill));
        self.last_action = Some("kill".to_string());
        let start: String = self.state.lines[self.state.cursor_line].chars().take(delete_from).collect();
        let end: String = self.state.lines[self.state.cursor_line].chars().skip(old_cursor).collect();
        self.state.lines[self.state.cursor_line] = format!("{start}{end}");
        self.state.cursor_col = delete_from;
    }

    fn delete_word_forward(&mut self) {
        let line_len = self.current_line().chars().count();
        if self.state.cursor_col >= line_len {
            return;
        }
        let was_kill = self.last_action.as_deref() == Some("kill");
        self.push_undo_snapshot();
        let old_cursor = self.state.cursor_col;
        self.move_word_forwards();
        let delete_to = self.state.cursor_col;
        self.state.cursor_col = old_cursor;
        let deleted: String = self.state.lines[self.state.cursor_line].chars().skip(old_cursor).take(delete_to - old_cursor).collect();
        self.kill_ring.push(&deleted, false, Some(was_kill));
        self.last_action = Some("kill".to_string());
        let start: String = self.state.lines[self.state.cursor_line].chars().take(old_cursor).collect();
        let end: String = self.state.lines[self.state.cursor_line].chars().skip(delete_to).collect();
        self.state.lines[self.state.cursor_line] = format!("{start}{end}");
    }

    fn yank(&mut self) {
        let text = match self.kill_ring.peek() {
            Some(text) => text.to_string(),
            None => return,
        };
        self.push_undo_snapshot();
        let cursor_col = self.state.cursor_col;
        let line = &mut self.state.lines[self.state.cursor_line];
        let offset = char_offset(line, cursor_col);
        line.insert_str(offset, &text);
        self.state.cursor_col = cursor_col + text.chars().count();
        self.last_action = Some("yank".to_string());
    }

    fn yank_pop(&mut self) {
        if self.last_action.as_deref() != Some("yank") || self.kill_ring.len() <= 1 {
            return;
        }
        self.push_undo_snapshot();
        let prev_text = self.kill_ring.peek().unwrap_or("").to_string();
        let prev_len = prev_text.chars().count();
        let line = &mut self.state.lines[self.state.cursor_line];
        let start: String = line.chars().take(self.state.cursor_col - prev_len).collect();
        let end: String = line.chars().skip(self.state.cursor_col).collect();
        *line = format!("{start}{end}");
        self.state.cursor_col -= prev_len;
        self.kill_ring.rotate();
        let text = self.kill_ring.peek().unwrap_or("").to_string();
        let offset = char_offset(&line.clone(), self.state.cursor_col);
        line.insert_str(offset, &text);
        self.state.cursor_col += text.chars().count();
        self.last_action = Some("yank".to_string());
    }

    fn move_word_backwards(&mut self) {
        self.last_action = None;
        if self.state.cursor_col == 0 {
            return;
        }
        let line = self.current_line();
        self.state.cursor_col = find_word_backward(line, char_offset(line, self.state.cursor_col), None);
    }

    fn move_word_forwards(&mut self) {
        self.last_action = None;
        let line = self.current_line();
        let line_len = line.chars().count();
        if self.state.cursor_col >= line_len {
            return;
        }
        self.state.cursor_col = find_word_forward(line, char_offset(line, self.state.cursor_col), None);
    }

    fn jump_to_char(&mut self, char: char, direction: &str) {
        let line = self.current_line();
        let chars: Vec<char> = line.chars().collect();
        if direction == "forward" {
            if let Some(index) = chars[self.state.cursor_col + 1..].iter().position(|c| *c == char) {
                self.state.cursor_col = self.state.cursor_col + 1 + index;
            }
        } else {
            let before = &chars[..self.state.cursor_col];
            if let Some(index) = before.iter().rposition(|c| *c == char) {
                self.state.cursor_col = index;
            }
        }
    }

    fn navigate_history(&mut self, direction: isize) {
        if self.history.is_empty() {
            return;
        }
        if self.history_index == -1 {
            self.history_draft = Some(self.state.clone());
            self.history_index = 0;
        } else {
            let new_index = self.history_index + direction;
            if new_index < 0 || new_index >= self.history.len() as isize {
                return;
            }
            self.history_index = new_index;
        }
        let text = &self.history[self.history_index as usize];
        self.state.lines = text.split('\n').map(|line| line.to_string()).collect();
        self.state.cursor_line = self.state.lines.len().saturating_sub(1);
        self.state.cursor_col = self.state.lines[self.state.cursor_line].chars().count();
    }

    fn exit_history_browsing(&mut self) {
        if self.history_index == -1 {
            return;
        }
        self.history_index = -1;
        if let Some(draft) = &self.history_draft {
            self.state = draft.clone();
        }
        self.history_draft = None;
    }

    fn handle_paste(&mut self, pasted_text: &str) {
        self.last_action = None;
        self.push_undo_snapshot();
        let clean_text = pasted_text.replace('\r', "");
        let lines: Vec<String> = clean_text.split('\n').map(|line| line.to_string()).collect();
        if lines.len() <= 1 {
            let cursor_col = self.state.cursor_col;
            let line = &mut self.state.lines[self.state.cursor_line];
            let offset = char_offset(line, cursor_col);
            line.insert_str(offset, &clean_text);
            self.state.cursor_col = cursor_col + clean_text.chars().count();
        } else {
            let current = &mut self.state.lines[self.state.cursor_line];
            let offset = char_offset(current, self.state.cursor_col);
            let head = current[..offset].to_string();
            let tail = current[offset..].to_string();
            current.clear();
            current.push_str(&head);
            current.push_str(&lines[0]);
            for line in &lines[1..] {
                self.state.lines.insert(self.state.cursor_line + 1, line.clone());
                self.state.cursor_line += 1;
            }
            self.state.lines[self.state.cursor_line].push_str(&tail);
            self.state.cursor_col = self.state.lines[self.state.cursor_line].chars().count();
        }
        if let Some(on_change) = &self.on_change {
            on_change(&self.get_text());
        }
    }

    fn cancel_autocomplete(&mut self) {
        self.autocomplete_state = None;
        self.autocomplete_list = None;
        self.autocomplete_prefix.clear();
    }

    fn handle_tab_completion(&mut self) {
        let Some(provider) = &self.autocomplete_provider else {
            return;
        };
        let lines = self.state.lines.clone();
        let cursor_line = self.state.cursor_line;
        let cursor_col = self.state.cursor_col;
        let suggestions = provider.get_suggestions(&lines, cursor_line, cursor_col, true);
        if let Some(suggestions) = suggestions {
            self.autocomplete_prefix = suggestions.prefix.clone();
            self.autocomplete_list = Some(suggestions.items);
            self.autocomplete_selected = 0;
            self.autocomplete_state = Some("force".to_string());
            (self.request_render)();
        }
    }

    fn apply_autocomplete(&mut self, item: &AutocompleteItem) {
        let Some(provider) = &self.autocomplete_provider else {
            return;
        };
        let result = provider.apply_completion(
            &self.state.lines,
            self.state.cursor_line,
            self.state.cursor_col,
            item,
            &self.autocomplete_prefix,
        );
        self.push_undo_snapshot();
        self.last_action = None;
        self.state.lines = result.0;
        self.state.cursor_line = result.1;
        self.set_cursor_col(result.2);
        self.cancel_autocomplete();
        if let Some(on_change) = &self.on_change {
            on_change(&self.get_text());
        }
    }

    fn layout_text(&self, content_width: f64) -> Vec<LayoutLine> {
        let mut layout: Vec<LayoutLine> = Vec::new();
        for (line_index, line) in self.state.lines.iter().enumerate() {
            let chunks = word_wrap_line(line, content_width);
            let is_cursor_line = line_index == self.state.cursor_line;
            for (chunk_index, chunk) in chunks.iter().enumerate() {
                let has_cursor = is_cursor_line && self.state.cursor_col >= chunk.start_index && self.state.cursor_col <= chunk.end_index;
                let cursor_pos = if has_cursor {
                    Some(self.state.cursor_col - chunk.start_index)
                } else {
                    None
                };
                layout.push(LayoutLine {
                    text: chunk.text.clone(),
                    has_cursor,
                    cursor_pos,
                });
                let _ = chunk_index;
            }
        }
        if layout.is_empty() {
            layout.push(LayoutLine {
                text: String::new(),
                has_cursor: true,
                cursor_pos: Some(0),
            });
        }
        layout
    }

    fn handle_input_with(&mut self, keybindings: &KeybindingsManager, data: &str) {
        // Character jump mode.
        if let Some(jump_mode) = self.jump_mode.clone() {
            if keybindings.matches(data, "tui.editor.jumpForward") || keybindings.matches(data, "tui.editor.jumpBackward") {
                self.jump_mode = None;
                return;
            }
            if let Some(printable) = decode_printable_key(data) {
                let char = printable.chars().next().unwrap_or(' ');
                self.jump_mode = None;
                self.jump_to_char(char, &jump_mode);
                return;
            }
            self.jump_mode = None;
        }

        // Bracketed paste.
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

        // Ctrl+C passthrough.
        if keybindings.matches(data, "tui.input.copy") {
            return;
        }

        if keybindings.matches(data, "tui.editor.undo") {
            self.undo();
            return;
        }

        // Autocomplete mode.
        if self.autocomplete_state.is_some() {
            if keybindings.matches(data, "tui.select.cancel") {
                self.cancel_autocomplete();
                return;
            }
            if keybindings.matches(data, "tui.select.up") {
                let len = self.autocomplete_list.as_ref().map(|list| list.len()).unwrap_or(0);
                self.autocomplete_selected = if self.autocomplete_selected == 0 { len.saturating_sub(1) } else { self.autocomplete_selected - 1 };
                (self.request_render)();
                return;
            }
            if keybindings.matches(data, "tui.select.down") {
                let len = self.autocomplete_list.as_ref().map(|list| list.len()).unwrap_or(0);
                self.autocomplete_selected = if self.autocomplete_selected >= len.saturating_sub(1) { 0 } else { self.autocomplete_selected + 1 };
                (self.request_render)();
                return;
            }
            if keybindings.matches(data, "tui.input.tab") || keybindings.matches(data, "tui.select.confirm") {
                if let Some(item) = self.autocomplete_list.as_ref().and_then(|list| list.get(self.autocomplete_selected)).cloned() {
                    let is_command = self.autocomplete_prefix.starts_with('/');
                    self.apply_autocomplete(&item);
                    if is_command && keybindings.matches(data, "tui.select.confirm") {
                        // Fall through to submit.
                        self.submit_value();
                    }
                }
                return;
            }
        }

        if keybindings.matches(data, "tui.input.tab") && self.autocomplete_state.is_none() {
            self.handle_tab_completion();
            return;
        }

        if keybindings.matches(data, "tui.editor.deleteToLineEnd") {
            self.delete_to_end_of_line();
            return;
        }
        if keybindings.matches(data, "tui.editor.deleteToLineStart") {
            self.delete_to_start_of_line();
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
        if keybindings.matches(data, "tui.editor.deleteCharBackward") {
            self.handle_backspace();
            return;
        }
        if keybindings.matches(data, "tui.editor.deleteCharForward") {
            let line_len = self.current_line().chars().count();
            if self.state.cursor_col < line_len {
                self.push_undo_snapshot();
                let line = &mut self.state.lines[self.state.cursor_line];
                let after: String = line.chars().skip(self.state.cursor_col).collect();
                let graphemes = graphemes(&after);
                let grapheme_length = graphemes.first().map(|g| g.chars().count()).unwrap_or(1);
                let start: String = line.chars().take(self.state.cursor_col).collect();
                let end: String = line.chars().skip(self.state.cursor_col + grapheme_length).collect();
                *line = format!("{start}{end}");
            }
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
            if self.state.cursor_col > 0 {
                let line = self.current_line();
                let before: String = line.chars().take(self.state.cursor_col).collect();
                let graphemes = graphemes(&before);
                self.state.cursor_col -= graphemes.last().map(|g| g.chars().count()).unwrap_or(1);
            } else if self.state.cursor_line > 0 {
                self.state.cursor_line -= 1;
                self.state.cursor_col = self.state.lines[self.state.cursor_line].chars().count();
            }
            return;
        }
        if keybindings.matches(data, "tui.editor.cursorRight") {
            self.last_action = None;
            let line = self.current_line();
            if self.state.cursor_col < line.chars().count() {
                let after: String = line.chars().skip(self.state.cursor_col).collect();
                let graphemes = graphemes(&after);
                self.state.cursor_col += graphemes.first().map(|g| g.chars().count()).unwrap_or(1);
            } else if self.state.cursor_line < self.state.lines.len() - 1 {
                self.state.cursor_line += 1;
                self.state.cursor_col = 0;
            }
            return;
        }
        if keybindings.matches(data, "tui.editor.cursorUp") {
            self.move_to_visual_line(-1);
            return;
        }
        if keybindings.matches(data, "tui.editor.cursorDown") {
            self.move_to_visual_line(1);
            return;
        }
        if keybindings.matches(data, "tui.editor.cursorLineStart") {
            self.move_to_line_start();
            return;
        }
        if keybindings.matches(data, "tui.editor.cursorLineEnd") {
            self.move_to_line_end();
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
        if keybindings.matches(data, "tui.editor.jumpForward") {
            self.jump_mode = Some("forward".to_string());
            return;
        }
        if keybindings.matches(data, "tui.editor.jumpBackward") {
            self.jump_mode = Some("backward".to_string());
            return;
        }
        if keybindings.matches(data, "tui.editor.historyPrevious") {
            self.navigate_history(-1);
            return;
        }
        if keybindings.matches(data, "tui.editor.historyNext") {
            self.navigate_history(1);
            return;
        }
        if keybindings.matches(data, "tui.input.newLine") {
            self.add_new_line();
            if let Some(on_change) = &self.on_change {
                on_change(&self.get_text());
            }
            return;
        }
        if keybindings.matches(data, "tui.input.submit") || data == "\n" {
            if !self.disable_submit {
                self.submit_value();
            }
            return;
        }
        if keybindings.matches(data, "tui.select.cancel") {
            self.exit_history_browsing();
            return;
        }

        if let Some(kitty_printable) = decode_printable_key(data) {
            self.insert_character(&kitty_printable, false);
            if let Some(on_change) = &self.on_change {
                on_change(&self.get_text());
            }
            return;
        }

        let has_control_chars = data.chars().any(|ch| {
            let code = ch as u32;
            code < 32 || code == 0x7f || (0x80..=0x9f).contains(&code)
        });
        if !has_control_chars {
            self.insert_character(data, false);
            if let Some(on_change) = &self.on_change {
                on_change(&self.get_text());
            }
        }
    }

    fn process_paste_chunk(&mut self, data: &str) {
        self.paste_buffer.push_str(data);
        if let Some(end_index) = self.paste_buffer.find("\x1b[201~") {
            let paste_content = self.paste_buffer[..end_index].to_string();
            if !paste_content.is_empty() {
                self.handle_paste(&paste_content);
            }
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

fn is_wide_char(char: char) -> bool {
    let cp = char as u32;
    (0x1100..=0x115f).contains(&cp)
        || (0x2e80..=0x303e).contains(&cp)
        || (0x3041..=0x33ff).contains(&cp)
        || (0x3400..=0x9fff).contains(&cp)
        || (0xac00..=0xd7a3).contains(&cp)
        || (0xf900..=0xfaff).contains(&cp)
        || (0xff00..=0xff60).contains(&cp)
        || (0x1f000..=0x1faff).contains(&cp)
}

impl Component for Editor {
    fn render(&self, width: usize) -> Vec<String> {
        let max_padding = ((width as f64 - 1.0) / 2.0).max(0.0).floor() as usize;
        let padding_x = self.padding_x.min(max_padding);
        let content_width = (width as f64 - padding_x as f64 * 2.0).max(1.0);
        let layout_width = if padding_x > 0 {
            content_width
        } else {
            (content_width - 1.0).max(1.0)
        };

        let horizontal = (self.border_color)("─");
        let layout_lines = self.layout_text(layout_width);

        let max_visible_lines = 5usize.max((80.0f64 * 0.3).floor() as usize);

        let cursor_line_index = layout_lines
            .iter()
            .position(|line| line.has_cursor)
            .unwrap_or(0);
        let mut scroll_offset = self.scroll_offset;
        if cursor_line_index < scroll_offset {
            scroll_offset = cursor_line_index;
        } else if cursor_line_index >= scroll_offset + max_visible_lines {
            scroll_offset = cursor_line_index - max_visible_lines + 1;
        }
        let max_scroll_offset = layout_lines.len().saturating_sub(max_visible_lines);
        scroll_offset = scroll_offset.min(max_scroll_offset);

        let visible_lines = &layout_lines[scroll_offset..(scroll_offset + max_visible_lines).min(layout_lines.len())];

        let mut result: Vec<String> = Vec::new();
        let left_padding = " ".repeat(padding_x);
        let right_padding = left_padding.clone();

        if scroll_offset > 0 {
            result.push((self.border_color)(&format!("↑ {}", scroll_offset)));
        } else {
            result.push(horizontal.repeat(width));
        }

        let emit_cursor_marker = self.focused;

        for layout_line in visible_lines {
            let mut display_text = layout_line.text.clone();
            let mut line_visible_width = visible_width(&layout_line.text);
            let mut cursor_in_padding = false;

            if layout_line.has_cursor {
                if let Some(cursor_pos) = layout_line.cursor_pos {
                    let before: String = display_text.chars().take(cursor_pos).collect();
                    let after: String = display_text.chars().skip(cursor_pos).collect();
                    let marker = if emit_cursor_marker { CURSOR_MARKER.to_string() } else { String::new() };

                    if !after.is_empty() {
                        let after_graphemes = graphemes(&after);
                        let first_grapheme = after_graphemes.first().cloned().unwrap_or_default();
                        let rest_after: String = after.chars().skip(first_grapheme.chars().count()).collect();
                        let cursor = format!("\x1b[7m{first_grapheme}\x1b[0m");
                        display_text = format!("{before}{marker}{cursor}{rest_after}");
                    } else {
                        let cursor = "\x1b[7m \x1b[0m";
                        display_text = format!("{before}{marker}{cursor}");
                        line_visible_width += 1.0;
                        if line_visible_width > content_width && padding_x > 0 {
                            cursor_in_padding = true;
                        }
                    }
                }
            }

            let padding = " ".repeat(((content_width - line_visible_width).max(0.0)) as usize);
            let line_right_padding = if cursor_in_padding {
                right_padding.chars().skip(1).collect::<String>()
            } else {
                right_padding.clone()
            };
            result.push(format!("{left_padding}{display_text}{padding}{line_right_padding}"));
        }

        let lines_below = layout_lines.len().saturating_sub(scroll_offset + visible_lines.len());
        if lines_below > 0 {
            result.push((self.border_color)(&format!("↓ {}", lines_below)));
        } else {
            result.push(horizontal.repeat(width));
        }

        // Autocomplete list.
        if self.autocomplete_state.is_some() {
            if let Some(list) = &self.autocomplete_list {
                for (index, item) in list.iter().take(self.autocomplete_max_visible).enumerate() {
                    let prefix = if index == self.autocomplete_selected { "→ " } else { "  " };
                    let line = format!("{prefix}{}", item.label);
                    let line_padding = " ".repeat(content_width as usize - (visible_width(&line) as usize).min(content_width as usize));
                    result.push(format!("{left_padding}{line}{line_padding}{right_padding}"));
                }
            }
        }

        result
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

    fn editor() -> Editor {
        Editor::new(
            EditorTheme {
                border_color: Arc::new(|text| text.to_string()),
            },
            EditorOptions::default(),
            Arc::new(|| {}),
        )
    }

    #[test]
    fn word_wrap_splits_long_lines() {
        let chunks = word_wrap_line("hello world", 5.0);
        assert!(chunks.len() >= 2);
        assert_eq!(chunks[0].text, "hello");
        assert_eq!(chunks[0].start_index, 0);
        assert_eq!(chunks[0].end_index, 5);
        assert!(chunks.last().unwrap().text.contains("world"));
    }

    #[test]
    fn word_wrap_short_line_single_chunk() {
        let chunks = word_wrap_line("hi", 10.0);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "hi");
    }

    #[test]
    fn word_wrap_force_breaks_long_words() {
        let chunks = word_wrap_line("abcdefgh", 3.0);
        assert!(chunks.len() >= 3);
        assert_eq!(chunks[0].text, "abc");
        assert_eq!(chunks[1].text, "def");
    }

    #[test]
    fn word_wrap_cjk_breaks_anywhere() {
        let chunks = word_wrap_line("你好世界", 4.0);
        assert!(chunks.len() >= 2);
        assert_eq!(chunks[0].text, "你好");
    }

    #[test]
    fn inserts_and_deletes() {
        let mut ed = editor();
        let kb = keybindings();
        ed.handle_input_with(&kb, "h");
        ed.handle_input_with(&kb, "i");
        assert_eq!(ed.get_text(), "hi");
        ed.handle_input_with(&kb, "\x7f");
        assert_eq!(ed.get_text(), "h");
        assert_eq!(ed.state.cursor_col, 1);
    }

    #[test]
    fn multi_line_editing() {
        let mut ed = editor();
        let kb = keybindings();
        ed.handle_input_with(&kb, "a");
        ed.handle_input_with(&kb, "b");
        ed.handle_input_with(&kb, "\x0a"); // ctrl+j newline
        ed.handle_input_with(&kb, "c");
        assert_eq!(ed.get_text(), "ab\nc");
        assert_eq!(ed.state.cursor_line, 1);
        ed.handle_input_with(&kb, "\x7f"); // deletes 'c'
        ed.handle_input_with(&kb, "\x7f"); // at line start: joins with previous (empty) line
        assert_eq!(ed.get_text(), "ab");
    }

    #[test]
    fn cursor_navigation() {
        let mut ed = editor();
        let kb = keybindings();
        ed.handle_input_with(&kb, "a");
        ed.handle_input_with(&kb, "b");
        ed.handle_input_with(&kb, "c");
        ed.handle_input_with(&kb, "\x1b[D"); // left
        assert_eq!(ed.state.cursor_col, 2);
        ed.handle_input_with(&kb, "\x1bOH"); // home
        assert_eq!(ed.state.cursor_col, 0);
        ed.handle_input_with(&kb, "\x1bOF"); // end
        assert_eq!(ed.state.cursor_col, 3);
    }

    #[test]
    fn delete_to_line_start_and_yank() {
        let mut ed = editor();
        let kb = keybindings();
        ed.handle_input_with(&kb, "a");
        ed.handle_input_with(&kb, "b");
        ed.handle_input_with(&kb, "c");
        ed.handle_input_with(&kb, "\x15"); // ctrl+u
        assert_eq!(ed.get_text(), "");
        ed.handle_input_with(&kb, "\x19"); // ctrl+y
        assert_eq!(ed.get_text(), "abc");
    }

    #[test]
    fn undo_restores() {
        let mut ed = editor();
        let kb = keybindings();
        ed.handle_input_with(&kb, "x");
        ed.handle_input_with(&kb, "y");
        assert_eq!(ed.get_text(), "xy");
        ed.handle_input_with(&kb, "\x1b[27;5;45~"); // ctrl+-
        assert_eq!(ed.get_text(), "");
    }

    #[test]
    fn history_navigation() {
        let mut ed = editor();
        let kb = keybindings();
        ed.add_to_history("first");
        ed.add_to_history("second");
        ed.navigate_history(-1);
        assert_eq!(ed.get_text(), "second");
        // Going further back is clamped (JS returns at the boundary).
        ed.navigate_history(-1);
        assert_eq!(ed.get_text(), "second");
        ed.navigate_history(1);
        assert_eq!(ed.get_text(), "first");
        ed.navigate_history(1);
        assert_eq!(ed.get_text(), "first");
    }

    #[test]
    fn submit_callback() {
        let mut ed = editor();
        let submitted = Arc::new(Mutex::new(None::<String>));
        let submitted_clone = submitted.clone();
        ed.on_submit = Some(Arc::new(move |text| {
            *submitted_clone.lock().unwrap() = Some(text.to_string());
        }));
        let kb = keybindings();
        ed.handle_input_with(&kb, "hello");
        ed.handle_input_with(&kb, "\r");
        assert_eq!(submitted.lock().unwrap().as_deref(), Some("hello"));
    }

    #[test]
    fn renders_with_cursor_and_borders() {
        let mut ed = editor();
        ed.set_text("hello");
        ed.focused = true;
        let lines = ed.render(30);
        assert!(lines.len() >= 3);
        assert!(lines[0].contains('─'));
        assert!(lines.last().unwrap().contains('─'));
        assert!(lines[1].contains(CURSOR_MARKER));
    }

    #[test]
    fn word_delete_uses_kill_ring() {
        let mut ed = editor();
        let kb = keybindings();
        for ch in ["a", "b", " ", "c", "d"] {
            ed.handle_input_with(&kb, ch);
        }
        ed.handle_input_with(&kb, "\x17"); // ctrl+w delete word backward
        assert_eq!(ed.get_text(), "ab ");
        assert_eq!(ed.kill_ring.peek(), Some("cd"));
    }

    #[test]
    fn paste_handles_newlines() {
        let mut ed = editor();
        let kb = keybindings();
        ed.handle_input_with(&kb, "\x1b[200~a\nb\x1b[201~");
        assert_eq!(ed.get_text(), "a\nb");
        assert_eq!(ed.state.cursor_line, 1);
    }
}
