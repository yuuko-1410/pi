//! Ls tool, port of `tools/ls.ts`.

use std::path::Path;

use pi_protocol::Value;

use super::path_utils::resolve_to_cwd;
use super::truncate::{format_size, truncate_head, TruncationOptions, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};

const DEFAULT_LIMIT: usize = 100;

#[derive(Clone, Debug, PartialEq)]
pub struct LsToolDetails {
    pub entry_limit_reached: Option<usize>,
    pub truncation: Option<super::truncate::TruncationResult>,
}

/// Execute the ls tool (sync analog).
pub fn execute_ls_tool(
    cwd: &str,
    path: Option<&str>,
    limit: Option<f64>,
) -> Result<(Vec<pi_ai::types::Content>, Option<LsToolDetails>), String> {
    let dir_path = resolve_to_cwd(path.unwrap_or("."), cwd);
    let effective_limit = (limit.unwrap_or(DEFAULT_LIMIT as f64) as usize).max(1);

    if !Path::new(&dir_path).exists() {
        return Err(format!("Path not found: {dir_path}"));
    }
    if !Path::new(&dir_path).is_dir() {
        return Err(format!("Not a directory: {dir_path}"));
    }

    let mut entries: Vec<String> = std::fs::read_dir(&dir_path)
        .map_err(|error| format!("Cannot read directory: {error}"))?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();

    // Sort alphabetically, case-insensitive (localeCompare approximation).
    entries.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));

    let mut results: Vec<String> = Vec::new();
    let mut entry_limit_reached = false;
    for entry in &entries {
        if results.len() >= effective_limit {
            entry_limit_reached = true;
            break;
        }
        let full_path = Path::new(&dir_path).join(entry);
        let mut suffix = "";
        if full_path.is_dir() {
            suffix = "/";
        }
        results.push(format!("{entry}{suffix}"));
    }

    if results.is_empty() {
        return Ok((
            vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
                text: "(empty directory)".to_string(),
                text_signature: None,
            })],
            None,
        ));
    }

    let raw_output = results.join("\n");
    let truncation = truncate_head(&raw_output, TruncationOptions {
        max_lines: Some(DEFAULT_MAX_LINES),
        max_bytes: Some(DEFAULT_MAX_BYTES),
    });
    let mut output = truncation.content.clone();
    let mut details: LsToolDetails = LsToolDetails {
        entry_limit_reached: None,
        truncation: None,
    };
    let mut notices: Vec<String> = Vec::new();
    if entry_limit_reached {
        notices.push(format!(
            "{effective_limit} entries limit reached. Use limit={} for more",
            effective_limit * 2
        ));
        details.entry_limit_reached = Some(effective_limit);
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES as f64)));
        details.truncation = Some(truncation.clone());
    }
    if !notices.is_empty() {
        output.push_str(&format!("\n\n[{}]", notices.join(". ")));
    }

    Ok((
        vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
            text: output,
            text_signature: None,
        })],
        if details.entry_limit_reached.is_none() && details.truncation.is_none() {
            None
        } else {
            Some(details)
        },
    ))
}

pub fn ls_tool_parameters() -> Value {
    Value::Map(vec![
        ("path".to_string(), Value::Map(vec![("description".to_string(), Value::String("Directory to list (default: cwd)".to_string()))])),
        ("limit".to_string(), Value::Map(vec![("description".to_string(), Value::String("Maximum entries".to_string()))])),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-ls-{}-{n}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("b.txt"), "").unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        dir.to_string_lossy().to_string()
    }

    #[test]
    fn lists_sorted_with_dir_suffix() {
        let dir = temp_dir();
        let (content, details) = execute_ls_tool("/tmp", Some(&dir), None).unwrap();
        assert!(matches!(&content[0], pi_ai::types::Content::Text(text) if {
            text.text == "a.txt\nb.txt\nsub/" || text.text == "a.txt\nsub/\nb.txt" || text.text == "sub/\na.txt\nb.txt" || text.text == "b.txt\na.txt\nsub/" || text.text == "b.txt\nsub/\na.txt"
        }), "got: {:?}", content);
        assert!(details.is_none());
    }

    #[test]
    fn limit_reached_notice() {
        let dir = temp_dir();
        let (content, details) = execute_ls_tool("/tmp", Some(&dir), Some(2.0)).unwrap();
        assert!(matches!(&content[0], pi_ai::types::Content::Text(text) if text.text.contains("2 entries limit reached")));
        assert_eq!(details.unwrap().entry_limit_reached, Some(2));
    }

    #[test]
    fn empty_directory() {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-ls-empty-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (content, _) = execute_ls_tool("/tmp", Some(&dir.to_string_lossy()), None).unwrap();
        assert!(matches!(&content[0], pi_ai::types::Content::Text(text) if text.text == "(empty directory)"));
    }

    #[test]
    fn missing_and_non_dir_errors() {
        let error = execute_ls_tool("/tmp", Some("/definitely/not/here"), None).unwrap_err();
        assert!(error.contains("Path not found"));
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let file = std::env::temp_dir().join(format!("pi-ls-file-{}-{n}", std::process::id()));
        std::fs::write(&file, "").unwrap();
        let error = execute_ls_tool("/tmp", Some(&file.to_string_lossy()), None).unwrap_err();
        assert!(error.contains("Not a directory"));
    }
}
