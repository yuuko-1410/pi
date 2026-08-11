//! Shared truncation utilities for tool outputs, port of
//! `packages/agent/src/harness/utils/truncate.ts`.
//!
//! Rust `str` is UTF-8, so the JS UTF-16→UTF-8 byte counting collapses to
//! `str.len()`; unpaired surrogate replacement is a no-op (unrepresentable).

pub const DEFAULT_MAX_LINES: f64 = 2000.0;
pub const DEFAULT_MAX_BYTES: f64 = 50.0 * 1024.0; // 50KB
pub const GREP_MAX_LINE_LENGTH: usize = 500; // Max chars per grep match line

#[derive(Clone, Debug, PartialEq)]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    /// Which limit was hit: "lines", "bytes", or null if not truncated.
    pub truncated_by: Option<String>,
    pub total_lines: f64,
    pub total_bytes: f64,
    pub output_lines: f64,
    pub output_bytes: f64,
    /// Whether the last line was partially truncated (tail edge case).
    pub last_line_partial: bool,
    /// Whether the first line exceeded the byte limit (head).
    pub first_line_exceeds_limit: bool,
    pub max_lines: f64,
    pub max_bytes: f64,
}

impl TruncationResult {
    fn untruncated(content: String, max_lines: f64, max_bytes: f64) -> Self {
        let total_bytes = content.len() as f64;
        let total_lines = split_lines_for_counting(&content).len() as f64;
        Self {
            content,
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TruncationOptions {
    pub max_lines: Option<f64>,
    pub max_bytes: Option<f64>,
}

fn utf8_byte_length(content: &str) -> f64 {
    content.len() as f64
}

fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Format bytes as human-readable size.
pub fn format_size(bytes: f64) -> String {
    if bytes < 1024.0 {
        format!("{bytes}B")
    } else if bytes < 1024.0 * 1024.0 {
        format!("{:.1}KB", bytes / 1024.0)
    } else {
        format!("{:.1}MB", bytes / (1024.0 * 1024.0))
    }
}

/// Truncate content from the head (keep first N lines/bytes). Never returns
/// partial lines; if the first line exceeds the byte limit, returns empty
/// content with first_line_exceeds_limit=true.
pub fn truncate_head(content: &str, options: &TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = utf8_byte_length(content);
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len() as f64;

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult::untruncated(content.to_string(), max_lines, max_bytes);
    }

    // Check if the first line alone exceeds the byte limit.
    let first_line_bytes = utf8_byte_length(lines.first().copied().unwrap_or(""));
    if first_line_bytes > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some("bytes".to_string()),
            total_lines,
            total_bytes,
            output_lines: 0.0,
            output_bytes: 0.0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    // Collect complete lines that fit.
    let mut output_lines_arr: Vec<&str> = Vec::new();
    let mut output_bytes_count = 0.0f64;
    let mut truncated_by = "lines";

    for (i, line) in lines.iter().enumerate() {
        if i as f64 >= max_lines {
            break;
        }
        let line_bytes = utf8_byte_length(line) + if i > 0 { 1.0 } else { 0.0 }; // +1 newline

        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = "bytes";
            break;
        }

        output_lines_arr.push(line);
        output_bytes_count += line_bytes;
    }

    // If we exited due to the line limit.
    if output_lines_arr.len() as f64 >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = "lines";
    }

    let output_content = output_lines_arr.join("\n");
    let final_output_bytes = utf8_byte_length(&output_content);

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by.to_string()),
        total_lines,
        total_bytes,
        output_lines: output_lines_arr.len() as f64,
        output_bytes: final_output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Truncate content from the tail (keep last N lines/bytes). May return a
/// partial first line if the last line of the original content exceeds the
/// byte limit.
pub fn truncate_tail(content: &str, options: &TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = utf8_byte_length(content);
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len() as f64;

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult::untruncated(content.to_string(), max_lines, max_bytes);
    }

    // Work backwards from the end.
    let mut output_lines_arr: Vec<String> = Vec::new();
    let mut output_bytes_count = 0.0f64;
    let mut truncated_by = "lines";
    let mut last_line_partial = false;

    for i in (0..lines.len()).rev() {
        if output_lines_arr.len() as f64 >= max_lines {
            break;
        }
        let line = lines[i];
        let line_bytes = utf8_byte_length(line) + if !output_lines_arr.is_empty() { 1.0 } else { 0.0 };

        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = "bytes";
            // Edge case: no lines added yet and this line exceeds maxBytes —
            // take the end of the line (partial).
            if output_lines_arr.is_empty() {
                let truncated_line = truncate_string_to_bytes_from_end(line, max_bytes as usize);
                output_bytes_count = truncated_line.len() as f64;
                output_lines_arr.push(truncated_line);
                last_line_partial = true;
            }
            break;
        }

        output_lines_arr.insert(0, line.to_string());
        output_bytes_count += line_bytes;
    }

    // If we exited due to the line limit.
    if output_lines_arr.len() as f64 >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = "lines";
    }

    let output_content = output_lines_arr.join("\n");
    let final_output_bytes = utf8_byte_length(&output_content);

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by.to_string()),
        total_lines,
        total_bytes,
        output_lines: output_lines_arr.len() as f64,
        output_bytes: final_output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Truncate a string to fit within a byte limit (from the end), handling
