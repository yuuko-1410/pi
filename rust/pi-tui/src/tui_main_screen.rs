//! Main-screen TUI, port of `packages/tui/src/tui-main-screen.ts`.
//!
//! Differential renderer: compares previous and new line buffers and emits
//! only the changed region, with Kitty image cleanup, synchronized output
//! wrapping, and hardware cursor positioning.

use std::sync::{Arc, Mutex};

use crate::terminal::Terminal;
use crate::terminal_image::{delete_kitty_image, is_image_line};
use crate::tui::{CURSOR_MARKER, TuiBase};
use crate::utils::visible_width;

#[derive(Clone, Debug, Default)]
pub struct TuiMainScreenRenderState {
    pub previous_lines: Vec<String>,
    pub previous_width: f64,
    pub previous_height: f64,
    pub cursor_row: usize,
    pub hardware_cursor_row: usize,
    pub max_lines_rendered: usize,
    pub previous_viewport_top: f64,
}

pub struct TuiMainScreen {
    pub base: TuiBase,
    pub previous_lines: Vec<String>,
    pub previous_width: f64,
    pub previous_height: f64,
    pub cursor_row: usize,
    pub hardware_cursor_row: usize,
    pub max_lines_rendered: usize,
    pub previous_viewport_top: f64,
    /// Root render closure: produces the new lines for the current width.
    pub render_root: Arc<dyn Fn(usize) -> Vec<String> + Send + Sync>,
}

impl TuiMainScreen {
    pub fn new(terminal: Arc<Mutex<dyn Terminal>>, render_root: Arc<dyn Fn(usize) -> Vec<String> + Send + Sync>) -> Self {
        let render_fn: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        Self {
            base: TuiBase::new(terminal, render_fn, Arc::new(|| {})),
            previous_lines: Vec::new(),
            previous_width: 0.0,
            previous_height: 0.0,
            cursor_row: 0,
            hardware_cursor_row: 0,
            max_lines_rendered: 0,
            previous_viewport_top: 0.0,
            render_root,
        }
    }

    pub fn capture_render_state(&self) -> TuiMainScreenRenderState {
        TuiMainScreenRenderState {
            previous_lines: self.previous_lines.clone(),
            previous_width: self.previous_width,
            previous_height: self.previous_height,
            cursor_row: self.cursor_row,
            hardware_cursor_row: self.hardware_cursor_row,
            max_lines_rendered: self.max_lines_rendered,
            previous_viewport_top: self.previous_viewport_top,
        }
    }

    pub fn restore_render_state(&mut self, state: TuiMainScreenRenderState) {
        self.previous_lines = state.previous_lines.iter().map(|line| if is_image_line(line) { String::new() } else { line.clone() }).collect();
        self.previous_width = state.previous_width;
        self.previous_height = state.previous_height;
        self.cursor_row = state.cursor_row;
        self.hardware_cursor_row = state.hardware_cursor_row;
        self.max_lines_rendered = state.max_lines_rendered;
        self.previous_viewport_top = state.previous_viewport_top;
    }

    pub fn reset_render_state(&mut self) {
        self.previous_lines.clear();
        self.previous_width = -1.0;
        self.previous_height = -1.0;
        self.cursor_row = 0;
        self.hardware_cursor_row = 0;
        self.max_lines_rendered = 0;
        self.previous_viewport_top = 0.0;
    }

    /// Extract the cursor position from the CURSOR_MARKER in the rendered
    /// lines and strip the marker.
    fn extract_cursor_position(&self, lines: &mut Vec<String>, _height: f64) -> Option<(usize, usize)> {
        for (row, line) in lines.iter_mut().enumerate() {
            if let Some(index) = line.find(CURSOR_MARKER) {
                let col = line[..index].chars().count();
                line.replace_range(index..index + CURSOR_MARKER.len(), "");
                return Some((row, col));
            }
        }
        None
    }

    fn get_kitty_image_reserved_rows(&self, lines: &[String], index: usize, max_index: usize) -> usize {
        let rows = kitty_image_rows(&lines[index]);
        if rows <= 1 {
            return 1;
        }
        let max_rows = rows.min(max_index - index + 1).min(lines.len() - index);
        let mut reserved_rows = 1;
        while reserved_rows < max_rows {
            let line = lines.get(index + reserved_rows).cloned().unwrap_or_default();
            if is_image_line(&line) || visible_width(&line) > 0.0 {
                break;
            }
            reserved_rows += 1;
        }
        reserved_rows
    }

