//! Find tool, port of `tools/find.ts`. Uses fd when available; falls back
//! to a std::fs recursive walk with gitignore-free filtering.
//! ponytail: the fallback walk ignores .gitignore rules (fd is required for
//! full parity); noted in the module header.

use std::path::Path;
use std::process::{Command, Stdio};

use pi_protocol::Value;

use super::path_utils::resolve_to_cwd;
use super::truncate::{truncate_head, TruncationOptions, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};

const DEFAULT_LIMIT: usize = 1000;

/// Execute the find tool (sync analog).
pub fn execute_find_tool(
    cwd: &str,
    pattern: &str,
    search_dir: Option<&str>,
    limit: Option<f64>,
) -> Result<(Vec<pi_ai::types::Content>, Option<Value>), String> {
    let search_path = resolve_to_cwd(search_dir.unwrap_or("."), cwd);
    if !Path::new(&search_path).exists() {
        return Err(format!("Path not found: {search_path}"));
    }
    let effective_limit = (limit.unwrap_or(DEFAULT_LIMIT as f64) as usize).max(1);

    let results: Vec<String> = if Command::new("fd").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok() {
        run_fd(&search_path, pattern, effective_limit)
    } else {
        // Fallback recursive walk.
        walk(&search_path, pattern, effective_limit)
    };

    let mut output = results.join("\n");
    let mut notices: Vec<String> = Vec::new();
    let mut details: Vec<(String, Value)> = Vec::new();
    if results.len() >= effective_limit {
        notices.push(format!("{effective_limit} results limit reached. Use limit={} for more", effective_limit * 2));
        details.push(("resultLimitReached".to_string(), Value::Number(effective_limit as f64)));
    }
    let truncation = truncate_head(&output, TruncationOptions {
        max_lines: Some(DEFAULT_MAX_LINES),
        max_bytes: Some(DEFAULT_MAX_BYTES),
    });
    output = truncation.content.clone();
    if truncation.truncated {
        notices.push(format!("{} limit reached", super::truncate::format_size(DEFAULT_MAX_BYTES as f64)));
    }
    if !notices.is_empty() {
        output.push_str(&format!("\n\n[{}]", notices.join(". ")));
    }
    let details = if details.is_empty() { None } else { Some(Value::Map(details)) };

    Ok((
        vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
            text: output,
            text_signature: None,
        })],
        details,
    ))
}

fn run_fd(search_path: &str, pattern: &str, effective_limit: usize) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--glob".into(),
        "--color=never".into(),
        "--hidden".into(),
        "--no-require-git".into(),
        "--max-results".into(),
        effective_limit.to_string(),
    ];
    let mut effective_pattern = pattern.to_string();
    if pattern.contains('/') {
        args.push("--full-path".into());
        if !pattern.starts_with('/') && !pattern.starts_with("**/") && pattern != "**" {
            effective_pattern = format!("**/{pattern}");
        }
    }
    args.push(effective_pattern);
    args.push(search_path.to_string());

    let output = Command::new("fd").args(&args).stdin(Stdio::null()).output();
    match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let relative_base = Path::new(search_path);
            text.lines()
                .map(|line| {
                    let path = Path::new(line);
                    path.strip_prefix(relative_base)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .replace('\\', "/")
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Recursive walk fallback matching the glob pattern against relative paths.
fn walk(dir: &str, pattern: &str, effective_limit: usize) -> Vec<String> {
    let mut results = Vec::new();
    let mut stack = vec![dir.to_string()];
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();

    while let Some(current) = stack.pop() {
        if results.len() >= effective_limit {
            break;
        }
        if !visited.insert(current.clone()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        let mut subdirs = Vec::new();
        for entry in entries.flatten() {
            if results.len() >= effective_limit {
                break;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "node_modules" || name == "target" || name == "vendor" {
                continue;
            }
            let relative = path
                .strip_prefix(dir)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let file_type = entry.file_type().ok();
            if file_type.map(|t| t.is_dir()).unwrap_or(false) {
                subdirs.push(path.to_string_lossy().to_string());
            } else if file_type.map(|t| t.is_file()).unwrap_or(false) {
                if glob_match(pattern, &relative) {
                    results.push(relative);
                }
            }
        }
        stack.extend(subdirs.into_iter().rev());
    }
    results
}

/// Simple glob matcher (*, **, ?) against path strings, anchored like fd
/// glob semantics: the pattern must match the full (relative) path.
fn glob_match(glob: &str, text: &str) -> bool {
    if glob.contains("**") {
        // Convert ** to a crossing-any matcher: split on ** and require each
        // segment to match in order.
        let segments: Vec<&str> = glob.split("**").collect();
        let mut index = 0usize;
        for segment in segments.iter() {
            if segment.is_empty() {
                continue;
            }
            let segment_match = find_glob(segment, &text[index..]);
            match segment_match {
                Some(end) => {
                    index += end;
                }
                None => return false,
            }
        }
        true
    } else {
        match_segment(glob, text).is_some()
    }
}

/// Find the first match of a single-segment glob; returns the end offset.
fn find_glob(glob: &str, text: &str) -> Option<usize> {
    // Try every start position (segment matching).
    for start in 0..=text.len() {
        if let Some(end) = match_segment(glob, &text[start..]) {
            return Some(start + end);
        }
    }
    None
}

fn match_segment(glob: &str, text: &str) -> Option<usize> {
    let glob_chars: Vec<char> = glob.chars().collect();
    let text_chars: Vec<char> = text.chars().collect();
    // DP: match glob[..i] against text[..j].
    let n = glob_chars.len();
    let m = text_chars.len();
    let mut dp = vec![vec![false; m + 1]; n + 1];
    dp[0][0] = true;
    for i in 1..=n {
        if glob_chars[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=n {
        for j in 1..=m {
            match glob_chars[i - 1] {
                '*' => dp[i][j] = dp[i - 1][j] || dp[i][j - 1],
                '?' => dp[i][j] = dp[i - 1][j - 1],
                c => dp[i][j] = dp[i - 1][j - 1] && c == text_chars[j - 1],
            }
        }
    }
    if dp[n][m] {
        Some(m)
    } else {
        None
    }
}

pub fn find_tool_parameters() -> Value {
    Value::Map(vec![
        ("pattern".to_string(), Value::Map(vec![("description".to_string(), Value::String("Glob pattern to match".to_string()))])),
        ("path".to_string(), Value::Map(vec![("description".to_string(), Value::String("Directory to search (default: cwd)".to_string()))])),
        ("limit".to_string(), Value::Map(vec![("description".to_string(), Value::String("Maximum results".to_string()))])),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_segment_matching() {
        assert!(match_segment("*.rs", "main.rs").is_some());
        assert!(match_segment("src/**/*.ts", "src/a/b.ts").is_some());
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "main.txt"));
        assert!(glob_match("**/*.rs", "a/b/main.rs"));
        assert!(glob_match("?.rs", "a.rs"));
        assert!(!glob_match("?.rs", "ab.rs"));
    }

    #[test]
    fn walk_finds_files() {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-find-{}-{n}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.rs"), "").unwrap();
        std::fs::write(dir.join("sub").join("b.rs"), "").unwrap();
        std::fs::write(dir.join("c.txt"), "").unwrap();

        let results = walk(&dir.to_string_lossy(), "*.rs", 100);
        assert_eq!(results.len(), 2);
        assert!(results.contains(&"a.rs".to_string()));
        assert!(results.contains(&"sub/b.rs".to_string()));
    }
}