/// multi-byte UTF-8 correctly (never splitting a character).
fn truncate_string_to_bytes_from_end(str: &str, max_bytes: usize) -> String {
    if max_bytes == 0 {
        return String::new();
    }
    let mut bytes = 0usize;
    let mut start = str.len();
    for (byte_index, _) in str.char_indices().rev() {
        let char_bytes = str[byte_index..].chars().next().expect("char").len_utf8();
        if bytes + char_bytes > max_bytes {
            break;
        }
        bytes += char_bytes;
        start = byte_index;
    }
    str[start..].to_string()
}

/// Truncate a single line to max characters, adding a [truncated] suffix.
/// Used for grep match lines.
pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    if line.chars().count() <= max_chars {
        return (line.to_string(), false);
    }
    let truncated: String = line.chars().take(max_chars).collect();
    (format!("{truncated}... [truncated]"), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(format_size(512.0), "512B");
        assert_eq!(format_size(2048.0), "2.0KB");
        assert_eq!(format_size(5.0 * 1024.0 * 1024.0), "5.0MB");
    }

    #[test]
    fn head_truncation_by_lines() {
        let content = (0..10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let result = truncate_head(&content, &TruncationOptions {
            max_lines: Some(3.0),
            ..TruncationOptions::default()
        });
        assert!(result.truncated);
        assert_eq!(result.truncated_by.as_deref(), Some("lines"));
        assert_eq!(result.content, "line 0\nline 1\nline 2");
        assert_eq!(result.total_lines, 10.0);
        assert_eq!(result.output_lines, 3.0);
    }

    #[test]
    fn head_truncation_by_bytes_never_splits_lines() {
        let content = "abcdef\nghijkl\nmnopqr";
        let result = truncate_head(&content, &TruncationOptions {
            max_bytes: Some(10.0),
            ..TruncationOptions::default()
        });
        assert!(result.truncated);
        assert_eq!(result.truncated_by.as_deref(), Some("bytes"));
        assert_eq!(result.content, "abcdef");
    }

    #[test]
    fn head_first_line_exceeding_limit_returns_empty() {
        let content = "a very long first line that exceeds the byte limit entirely";
        let result = truncate_head(&content, &TruncationOptions {
            max_bytes: Some(10.0),
            ..TruncationOptions::default()
        });
        assert!(result.truncated);
        assert!(result.first_line_exceeds_limit);
        assert_eq!(result.content, "");
    }

    #[test]
    fn tail_truncation_keeps_the_end() {
        let content = (0..10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let result = truncate_tail(&content, &TruncationOptions {
            max_lines: Some(3.0),
            ..TruncationOptions::default()
        });
        assert!(result.truncated);
        assert_eq!(result.content, "line 7\nline 8\nline 9");
    }

    #[test]
    fn tail_truncation_by_bytes_keeps_whole_lines() {
        let content = "short\nline";
        // Backwards: "line" (4) fits; "short" + newline (6) does not (4+6>6).
        let result = truncate_tail(&content, &TruncationOptions {
            max_bytes: Some(6.0),
            ..TruncationOptions::default()
        });
        assert!(result.truncated);
        assert_eq!(result.truncated_by.as_deref(), Some("bytes"));
        assert!(!result.last_line_partial);
        assert_eq!(result.content, "line");

        // Edge case: the last line alone exceeds the limit — take its tail.
        let long = format!("short\n{}", "x".repeat(20));
        let result = truncate_tail(&long, &TruncationOptions {
            max_bytes: Some(10.0),
            ..TruncationOptions::default()
        });
        assert!(result.last_line_partial);
        assert!(result.content.ends_with("xxx"));
        assert!(result.content.len() <= 10);
    }

    #[test]
    fn truncates_lines_with_suffix() {
        let (text, was_truncated) = truncate_line(&"x".repeat(600), GREP_MAX_LINE_LENGTH);
        assert!(was_truncated);
        assert!(text.ends_with("... [truncated]"));
        let (text, was_truncated) = truncate_line("short", GREP_MAX_LINE_LENGTH);
        assert!(!was_truncated);
        assert_eq!(text, "short");
    }

    #[test]
    fn utf8_bytes_count_correctly() {
        // "héllo" = h(1) é(2) l l o = 6 bytes.
        assert_eq!(utf8_byte_length("héllo"), 6.0);
        // Emoji = 4 bytes.
        assert_eq!(utf8_byte_length("🙈"), 4.0);
        let result = truncate_head("🙈🙈🙈", &TruncationOptions {
            max_bytes: Some(6.0),
            ..TruncationOptions::default()
        });
        // The single line exceeds the byte limit: head returns empty with the
        // first-line-exceeds flag (never a partial line).
        assert!(result.truncated);
        assert!(result.first_line_exceeds_limit);
        assert_eq!(result.content, "");
    }
}