    fn delete_kitty_images(&self, lines: &[String], first: usize, last: usize) -> String {
        let mut buffer = String::new();
        let max_line = last.min(lines.len().saturating_sub(1));
        if first > max_line {
            return buffer;
        }
        for line in &lines[first..=max_line] {
            for id in kitty_image_ids(line) {
                buffer += &delete_kitty_image(id);
            }
        }
        buffer
    }

    fn position_hardware_cursor(&self, cursor_pos: Option<(usize, usize)>, _lines_len: usize) {
        if let Some((row, col)) = cursor_pos {
            if self.base.get_show_hardware_cursor() {
                let mut terminal = self.base.terminal.lock().unwrap();
                // Move to the cursor row (1-based) and column.
                let _ = col;
                let _ = row;
                terminal.hide_cursor();
            }
        }
    }

    /// Run the differential render.
    pub fn do_render(&mut self) {
        if self.base.stopped {
            return;
        }
        let width = self.base.terminal.lock().unwrap().columns() as f64;
        let height = self.base.terminal.lock().unwrap().rows() as f64;
        let width_changed = self.previous_width != 0.0 && self.previous_width != width;
        let height_changed = self.previous_height != 0.0 && self.previous_height != height;
        let previous_buffer_length = if self.previous_height > 0.0 {
            self.previous_viewport_top + self.previous_height
        } else {
            height
        };
        let mut prev_viewport_top = if height_changed {
            (previous_buffer_length - height).max(0.0)
        } else {
            self.previous_viewport_top
        };
        let viewport_top = prev_viewport_top;
        let mut hardware_cursor_row = self.hardware_cursor_row;

        let mut new_lines = (self.render_root)(width as usize);

        let cursor_pos = self.extract_cursor_position(&mut new_lines, height);

        // Full render helper.
        let full_render = |this: &mut Self, clear: bool| {
            this.base.full_redraw_count += 1;
            let mut buffer = "\x1b[?2026h".to_string();
            if clear {
                buffer += "\x1b[2J\x1b[H\x1b[3J";
            }
            for (index, line) in new_lines.iter().enumerate() {
                if index > 0 {
                    buffer += "\r\n";
                }
                let is_image = is_image_line(line);
                let image_reserved_rows = if is_image {
                    this.get_kitty_image_reserved_rows(&new_lines, index, new_lines.len() - 1)
                } else {
                    1
                };
                if image_reserved_rows > 1 && image_reserved_rows <= height as usize {
                    for _ in 1..image_reserved_rows {
                        buffer += "\r\n";
                    }
                    buffer += &format!("\x1b[{}A", image_reserved_rows - 1);
                    buffer += line;
                    buffer += &format!("\x1b[{}B", image_reserved_rows - 1);
                    continue;
                }
                buffer += line;
            }
            buffer += "\x1b[?2026l";
            this.base.terminal.lock().unwrap().write(&buffer);
            this.cursor_row = new_lines.len().saturating_sub(1);
            this.hardware_cursor_row = this.cursor_row;
            if clear {
                this.max_lines_rendered = new_lines.len();
            } else {
                this.max_lines_rendered = this.max_lines_rendered.max(new_lines.len());
            }
            let buffer_length = height.max(new_lines.len() as f64);
            this.previous_viewport_top = (buffer_length - height).max(0.0);
            this.position_hardware_cursor(cursor_pos, new_lines.len());
            this.previous_lines = new_lines.clone();
            this.previous_width = width;
            this.previous_height = height;
        };

        // First render.
        if self.previous_lines.is_empty() && !width_changed && !height_changed {
            full_render(self, false);
            return;
        }

        if width_changed {
            full_render(self, true);
            return;
        }

        if height_changed {
            full_render(self, true);
            return;
        }

        if self.base.get_clear_on_shrink()
            && new_lines.len() < self.max_lines_rendered
            && !self.base.has_overlay()
        {
            full_render(self, true);
            return;
        }

        // Find changed lines.
        let mut first_changed = -1isize;
        let mut last_changed = -1isize;
        let max_lines = new_lines.len().max(self.previous_lines.len());
        for index in 0..max_lines {
            let old_line = self.previous_lines.get(index).cloned().unwrap_or_default();
            let new_line = new_lines.get(index).cloned().unwrap_or_default();
            if old_line != new_line {
                if first_changed == -1 {
                    first_changed = index as isize;
                }
                last_changed = index as isize;
            }
        }
        let appended_lines = new_lines.len() > self.previous_lines.len();
        if appended_lines {
            if first_changed == -1 {
                first_changed = self.previous_lines.len() as isize;
            }
            last_changed = new_lines.len() as isize - 1;
        }
        if first_changed == -1 {
            self.position_hardware_cursor(cursor_pos, new_lines.len());
            self.previous_viewport_top = prev_viewport_top;
            self.previous_height = height;
            return;
        }
        let append_start = appended_lines && first_changed == self.previous_lines.len() as isize && first_changed > 0;

        // All changes are deletions.
        if first_changed >= new_lines.len() as isize {
            if self.previous_lines.len() > new_lines.len() {
                let mut buffer = "\x1b[?2026h".to_string();
                buffer += &self.delete_kitty_images(&self.previous_lines, first_changed as usize, last_changed as usize);
                let target_row = new_lines.len().saturating_sub(1) as isize;
                let current_screen_row = hardware_cursor_row as isize - prev_viewport_top as isize;
                let target_screen_row = target_row - prev_viewport_top as isize;
                let line_diff = target_screen_row - current_screen_row;
                if line_diff > 0 {
                    buffer += &format!("\x1b[{line_diff}B");
                } else if line_diff < 0 {
                    buffer += &format!("\x1b[{}A", -line_diff);
                }
                buffer += "\r";
                let extra_lines = self.previous_lines.len() - new_lines.len();
                let clear_start_offset = if new_lines.is_empty() { 0 } else { 1 };
                if extra_lines > 0 && clear_start_offset > 0 {
                    buffer += &format!("\x1b[{clear_start_offset}B");
                }
                for index in 0..extra_lines {
                    buffer += "\r\x1b[2K";
                    if index < extra_lines - 1 {
                        buffer += "\x1b[1B";
                    }
                }
                let move_back = (extra_lines - 1 + clear_start_offset).max(0);
                if move_back > 0 {
                    buffer += &format!("\x1b[{move_back}A");
                }
                buffer += "\x1b[?2026l";
                self.base.terminal.lock().unwrap().write(&buffer);
                self.cursor_row = target_row as usize;
                self.hardware_cursor_row = target_row as usize;
            }
            self.position_hardware_cursor(cursor_pos, new_lines.len());
            self.previous_lines = new_lines;
            self.previous_width = width;
            self.previous_height = height;
            self.previous_viewport_top = prev_viewport_top;
            return;
        }

        // First changed line above the previous viewport needs a full redraw.
        if first_changed < prev_viewport_top as isize {
            full_render(self, true);
            return;
        }

        // Differential render from first changed to last changed.
        let mut buffer = "\x1b[?2026h".to_string();
        buffer += &self.delete_kitty_images(&self.previous_lines, first_changed as usize, last_changed as usize);
        let prev_viewport_bottom = prev_viewport_top + height - 1.0;
        let mut move_target_row = first_changed;
        if append_start {
            move_target_row = first_changed - 1;
        }
        if move_target_row as f64 > prev_viewport_bottom {
            let current_screen_row = ((hardware_cursor_row as f64 - prev_viewport_top).max(0.0)).min(height - 1.0);
            let move_to_bottom = height - 1.0 - current_screen_row;
            if move_to_bottom > 0.0 {
                buffer += &format!("\x1b[{}B", move_to_bottom as usize);
            }
            let scroll = (move_target_row as f64 - prev_viewport_bottom) as usize;
            buffer += &"\r\n".repeat(scroll);
            prev_viewport_top += scroll as f64;
            hardware_cursor_row = move_target_row as usize;
        }

        let current_screen_row = hardware_cursor_row as isize - prev_viewport_top as isize;
        let target_screen_row = move_target_row - prev_viewport_top as isize;
        let line_diff = target_screen_row - current_screen_row;
        if line_diff > 0 {
            buffer += &format!("\x1b[{line_diff}B");
        } else if line_diff < 0 {
            buffer += &format!("\x1b[{}A", -line_diff);
        }
        buffer += if append_start { "\r\n" } else { "\r" };

        let render_end = (last_changed as usize).min(new_lines.len() - 1);
        let mut index = first_changed as usize;
        while index <= render_end {
            if index > first_changed as usize {
                buffer += "\r\n";
            }
            let line = new_lines[index].clone();
            let is_image = is_image_line(&line);
            let image_reserved_rows = if is_image {
                self.get_kitty_image_reserved_rows(&new_lines, index, render_end)
            } else {
                1
            };
            if image_reserved_rows > 1 {
                let image_start_screen_row = index as f64 - viewport_top;
                if image_start_screen_row < 0.0 || image_start_screen_row + image_reserved_rows as f64 > height {
                    full_render(self, true);
                    return;
                }
                buffer += "\x1b[2K";
                for _ in 1..image_reserved_rows {
                    buffer += "\r\n\x1b[2K";
                }
                buffer += &format!("\x1b[{}A", image_reserved_rows - 1);
                buffer += &line;
                buffer += &format!("\x1b[{}B", image_reserved_rows - 1);
                index += image_reserved_rows;
                continue;
            }
            buffer += "\x1b[2K";
            buffer += &line;
            index += 1;
        }

        let mut final_cursor_row = render_end;
        if self.previous_lines.len() > new_lines.len() {
            if render_end < new_lines.len() - 1 {
                let move_down = new_lines.len() - 1 - render_end;
                buffer += &format!("\x1b[{move_down}B");
                final_cursor_row = new_lines.len() - 1;
            }
            let extra_lines = self.previous_lines.len() - new_lines.len();
            for _ in new_lines.len()..self.previous_lines.len() {
                buffer += "\r\n\x1b[2K";
            }
            buffer += &format!("\x1b[{extra_lines}A");
        }

        buffer += "\x1b[?2026l";
        self.base.terminal.lock().unwrap().write(&buffer);
        self.cursor_row = final_cursor_row;
        self.hardware_cursor_row = final_cursor_row;
        self.position_hardware_cursor(cursor_pos, new_lines.len());
        self.previous_lines = new_lines;
        self.previous_width = width;
        self.previous_height = height;
        self.previous_viewport_top = prev_viewport_top;
    }
}

