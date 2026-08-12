//! Basic components, ports of `packages/tui/src/components/{text,box,spacer,h-stack,v-stack}.ts`.
//!
//! Differences: render caches are omitted (recomputed each frame); the JS
//! `Stack` base class is replaced by layout-node sizing functions.

use std::sync::Arc;

use crate::layout_node::{allocate_stack_sizes, visible_stack_entries, LayoutAlign, LayoutViewport, StackLayoutEntry};
use crate::tui::{composite_tui_line, Component};
use crate::utils::{visible_width, wrap_text_with_ansi};

/// Text component: multi-line text with word wrapping and padding.
#[derive(Clone)]
pub struct Text {
    text: String,
    padding_x: usize,
    padding_y: usize,
    custom_bg_fn: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>,
}

impl Text {
    pub fn new(text: &str, padding_x: usize, padding_y: usize, custom_bg_fn: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>) -> Self {
        Self {
            text: text.to_string(),
            padding_x,
            padding_y,
            custom_bg_fn,
        }
    }

    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
    }
}

impl Component for Text {
    fn render(&self, width: usize) -> Vec<String> {
        if self.text.trim().is_empty() {
            return Vec::new();
        }
        let normalized = self.text.replace('\t', "   ");
        let content_width = ((width as isize) - (self.padding_x as isize) * 2).max(1) as usize;
        let wrapped = wrap_text_with_ansi(&normalized, content_width as f64);
        let left_margin = " ".repeat(self.padding_x);
        let right_margin = " ".repeat(self.padding_x);
        let mut content_lines: Vec<String> = Vec::new();
        for line in wrapped {
            let line_with_margins = format!("{left_margin}{line}{right_margin}");
            match &self.custom_bg_fn {
                Some(bg_fn) => content_lines.push(apply_background_to_line(&line_with_margins, width, bg_fn.as_ref())),
                None => {
                    let visible_len = visible_width(&line_with_margins);
                    let padding = ((width as f64) - visible_len).max(0.0) as usize;
                    content_lines.push(format!("{line_with_margins}{}", " ".repeat(padding)));
                }
            }
        }
        let empty_line = " ".repeat(width);
        let mut result: Vec<String> = Vec::new();
        for _ in 0..self.padding_y {
            result.push(match &self.custom_bg_fn {
                Some(bg_fn) => apply_background_to_line(&empty_line, width, bg_fn.as_ref()),
                None => empty_line.clone(),
            });
        }
        result.extend(content_lines);
        for _ in 0..self.padding_y {
            result.push(match &self.custom_bg_fn {
                Some(bg_fn) => apply_background_to_line(&empty_line, width, bg_fn.as_ref()),
                None => empty_line.clone(),
            });
        }
        if result.is_empty() {
            vec![String::new()]
        } else {
            result
        }
    }
}

/// Apply background to a line, padding to full width.
pub fn apply_background_to_line(line: &str, width: usize, bg_fn: &dyn Fn(&str) -> String) -> String {
    let visible_len = visible_width(line);
    let padding_needed = ((width as f64) - visible_len).max(0.0) as usize;
    let with_padding = format!("{line}{}", " ".repeat(padding_needed));
    bg_fn(&with_padding)
}

/// Box component: applies padding and background to all children.
pub struct Box {
    pub children: Vec<Arc<dyn Component>>,
    padding_x: usize,
    padding_y: usize,
    bg_fn: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>,
}

impl Box {
    pub fn new(padding_x: usize, padding_y: usize, bg_fn: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>) -> Self {
        Self {
            children: Vec::new(),
            padding_x,
            padding_y,
            bg_fn,
        }
    }

    pub fn add_child(&mut self, component: Arc<dyn Component>) {
        self.children.push(component);
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }
}

impl Component for Box {
    fn render(&self, width: usize) -> Vec<String> {
        let content_width = ((width as isize) - (self.padding_x as isize) * 2).max(1) as usize;
        let child_lines: Vec<String> = {
            let mut lines = Vec::new();
            for child in &self.children {
                lines.extend(child.render(content_width));
            }
            lines
        };
        let mut result: Vec<String> = Vec::new();
        let empty_line = " ".repeat(width);
        for _ in 0..self.padding_y {
            result.push(match &self.bg_fn {
                Some(bg_fn) => apply_background_to_line(&empty_line, width, bg_fn.as_ref()),
                None => empty_line.clone(),
            });
        }
        let left_margin = " ".repeat(self.padding_x);
        let right_margin = " ".repeat(self.padding_x);
        for line in child_lines {
            let padded = format!("{left_margin}{line}{right_margin}");
            match &self.bg_fn {
                Some(bg_fn) => result.push(apply_background_to_line(&padded, width, bg_fn.as_ref())),
                None => {
                    let visible_len = visible_width(&padded);
                    let padding = ((width as f64) - visible_len).max(0.0) as usize;
                    result.push(format!("{padded}{}", " ".repeat(padding)));
                }
            }
        }
        for _ in 0..self.padding_y {
            result.push(match &self.bg_fn {
                Some(bg_fn) => apply_background_to_line(&empty_line, width, bg_fn.as_ref()),
                None => empty_line.clone(),
            });
        }
        result
    }
}

/// Spacer component that renders empty lines.
pub struct Spacer {
    lines: usize,
}

impl Spacer {
    pub fn new(lines: usize) -> Self {
        Self { lines }
    }

    pub fn set_lines(&mut self, lines: usize) {
        self.lines = lines;
    }
}

impl Component for Spacer {
    fn render(&self, _width: usize) -> Vec<String> {
        vec![String::new(); self.lines]
    }
}

/// Horizontal stack: lays children out side by side.
pub struct HStack {
    pub entries: Vec<StackLayoutEntry>,
    pub gap: f64,
    pub align: LayoutAlign,
}

