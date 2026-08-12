//! Shared utility for truncating text to visual lines, port of
//! `components/visual-truncate.ts`.

use pi_tui::components::basic::Text;
use pi_tui::tui::Component;

pub struct VisualTruncateResult {
    /// The visual lines to display.
    pub visual_lines: Vec<String>,
    /// Number of visual lines that were skipped (hidden).
    pub skipped_count: usize,
}

/// Truncate text to a maximum number of visual lines (from the end).
pub fn truncate_to_visual_lines(text: &str, max_visual_lines: usize, width: usize) -> VisualTruncateResult {
    if text.is_empty() {
        return VisualTruncateResult {
            visual_lines: Vec::new(),
            skipped_count: 0,
        };
    }

    let temp_text = Text::new(text, 0, 0, None);
    let all_visual_lines = temp_text.render(width);

    if all_visual_lines.len() <= max_visual_lines {
        return VisualTruncateResult {
            visual_lines: all_visual_lines,
            skipped_count: 0,
        };
    }

    let start = all_visual_lines.len() - max_visual_lines;
    let truncated_lines = all_visual_lines[start..].to_vec();
    let skipped_count = all_visual_lines.len() - max_visual_lines;

    VisualTruncateResult {
        visual_lines: truncated_lines,
        skipped_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_returns_nothing() {
        let result = truncate_to_visual_lines("", 10, 80);
        assert!(result.visual_lines.is_empty());
        assert_eq!(result.skipped_count, 0);
    }

    #[test]
    fn short_text_untouched() {
        let result = truncate_to_visual_lines("hello", 10, 80);
        assert_eq!(result.visual_lines.len(), 1);
        assert_eq!(result.skipped_count, 0);
    }

    #[test]
    fn long_text_takes_last_lines() {
        let text = "line1\nline2\nline3\nline4\nline5";
        let result = truncate_to_visual_lines(text, 2, 80);
        assert_eq!(result.visual_lines.len(), 2);
        assert_eq!(result.skipped_count, 3);
        assert!(result.visual_lines[1].contains("line5"));
    }

    #[test]
    fn wrapping_counts_visual_lines() {
        // A long single line wraps at narrow widths.
        let text = "a".repeat(40) + " b";
        let result = truncate_to_visual_lines(&text, 1, 20);
        assert_eq!(result.skipped_count, 2); // 3 wrapped lines -> keep 1
        assert_eq!(result.visual_lines.len(), 1);
    }
}
