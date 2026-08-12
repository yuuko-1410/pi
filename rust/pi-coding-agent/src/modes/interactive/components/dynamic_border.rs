//! Dynamic border component, port of `components/dynamic-border.ts`.

use pi_tui::tui::Component;

use crate::modes::interactive::theme::theme::theme;

/// Horizontal border line spanning the viewport width.
pub struct DynamicBorder {
    color_fn: Box<dyn Fn(&str) -> String + Send + Sync>,
}

impl DynamicBorder {
    pub fn new(color_fn: Option<Box<dyn Fn(&str) -> String + Send + Sync>>) -> Self {
        let color_fn = color_fn.unwrap_or_else(|| {
            Box::new(|str: &str| {
                theme()
                    .as_ref()
                    .map(|t| t.fg("border", str))
                    .unwrap_or_else(|| str.to_string())
            })
        });
        Self { color_fn }
    }
}

impl Component for DynamicBorder {
    fn render(&self, width: usize) -> Vec<String> {
        let line = "─".repeat(width.max(1));
        vec![(self.color_fn)(&line)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_one_line_at_width() {
        let border = DynamicBorder::new(None);
        let lines = border.render(10);
        assert_eq!(lines.len(), 1);
        assert_eq!(pi_tui::utils::visible_width(&lines[0]) as usize, 10);
    }

    #[test]
    fn zero_width_clamped() {
        let border = DynamicBorder::new(None);
        let lines = border.render(0);
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].is_empty());
    }
}
