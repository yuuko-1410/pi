//! Read tool, port of `tools/read.ts`. Image processing (processImage) is
//! deferred; image files report a note instead of embedding the image.

use std::fs;

use pi_protocol::Value;

use super::path_utils::resolve_read_path;
use super::truncate::{format_size, truncate_head, TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};

pub const READ_TOOL_SYSTEM_PROMPT_CONTRIBUTION_SNIPPET: &str = "Read file contents";
pub const READ_TOOL_SYSTEM_PROMPT_GUIDELINES: [&str; 1] = ["Use read to examine files instead of cat or sed."];

/// Pluggable operations for the read tool.
pub trait ReadOperations {
    fn read_file(&self, path: &str) -> Result<Vec<u8>, String>;
    fn access(&self, path: &str) -> Result<(), String>;
}

pub struct LocalReadOperations;

impl ReadOperations for LocalReadOperations {
    fn read_file(&self, path: &str) -> Result<Vec<u8>, String> {
        fs::read(path).map_err(|error| error.to_string())
    }
    fn access(&self, path: &str) -> Result<(), String> {
        fs::metadata(path).map(|_| ()).map_err(|error| error.to_string())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReadToolDetails {
    pub truncation: Option<TruncationResult>,
}

/// Execute the read tool (sync analog).
pub fn execute_read(
    cwd: &str,
    path: &str,
    offset: Option<f64>,
    limit: Option<f64>,
    operations: &dyn ReadOperations,
) -> Result<(Vec<pi_ai::types::Content>, Option<ReadToolDetails>), String> {
    let absolute_path = resolve_read_path(path, cwd);
    operations.access(&absolute_path).map_err(|error| format!("Could not read file: {path}. {error}."))?;

    // Image files: detect by extension; images are reported as a note.
    let mime_type = detect_image_mime_type(&absolute_path);
    if let Some(mime_type) = mime_type {
        let text_note = format!("Read image file [{mime_type}]\nImage content is not embedded in this runtime.");
        return Ok((
            vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
                text: text_note,
                text_signature: None,
            })],
            None,
        ));
    }

    let buffer = operations.read_file(&absolute_path)?;
    let text_content = String::from_utf8_lossy(&buffer).to_string();
    let all_lines: Vec<&str> = text_content.split('\n').collect();
    let total_file_lines = all_lines.len();

    let start_line = match offset {
        Some(offset) => ((offset as i64) - 1).max(0) as usize,
        None => 0,
    };
    let start_line_display = start_line + 1;

    if start_line >= total_file_lines {
        return Err(format!(
            "Offset {} is beyond end of file ({total_file_lines} lines total)",
            offset.unwrap_or(0.0) as i64
        ));
    }

    let selected_content: String;
    let mut user_limited_lines: Option<usize> = None;
    if let Some(limit) = limit {
        let end_line = ((start_line as f64 + limit) as usize).min(total_file_lines);
        selected_content = all_lines[start_line..end_line].join("\n");
        user_limited_lines = Some(end_line - start_line);
    } else {
        selected_content = all_lines[start_line..].join("\n");
    }

    // Apply truncation (line + byte limits).
    let truncation = truncate_head(&selected_content, TruncationOptions {
        max_lines: Some(DEFAULT_MAX_LINES),
        max_bytes: Some(DEFAULT_MAX_BYTES),
    });

    let mut output_text: String;
    let details: Option<ReadToolDetails>;
    if truncation.first_line_exceeds_limit {
        let first_line_size = format_size(all_lines[start_line].len() as f64);
        output_text = format!(
            "[Line {start_line_display} is {first_line_size}, exceeds {} limit. Use bash: sed -n '{start_line_display}p' {path} | head -c {DEFAULT_MAX_BYTES}]",
            format_size(DEFAULT_MAX_BYTES as f64)
        );
        details = Some(ReadToolDetails {
            truncation: Some(truncation),
        });
    } else if truncation.truncated {
        let end_line_display = start_line_display + truncation.output_lines.saturating_sub(1);
        let next_offset = end_line_display + 1;
        output_text = truncation.content.clone();
        let notice = if truncation.truncated_by == Some("lines") {
            format!(
                "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines}. Use offset={next_offset} to continue.]"
            )
        } else {
            format!(
                "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines} ({} limit). Use offset={next_offset} to continue.]",
                format_size(DEFAULT_MAX_BYTES as f64)
            )
        };
        output_text.push_str(&notice);
        details = Some(ReadToolDetails {
            truncation: Some(truncation),
        });
    } else if let Some(user_limited_lines) = user_limited_lines {
        if start_line + user_limited_lines < total_file_lines {
            let remaining = total_file_lines - (start_line + user_limited_lines);
            let next_offset = start_line + user_limited_lines + 1;
            output_text = format!(
                "{}\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]",
                truncation.content
            );
        } else {
            output_text = truncation.content.clone();
        }
        details = None;
    } else {
        output_text = truncation.content.clone();
        details = None;
    }

    Ok((
        vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
            text: output_text,
            text_signature: None,
        })],
        details,
    ))
}

