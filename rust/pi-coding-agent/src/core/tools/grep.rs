//! Grep tool, port of `tools/grep.ts`. Runs ripgrep (rg --json) and formats
//! matches with context and truncation.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use pi_protocol::Value;

use super::path_utils::resolve_to_cwd;
use super::truncate::{truncate_line, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, GREP_MAX_LINE_LENGTH};

const DEFAULT_LIMIT: usize = 100;

pub const GREP_TOOL_SYSTEM_PROMPT_CONTRIBUTION_SNIPPET: &str = "Search file contents for a pattern";

/// Execute the grep tool via ripgrep (sync analog).
pub fn execute_grep_tool(
    cwd: &str,
    pattern: &str,
    search_dir: Option<&str>,
    glob: Option<&str>,
    ignore_case: bool,
    literal: bool,
    context: Option<f64>,
    limit: Option<f64>,
) -> Result<(Vec<pi_ai::types::Content>, Option<Value>), String> {
    let search_path = resolve_to_cwd(search_dir.unwrap_or("."), cwd);
    let is_directory = Path::new(&search_path).is_dir();
    if !Path::new(&search_path).exists() {
        return Err(format!("Path not found: {search_path}"));
    }

    let context_value = context.filter(|value| *value > 0.0).unwrap_or(0.0) as usize;
    let effective_limit = (limit.unwrap_or(DEFAULT_LIMIT as f64) as usize).max(1);

    let mut args: Vec<String> = vec![
        "--json".into(),
        "--line-number".into(),
        "--color=never".into(),
        "--hidden".into(),
    ];
    if ignore_case {
        args.push("--ignore-case".into());
    }
    if literal {
        args.push("--fixed-strings".into());
    }
    if let Some(glob) = glob {
        args.push("--glob".into());
        args.push(glob.to_string());
    }
    args.push("--".into());
    args.push(pattern.to_string());
    args.push(search_path.clone());

    let mut child = Command::new("rg")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("ripgrep (rg) is not available: {error}"))?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stderr_text = {
        let mut reader = BufReader::new(stderr);
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut reader, &mut text);
        text
    };

    let format_path = |file_path: &str| -> String {
        if is_directory {
            if let Some(relative) = Path::new(&search_path)
                .parent()
                .and_then(|_| Path::new(file_path).strip_prefix(&search_path).ok())
            {
                let relative = relative.to_string_lossy().replace('\\', "/");
                if !relative.starts_with("..") {
                    return relative;
                }
            }
        }
        Path::new(file_path)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default()
    };

    let file_cache: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let get_file_lines = |file_path: &str, cache: &mut std::collections::HashMap<String, Vec<String>>| -> Vec<String> {
        if let Some(lines) = cache.get(file_path) {
            return lines.clone();
        }
        let lines = match std::fs::read_to_string(file_path) {
            Ok(content) => content.replace("\r\n", "\n").replace('\r', "\n").split('\n').map(|line| line.to_string()).collect(),
            Err(_) => Vec::new(),
        };
        cache.insert(file_path.to_string(), lines.clone());
        lines
    };

    let mut matches: Vec<(String, usize)> = Vec::new(); // (file, line number)
    let mut match_count = 0usize;
    let mut match_limit_reached = false;
    let mut killed_due_to_limit = false;

    {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() || match_count >= effective_limit {
                continue;
            }
            let Ok(value) = pi_ai::utils::json::parse_json_with_repair::<Value>(&line) else {
                continue;
            };
            let Some(entries) = value.as_map() else { continue };
            let event_type = entries
                .iter()
                .find(|(k, _)| k == "type")
                .and_then(|(_, v)| v.as_str())
                .unwrap_or("");
            if event_type != "match" {
                continue;
            }
            match_count += 1;
            let data = entries.iter().find(|(k, _)| k == "data").and_then(|(_, v)| v.as_map());
            if let Some(data) = data {
                let file_path = data
                    .iter()
                    .find(|(k, _)| k == "path")
                    .and_then(|(_, v)| v.as_map())
                    .and_then(|path| path.iter().find(|(k, _)| k == "text"))
                    .and_then(|(_, v)| v.as_str())
                    .map(|value| value.to_string());
                let line_number = data
                    .iter()
                    .find(|(k, _)| k == "line_number")
                    .and_then(|(_, v)| v.as_number())
                    .map(|value| value as usize);
                if let (Some(file_path), Some(line_number)) = (file_path, line_number) {
                    matches.push((file_path, line_number));
                }
            }
            if match_count >= effective_limit {
                match_limit_reached = true;
                killed_due_to_limit = true;
                let _ = child.kill();
                break;
            }
        }
    }
    let status = child.wait();
    let status_code = status.as_ref().ok().and_then(|s| s.code());
    if status.is_err() && !killed_due_to_limit {
        return Err(format!("rg failed: {stderr_text}"));
    }
    if match_count == 0 && !stderr_text.is_empty() && status.as_ref().map(|s| !s.success()).unwrap_or(false) {
        // rg exits 1 on no matches; 2 on errors.
        if status_code == Some(2) {
            return Err(format!("rg error: {stderr_text}"));
        }
    }

    // Format matches with context blocks.
    let mut output_lines: Vec<String> = Vec::new();
    let mut lines_truncated = false;
    let mut cache = file_cache;
    for (file_path, line_number) in &matches {
        let relative_path = format_path(file_path);
        let lines = get_file_lines(file_path, &mut cache);
        if lines.is_empty() {
            output_lines.push(format!("{relative_path}:{line_number}: (unable to read file)"));
            continue;
        }
        let start = if context_value > 0 {
            line_number.saturating_sub(context_value)
        } else {
            *line_number
        };
        let end = if context_value > 0 {
            lines.len().min(line_number + context_value)
        } else {
            *line_number
        };
        for current in start..=end {
            let line_text = lines.get(current - 1).cloned().unwrap_or_default().replace('\r', "");
            let (truncated_text, was_truncated) = truncate_line(&line_text, Some(GREP_MAX_LINE_LENGTH));
            if was_truncated {
                lines_truncated = true;
            }
            if current == *line_number {
                output_lines.push(format!("{relative_path}:{current}: {truncated_text}"));
            } else {
                output_lines.push(format!("{relative_path}-{current}- {truncated_text}"));
            }
        }
    }

    // Apply truncation.
    let raw_output = output_lines.join("\n");
    let truncation = super::truncate::truncate_head(&raw_output, super::truncate::TruncationOptions {
        max_lines: Some(DEFAULT_MAX_LINES),
        max_bytes: Some(DEFAULT_MAX_BYTES),
    });
    let mut output = truncation.content.clone();
    let mut notices: Vec<String> = Vec::new();
    if match_limit_reached {
        notices.push(format!("{effective_limit} match limit reached"));
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", super::truncate::format_size(DEFAULT_MAX_BYTES as f64)));
    }
    if lines_truncated {
        notices.push(format!(
            "Some lines truncated to {GREP_MAX_LINE_LENGTH} chars. Use read tool to see full lines"
        ));
    }
    if !notices.is_empty() {
        output.push_str(&format!("\n\n[{}]", notices.join(". ")));
    }

    Ok((
        vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
            text: output,
            text_signature: None,
        })],
        None,
    ))
}

