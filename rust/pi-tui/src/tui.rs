//! TUI core types, port of `packages/tui/src/tui.ts` (core parts).
//!
//! The full TUI class (terminal integration, rendering loop) is ported
//! separately; this module holds the Component/Container abstraction, the
//! cursor marker, and line compositing.

use std::sync::Arc;

use crate::terminal_image::is_image_line;
use crate::utils::{extract_segments, slice_by_column, slice_with_width, visible_width};

/// Cursor position marker — APC sequence that terminals ignore. Components
/// emit this at the cursor position when focused.
pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";

const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

/// A renderable UI component.
pub trait Component: Send + Sync {
    /// Render the component to lines for the given viewport width.
    fn render(&self, width: usize) -> Vec<String>;

    /// Optional handler for keyboard input when the component has focus.
    fn handle_input(&mut self, _data: &str) {}

    /// True when the component wants key release events (Kitty protocol).
    fn wants_key_release(&self) -> bool {
        false
    }

    /// Invalidate any cached rendering state.
    fn invalidate(&mut self) {}
}

/// Container with child components.
pub struct Container {
    pub children: Vec<Arc<dyn Component>>,
}

impl Container {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    pub fn add_child(&mut self, component: Arc<dyn Component>) {
        self.children.push(component);
    }

    pub fn remove_child(&mut self, component: &Arc<dyn Component>) {
        self.children.retain(|child| !Arc::ptr_eq(child, component));
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }

    pub fn invalidate(&mut self) {
        // Components are immutable in this model; invalidation is a no-op
        // unless a component carries interior mutability.
    }
}

impl Component for Container {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        for child in &self.children {
            for line in child.render(width) {
                lines.push(line);
            }
        }
        lines
    }
}

/// Composite overlay content into a terminal line at a fixed column.
pub fn composite_tui_line(
    base_line: &str,
    overlay_line: &str,
    start_col: f64,
    overlay_width: f64,
    total_width: f64,
) -> String {
    if is_image_line(base_line) {
        return base_line.to_string();
    }

    let after_start = start_col + overlay_width;
    let (before, before_width, after, after_width) =
        extract_segments(base_line, start_col, after_start, total_width - after_start, true);
    let (overlay_text, overlay_actual_width) = slice_with_width(overlay_line, 0.0, overlay_width, true);
    let before_pad = (start_col - before_width).max(0.0);
    let overlay_pad = (overlay_width - overlay_actual_width).max(0.0);
    let actual_before_width = start_col.max(before_width);
    let actual_overlay_width = overlay_width.max(overlay_actual_width);
    let after_target = (total_width - actual_before_width - actual_overlay_width).max(0.0);
    let after_pad = (after_target - after_width).max(0.0);

    let mut result = String::new();
    result.push_str(&before);
    result.push_str(&" ".repeat(before_pad as usize));
    result.push_str(SEGMENT_RESET);
    result.push_str(&overlay_text);
    result.push_str(&" ".repeat(overlay_pad as usize));
    result.push_str(SEGMENT_RESET);
    result.push_str(&after);
    result.push_str(&" ".repeat(after_pad as usize));

    if visible_width(&result) <= total_width {
        result
    } else {
        slice_by_column(&result, 0.0, total_width, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TextComponent {
        text: String,
    }

    impl Component for TextComponent {
        fn render(&self, _width: usize) -> Vec<String> {
            vec![self.text.clone()]
        }
    }

    #[test]
    fn container_concatenates_children() {
        let mut container = Container::new();
        container.add_child(Arc::new(TextComponent {
            text: "one".to_string(),
        }));
        container.add_child(Arc::new(TextComponent {
            text: "two".to_string(),
        }));
        let lines = container.render(10);
        assert_eq!(lines, vec!["one", "two"]);
    }

    #[test]
    fn container_remove_and_clear() {
        let mut container = Container::new();
        let child = Arc::new(TextComponent {
            text: "one".to_string(),
        });
        let child: Arc<dyn Component> = child;
        container.add_child(child.clone());
        container.remove_child(&child);
        assert!(container.render(10).is_empty());
        container.add_child(child.clone());
        container.clear();
        assert!(container.render(10).is_empty());
    }

    #[test]
    fn composite_overlay_onto_empty() {
        let result = composite_tui_line("", "hello", 2.0, 5.0, 10.0);
        assert_eq!(visible_width(&result), 10.0);
        assert!(result.contains("hello"));
    }

    #[test]
    fn composite_replaces_middle() {
        let result = composite_tui_line("abcdefgh", "XY", 2.0, 2.0, 8.0);
        assert_eq!(visible_width(&result), 8.0);
        assert!(result.contains("XY"));
        assert!(result.contains('a'));
        assert!(result.contains('h'));
    }

    #[test]
    fn composite_truncates_overwide() {
        let result = composite_tui_line("abcdefgh", "LONG", 6.0, 4.0, 8.0);
        assert_eq!(visible_width(&result), 8.0);
    }
}