fn detect_image_mime_type(path: &str) -> Option<String> {
    let ext = path.rsplit('.').next()?.to_lowercase();
    match ext.as_str() {
        "png" => Some("image/png".to_string()),
        "jpg" | "jpeg" => Some("image/jpeg".to_string()),
        "gif" => Some("image/gif".to_string()),
        "webp" => Some("image/webp".to_string()),
        _ => None,
    }
}

pub fn read_tool_parameters() -> Value {
    Value::Map(vec![
        (
            "path".to_string(),
            Value::Map(vec![(
                "description".to_string(),
                Value::String("Path to the file to read (relative or absolute)".to_string()),
            )]),
        ),
        (
            "offset".to_string(),
            Value::Map(vec![(
                "description".to_string(),
                Value::String("Line number to start reading from (1-indexed)".to_string()),
            )]),
        ),
        (
            "limit".to_string(),
            Value::Map(vec![(
                "description".to_string(),
                Value::String("Maximum number of lines to read".to_string()),
            )]),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(content: &str) -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("pi-read-{}-{n}.txt", std::process::id()));
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().to_string()
    }

    #[test]
    fn reads_full_file() {
        let path = temp_file("a\nb\nc\n");
        let (content, details) = execute_read("/tmp", &path, None, None, &LocalReadOperations).unwrap();
        assert_eq!(content.len(), 1);
        if let pi_ai::types::Content::Text(text) = &content[0] {
            // JS: split("\n") keeps the trailing empty element; join keeps
            // the trailing newline.
            assert_eq!(text.text, "a\nb\nc\n");
        } else {
            panic!("not text");
        }
        assert!(details.is_none());
    }

    #[test]
    fn offset_and_limit() {
        let path = temp_file("l1\nl2\nl3\nl4\nl5\n");
        let (content, _) = execute_read("/tmp", &path, Some(2.0), Some(2.0), &LocalReadOperations).unwrap();
        if let pi_ai::types::Content::Text(text) = &content[0] {
            // JS: the user limit leaves 3 more elements (incl. trailing "").
            assert_eq!(text.text, "l2\nl3\n\n[3 more lines in file. Use offset=4 to continue.]");
        } else {
            panic!("not text");
        }
    }

    #[test]
    fn user_limit_continuation_notice() {
        let path = temp_file("l1\nl2\nl3\nl4\nl5\n");
        let (content, _) = execute_read("/tmp", &path, None, Some(2.0), &LocalReadOperations).unwrap();
        if let pi_ai::types::Content::Text(text) = &content[0] {
            // 6 split elements, 2 shown -> 4 more; next offset 3.
            assert!(text.text.contains("4 more lines in file"), "got: {:?}", text.text);
            assert!(text.text.contains("offset=3"), "got: {:?}", text.text);
        } else {
            panic!("not text");
        }
    }

    #[test]
    fn offset_beyond_end_errors() {
        let path = temp_file("a\nb\n");
        let error = execute_read("/tmp", &path, Some(10.0), None, &LocalReadOperations).unwrap_err();
        assert!(error.contains("beyond end of file"));
    }

    #[test]
    fn missing_file_errors() {
        let error = execute_read("/tmp", "/definitely/not/here.txt", None, None, &LocalReadOperations).unwrap_err();
        assert!(error.contains("Could not read file"));
    }

    #[test]
    fn image_files_reported_as_note() {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("pi-read-{}-{n}.png", std::process::id()));
        std::fs::write(&path, b"not really a png").unwrap();
        let (content, _) = execute_read("/tmp", &path.to_string_lossy(), None, None, &LocalReadOperations).unwrap();
        assert!(matches!(&content[0], pi_ai::types::Content::Text(text) if text.text.contains("image/png")));
    }
}