pub fn grep_tool_parameters() -> Value {
    Value::Map(vec![
        ("pattern".to_string(), Value::Map(vec![("description".to_string(), Value::String("Pattern to search for".to_string()))])),
        ("path".to_string(), Value::Map(vec![("description".to_string(), Value::String("Directory to search (default: cwd)".to_string()))])),
        ("glob".to_string(), Value::Map(vec![("description".to_string(), Value::String("File glob filter".to_string()))])),
        ("ignoreCase".to_string(), Value::Map(vec![("description".to_string(), Value::String("Case-insensitive search".to_string()))])),
        ("literal".to_string(), Value::Map(vec![("description".to_string(), Value::String("Fixed string search".to_string()))])),
        ("context".to_string(), Value::Map(vec![("description".to_string(), Value::String("Context lines around matches".to_string()))])),
        ("limit".to_string(), Value::Map(vec![("description".to_string(), Value::String("Maximum matches".to_string()))])),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir_with_file() -> (String, String) {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-grep-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello world\nfoo bar\nhello again\n").unwrap();
        (dir.to_string_lossy().to_string(), dir.join("a.txt").to_string_lossy().to_string())
    }

    fn rg_available() -> bool {
        Command::new("rg").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
    }

    #[test]
    fn finds_matches() {
        if !rg_available() {
            return;
        }
        let (dir, _) = temp_dir_with_file();
        let (content, _) = execute_grep_tool(&dir, "hello", None, None, false, false, None, None).unwrap();
        if let pi_ai::types::Content::Text(text) = &content[0] {
            assert!(text.text.contains("a.txt:1: hello world"), "got: {}", text.text);
            assert!(text.text.contains("a.txt:3: hello again"));
        } else {
            panic!("not text");
        }
    }

    #[test]
    fn case_insensitive() {
        if !rg_available() {
            return;
        }
        let (dir, _) = temp_dir_with_file();
        let (content, _) = execute_grep_tool(&dir, "HELLO", None, None, true, false, None, None).unwrap();
        assert!(matches!(&content[0], pi_ai::types::Content::Text(text) if text.text.contains("hello world")));
    }

    #[test]
    fn missing_path_errors() {
        let error = execute_grep_tool("/tmp", "x", Some("/definitely/not/here"), None, false, false, None, None).unwrap_err();
        assert!(error.contains("Path not found"));
    }

    #[test]
    fn no_matches_empty_output() {
        if !rg_available() {
            return;
        }
        let (dir, _) = temp_dir_with_file();
        let (content, _) = execute_grep_tool(&dir, "zzz_nothing", None, None, false, false, None, None).unwrap();
        assert!(matches!(&content[0], pi_ai::types::Content::Text(text) if text.text.is_empty()));
    }
}