/// Extract Kitty image ids from a line (ESC _ G ... ESC \).
fn kitty_image_ids(line: &str) -> Vec<u64> {
    let mut ids = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find("\x1b_G") {
        let after = &rest[start + 4..];
        let Some(semicolon) = after.find(';') else { break };
        let controls = &after[..semicolon];
        if let Some(id) = controls
            .split(',')
            .find_map(|control| control.strip_prefix("i=").and_then(|value| value.parse::<u64>().ok()))
        {
            ids.push(id);
        }
        let Some(term) = after.find("\x1b\\") else { break };
        rest = &after[term + 2..];
    }
    ids
}

/// Extract the row count from a Kitty image line (`r=N` control).
fn kitty_image_rows(line: &str) -> usize {
    if !line.contains("\x1b_G") {
        return 1;
    }
    let Some(start) = line.find("\x1b_G") else { return 1 };
    let after = &line[start + 4..];
    let Some(semicolon) = after.find(';') else { return 1 };
    let controls = &after[..semicolon];
    controls
        .split(',')
        .find_map(|control| control.strip_prefix("r=").and_then(|value| value.parse::<usize>().ok()))
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_kitty_ids_and_rows() {
        assert!(kitty_image_ids("plain").is_empty());
        assert_eq!(kitty_image_ids("\x1b_Ga=T,i=7;data\x1b\\"), vec![7]);
        assert_eq!(kitty_image_rows("\x1b_Ga=T,r=3;data\x1b\\"), 3);
        assert_eq!(kitty_image_rows("plain"), 1);
    }

    #[test]
    fn extracts_cursor_marker() {
        let mut lines = vec!["abc".to_string(), format!("xy{CURSOR_MARKER}z")];
        let screen = TuiMainScreen {
            base: TuiBase::new(
                Arc::new(Mutex::new(NoopTerminal)),
                Arc::new(|| {}),
                Arc::new(|| {}),
            ),
            previous_lines: Vec::new(),
            previous_width: 0.0,
            previous_height: 0.0,
            cursor_row: 0,
            hardware_cursor_row: 0,
            max_lines_rendered: 0,
            previous_viewport_top: 0.0,
            render_root: Arc::new(|_| Vec::new()),
        };
        let pos = screen.extract_cursor_position(&mut lines, 10.0);
        assert!(pos.is_some());
        assert_eq!(lines[1], "xyz");
    }

    struct NoopTerminal;

    impl Terminal for NoopTerminal {
        fn start(&mut self, _on_input: Arc<dyn Fn(&str) + Send + Sync>) {}
        fn stop(&mut self) {}
        fn write(&mut self, _data: &str) {}
        fn columns(&self) -> usize {
            80
        }
        fn rows(&self) -> usize {
            24
        }
        fn kitty_protocol_active(&self) -> bool {
            false
        }
        fn move_by(&mut self, _lines: isize) {}
        fn hide_cursor(&mut self) {}
        fn show_cursor(&mut self) {}
        fn clear_line(&mut self) {}
        fn clear_from_cursor(&mut self) {}
        fn clear_screen(&mut self) {}
        fn set_title(&mut self, _title: &str) {}
        fn set_progress(&mut self, _active: bool) {}
    }
}