impl HStack {
    pub fn new(entries: Vec<StackLayoutEntry>, gap: f64, align: LayoutAlign) -> Self {
        Self { entries, gap, align }
    }
}

impl Component for HStack {
    fn render(&self, width: usize) -> Vec<String> {
        let safe_width = width.max(1) as f64;
        let viewport = LayoutViewport {
            width: safe_width,
            height: f64::MAX,
        };
        let entries = visible_stack_entries(&self.entries, &viewport);
        if entries.is_empty() {
            return Vec::new();
        }
        let intrinsic_widths: Vec<f64> = entries
            .iter()
            .map(|entry| {
                entry
                    .component
                    .render(safe_width as usize)
                    .iter()
                    .map(|line| visible_width(line))
                    .fold(0.0, f64::max)
            })
            .collect();
        let widths = allocate_stack_sizes(&entries, &intrinsic_widths, Some(safe_width), self.gap);
        let rendered: Vec<Vec<String>> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                if widths[index] == 0.0 {
                    Vec::new()
                } else {
                    entry.component.render(widths[index] as usize)
                }
            })
            .collect();
        let height = rendered.iter().map(|lines| lines.len()).fold(0usize, usize::max);
        let mut result = vec![String::new(); height];
        let mut x = 0.0;
        for (index, lines) in rendered.iter().enumerate() {
            let child_width = widths[index];
            let mut offset = 0;
            if self.align == LayoutAlign::Center {
                offset = (height as isize - lines.len() as isize).max(0) / 2;
            } else if self.align == LayoutAlign::End {
                offset = (height as isize - lines.len() as isize).max(0);
            }
            for (row, line) in lines.iter().enumerate() {
                let target = row as isize + offset;
                if target < 0 || target >= result.len() as isize {
                    continue;
                }
                result[target as usize] =
                    composite_tui_line(&result[target as usize], line, x, child_width, safe_width);
            }
            x += child_width + self.gap;
        }
        result
    }
}

/// Vertical stack: lays children out top to bottom.
pub struct VStack {
    pub entries: Vec<StackLayoutEntry>,
    pub gap: f64,
}

impl VStack {
    pub fn new(entries: Vec<StackLayoutEntry>, gap: f64) -> Self {
        Self { entries, gap }
    }
}

impl Component for VStack {
    fn render(&self, width: usize) -> Vec<String> {
        let viewport = LayoutViewport {
            width: width.max(1) as f64,
            height: f64::MAX,
        };
        let entries = visible_stack_entries(&self.entries, &viewport);
        let rendered: Vec<Vec<String>> = entries
            .iter()
            .map(|entry| entry.component.render(width.max(1)))
            .collect();
        let sizes = allocate_stack_sizes(
            &entries,
            &rendered.iter().map(|lines| lines.len() as f64).collect::<Vec<_>>(),
            None,
            self.gap,
        );
        let mut lines: Vec<String> = Vec::new();
        for (index, child_lines) in rendered.iter().enumerate() {
            if index > 0 {
                for _ in 0..self.gap as usize {
                    lines.push(String::new());
                }
            }
            lines.extend(child_lines.iter().cloned());
        }
        let _ = sizes;
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_visible(line: &str) -> String {
        line.replace("[0m]8;;", "")
    }

    struct LineText {
        text: String,
    }

    impl Component for LineText {
        fn render(&self, _width: usize) -> Vec<String> {
            vec![self.text.clone()]
        }
    }

    #[test]
    fn text_renders_with_padding() {
        let text = Text::new("hello", 1, 0, None);
        let lines = text.render(9);
        assert_eq!(lines[0], " hello   ");
    }

    #[test]
    fn text_wraps_long_content() {
        let text = Text::new("hello world", 0, 0, None);
        let lines = text.render(5);
        assert!(lines.len() > 1);
        assert_eq!(visible_width(&lines[0]), 5.0);
    }

    #[test]
    fn text_empty_renders_nothing() {
        let text = Text::new("  ", 1, 1, None);
        assert!(text.render(10).is_empty());
    }

    #[test]
    fn spacer_renders_empty_lines() {
        let spacer = Spacer::new(3);
        assert_eq!(spacer.render(10), vec![String::new(); 3]);
    }

    #[test]
    fn box_pads_children() {
        let mut bx = Box::new(1, 1, None);
        bx.add_child(Arc::new(LineText {
            text: "x".to_string(),
        }));
        let lines = bx.render(5);
        assert_eq!(lines.len(), 3); // padding top + content + padding bottom
        assert_eq!(lines[1], " x   ");
    }

    #[test]
    fn vstack_stacks_children() {
        let stack = VStack::new(
            vec![
                StackLayoutEntry::new(Arc::new(LineText {
                    text: "a".to_string(),
                })),
                StackLayoutEntry::new(Arc::new(LineText {
                    text: "b".to_string(),
                })),
            ],
            1.0,
        );
        let lines = stack.render(10);
        assert_eq!(lines, vec!["a", "", "b"]);
    }

    #[test]
    fn hstack_side_by_side() {
        let stack = HStack::new(
            vec![
                StackLayoutEntry::new(Arc::new(LineText {
                    text: "ab".to_string(),
                })),
                StackLayoutEntry::new(Arc::new(LineText {
                    text: "cd".to_string(),
                })),
            ],
            0.0,
            LayoutAlign::Stretch,
        );
        let lines = stack.render(10);
        // compositeTuiLine embeds SGR reset markers (JS behavior); check
        // visible content.
        assert!(visible_width(&lines[0]) >= 4.0);
        assert!(strip_visible(&lines[0]).contains("abcd"));
    }
}
