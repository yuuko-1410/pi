//! Shared truncation utilities for tool outputs, port of
//! `core/tools/truncate.ts`. Two independent limits, whichever hits first:
//! lines (2000) and bytes (50KB). Never returns partial lines (except the
//! bash tail edge case).

pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
pub const GREP_MAX_LINE_LENGTH: usize = 500;

#[derive(Clone, Debug, PartialEq)]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    pub truncated_by: Option<&'static str>, // "lines" | "bytes" | null
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct TruncationOptions {
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
}

impl Default for TruncationOptions {
    fn default() -> Self {
        Self {
            max_lines: None,
            max_bytes: None,
        }
    }
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

/// Format bytes as a human-readable size (JS toFixed(1) semantics).
pub fn format_size(bytes: f64) -> String {
    if bytes < 1024.0 {
        format!("{}B", bytes as i64)
    } else if bytes < 1024.0 * 1024.0 {
        format!("{}KB", to_fixed_1(bytes / 1024.0))
    } else {
        format!("{}MB", to_fixed_1(bytes / (1024.0 * 1024.0)))
    }
}

fn to_fixed_1(value: f64) -> String {
    format!("{:.1}", (value * 10.0).round() / 10.0)
}

/// Truncate from the head (keep first N lines/bytes). Never returns partial
/// lines; an over-limit first line yields empty content with
/// first_line_exceeds_limit.
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
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
        };
    }

    let first_line_bytes = lines.first().map(|line| line.len()).unwrap_or(0);
    if first_line_bytes > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some("bytes"),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    let mut output_lines_arr: Vec<&str> = Vec::new();
    let mut output_bytes_count = 0;
    let mut truncated_by: &'static str = "lines";

    for (i, line) in lines.iter().take(max_lines).enumerate() {
        // +1 for the newline of non-first lines.
        let line_bytes = line.len() + if i > 0 { 1 } else { 0 };
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = "bytes";
            break;
        }
        output_lines_arr.push(line);
        output_bytes_count += line_bytes;
    }

    if output_lines_arr.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = "lines";
    }

    let output_content = output_lines_arr.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines_arr.len(),
        output_bytes: final_output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Truncate from the tail (keep last N lines/bytes). May return a partial
/// first line when the last line of the original exceeds the byte limit.
pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
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
        };
    }

    let mut output_lines_arr: Vec<String> = Vec::new();
    let mut output_bytes_count = 0;
    let mut truncated_by: &'static str = "lines";
    let mut last_line_partial = false;

    let mut index = lines.len();
    while index > 0 && output_lines_arr.len() < max_lines {
        index -= 1;
        let line = lines[index];
        // +1 for the newline of already-added lines.
        let line_bytes = line.len() + if !output_lines_arr.is_empty() { 1 } else { 0 };

        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = "bytes";
            if output_lines_arr.is_empty() {
                let truncated_line = truncate_string_to_bytes_from_end(line, max_bytes);
                output_bytes_count = truncated_line.len();
                last_line_partial = true;
                output_lines_arr.push(truncated_line);
            }
            break;
        }

        output_lines_arr.push(line.to_string());
        output_bytes_count += line_bytes;
    }

    if output_lines_arr.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = "lines";
    }

    output_lines_arr.reverse();
    let output_content = output_lines_arr.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines_arr.len(),
        output_bytes: final_output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Keep the last `max_bytes` bytes of a string on a UTF-8 boundary.
fn truncate_string_to_bytes_from_end(input: &str, max_bytes: usize) -> String {
    let bytes = input.as_bytes();
    if bytes.len() <= max_bytes {
        return input.to_string();
    }
    let mut start = bytes.len() - max_bytes;
    // Find a valid UTF-8 boundary (start of a character).
    while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).to_string()
}

/// Truncate a single line to max characters, adding a [truncated] suffix.
pub fn truncate_line(line: &str, max_chars: Option<usize>) -> (String, bool) {
    let max_chars = max_chars.unwrap_or(GREP_MAX_LINE_LENGTH);
    if line.chars().count() <= max_chars {
        return (line.to_string(), false);
    }
    let text: String = line.chars().take(max_chars).collect();
    (format!("{text}... [truncated]"), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_untouched_when_within_limits() {
        let result = truncate_head("a\nb", TruncationOptions::default());
        assert!(!result.truncated);
        assert_eq!(result.content, "a\nb");
        assert_eq!(result.truncated_by, None);
    }

    #[test]
    fn head_line_limit() {
        let content = (0..10).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let result = truncate_head(&content, TruncationOptions { max_lines: Some(3), max_bytes: None });
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some("lines"));
        assert_eq!(result.content, "line0\nline1\nline2");
        assert_eq!(result.total_lines, 10);
        assert_eq!(result.output_lines, 3);
    }

    #[test]
    fn head_byte_limit() {
        let content = "aaaaaa\nbbbbbb";
        let result = truncate_head(&content, TruncationOptions { max_lines: None, max_bytes: Some(8) });
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some("bytes"));
        // "aaaaaa" (6) + newline (1) = 7 fits; "bbbbbb" would exceed.
        assert_eq!(result.content, "aaaaaa");
    }

    #[test]
    fn head_first_line_exceeds() {
        let content = "aaaaaa\nbb";
        let result = truncate_head(&content, TruncationOptions { max_lines: None, max_bytes: Some(4) });
        assert!(result.truncated);
        assert_eq!(result.content, "");
        assert!(result.first_line_exceeds_limit);
    }

    #[test]
    fn tail_keeps_end() {
        let content = (0..10).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let result = truncate_tail(&content, TruncationOptions { max_lines: Some(3), max_bytes: None });
        assert!(result.truncated);
        assert_eq!(result.content, "line7\nline8\nline9");
    }

    #[test]
    fn tail_partial_last_line() {
        let content = "aaaaaa\nbbbbbb";
        let result = truncate_tail(&content, TruncationOptions { max_lines: None, max_bytes: Some(4) });
        assert!(result.truncated);
        assert!(result.last_line_partial);
        assert_eq!(result.content, "bbbb");
    }

    #[test]
    fn tail_utf8_boundary() {
        let content = "aaaa\nééééé"; // é is 2 bytes
        let result = truncate_tail(&content, TruncationOptions { max_lines: None, max_bytes: Some(5) });
        assert!(result.last_line_partial);
        // 5 bytes from the end: "éé" + 1 byte of é -> boundary adjusted to 4 bytes.
        assert_eq!(result.content.len(), 4);
        assert_eq!(result.content, "éé");
    }

    #[test]
    fn format_size_matches_js() {
        assert_eq!(format_size(512.0), "512B");
        assert_eq!(format_size(2048.0), "2.0KB");
        assert_eq!(format_size(3.5 * 1024.0 * 1024.0), "3.5MB");
    }

    #[test]
    fn truncate_line_adds_suffix() {
        let (text, truncated) = truncate_line("abcdef", Some(3));
        assert!(truncated);
        assert_eq!(text, "abc... [truncated]");
        let (text, truncated) = truncate_line("abc", Some(3));
        assert!(!truncated);
        assert_eq!(text, "abc");
    }
}
