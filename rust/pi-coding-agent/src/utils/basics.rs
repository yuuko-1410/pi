//! Small pure-logic utilities, ports of the corresponding
//! `packages/coding-agent/src/utils/*.ts` files.

/// Emit a deprecation warning once per message (JS `warnDeprecation`).
use std::sync::Mutex;

static EMITTED_DEPRECATION_WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn warn_deprecation(message: &str) {
    let mut emitted = EMITTED_DEPRECATION_WARNINGS.lock().unwrap();
    if emitted.iter().any(|existing| existing == message) {
        return;
    }
    emitted.push(message.to_string());
    // chalk.yellow -> ANSI yellow on stderr.
    eprintln!("\x1b[33mDeprecation warning: {message}\x1b[0m");
}

/// Clear deprecation warning state (exported for tests).
pub fn clear_deprecation_warnings_for_tests() {
    EMITTED_DEPRECATION_WARNINGS.lock().unwrap().clear();
}

// ---------------------------------------------------------------------------
// sleep.ts
// ---------------------------------------------------------------------------

/// Sleep for `ms` milliseconds.
pub fn sleep(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

// ---------------------------------------------------------------------------
// pi-user-agent.ts
// ---------------------------------------------------------------------------

/// Build the pi user agent string (JS `getPiUserAgent`).
pub fn get_pi_user_agent(version: &str) -> String {
    let runtime = std::env::var("BUN_VERSION")
        .map(|bun_version| format!("bun/{bun_version}"))
        .unwrap_or_else(|_| format!("node/{}", std::env::var("npm_config_user_agent").unwrap_or_default()));
    let platform = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    format!("pi/{version} ({platform}; {runtime}; {arch})")
}

// ---------------------------------------------------------------------------
// json.ts
// ---------------------------------------------------------------------------

/// Strip `//` line comments and trailing commas from JSON, leaving string
/// literals untouched (JS `stripJsonComments`).
pub fn strip_json_comments(input: &str) -> String {
    let mut result = String::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut pending_newline = false;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_string {
            result.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                result.push(ch);
            }
            '/' if chars.peek() == Some(&'/') => {
                // Consume until newline.
                for next in chars.by_ref() {
                    if next == '\n' {
                        pending_newline = true;
                        break;
                    }
                }
                if pending_newline {
                    result.push('\n');
                    pending_newline = false;
                }
            }
            ',' => {
                // Look ahead: if only whitespace then } or ], drop the comma.
                let mut ahead = chars.clone();
                let mut keep = true;
                while let Some(&next) = ahead.peek() {
                    if next.is_whitespace() {
                        ahead.next();
                    } else if next == '}' || next == ']' {
                        keep = false;
                        break;
                    } else {
                        break;
                    }
                }
                if keep {
                    result.push(',');
                }
            }
            other => result.push(other),
        }
    }
    result
}

// ---------------------------------------------------------------------------
// ansi.ts
// ---------------------------------------------------------------------------

/// Strip ANSI escape sequences (ported from ansi-regex + strip-ansi).
pub fn strip_ansi(value: &str) -> String {
    if !value.contains('\u{001B}') && !value.contains('\u{009B}') {
        return value.to_string();
    }
    let mut result = String::new();
    let chars: Vec<char> = value.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\u{001B}' && index + 1 < chars.len() && chars[index + 1] == ']' {
            // OSC sequence: ESC ] ... ST (ST = BEL, ESC\, or 0x9c).
            index += 2;
            while index < chars.len() {
                let current = chars[index];
                if current == '\u{0007}' || current == '\u{009C}' {
                    index += 1;
                    break;
                }
                if current == '\u{001B}' && index + 1 < chars.len() && chars[index + 1] == '\\' {
                    index += 2;
                    break;
                }
                index += 1;
            }
            continue;
        }
        if ch == '\u{001B}' || ch == '\u{009B}' {
            // CSI sequence: optional params and intermediates then final byte.
            index += 1;
            while index < chars.len() {
                let current = chars[index];
                if current == '[' || current == ']' || current == '(' || current == ')' || current == '#' || current == ';' || current == '?' {
                    index += 1;
                    continue;
                }
                if current.is_ascii_digit() || current == ';' || current == ':' {
                    index += 1;
                    continue;
                }
                // Final byte.
                index += 1;
                break;
            }
            continue;
        }
        result.push(ch);
        index += 1;
    }
    result
}

// ---------------------------------------------------------------------------
// frontmatter.ts
// ---------------------------------------------------------------------------

/// Extract YAML frontmatter (JS `parseFrontmatter`). YAML subset: scalar
/// key-value lines (see pi-agent-core skills parser for the subset).
pub fn parse_frontmatter(content: &str) -> (std::collections::HashMap<String, FrontmatterValue>, String) {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.starts_with("---") {
        return (std::collections::HashMap::new(), normalized);
    }
    let Some(end_index) = normalized.find("\n---") else {
        return (std::collections::HashMap::new(), normalized);
    };
    let yaml_string = &normalized[4..end_index];
    let body = normalized[end_index + 4..].trim().to_string();
    (parse_yaml_scalars(yaml_string), body)
}

#[derive(Clone, Debug, PartialEq)]
pub enum FrontmatterValue {
    String(String),
    Bool(bool),
    Number(f64),
    Null,
}

fn parse_yaml_scalars(yaml_string: &str) -> std::collections::HashMap<String, FrontmatterValue> {
    let mut frontmatter = std::collections::HashMap::new();
    for line in yaml_string.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(colon) = line.find(':') else {
            continue;
        };
        let key = line[..colon].trim().to_string();
        let raw = line[colon + 1..].trim();
        if raw.is_empty() {
            continue;
        }
        let value = if raw == "true" {
            FrontmatterValue::Bool(true)
        } else if raw == "false" {
            FrontmatterValue::Bool(false)
        } else if raw == "null" || raw == "~" {
            FrontmatterValue::Null
        } else if let Ok(number) = raw.parse::<f64>() {
            FrontmatterValue::Number(number)
        } else if (raw.starts_with('"') && raw.ends_with('"')) || (raw.starts_with('\'') && raw.ends_with('\'')) {
            FrontmatterValue::String(raw[1..raw.len() - 1].to_string())
        } else {
            FrontmatterValue::String(raw.split('#').next().unwrap_or("").trim().to_string())
        };
        frontmatter.insert(key, value);
    }
    frontmatter
}

/// Strip frontmatter, returning the body (JS `stripFrontmatter`).
pub fn strip_frontmatter(content: &str) -> String {
    parse_frontmatter(content).1
}

// ---------------------------------------------------------------------------
// shell.ts pure helpers
// ---------------------------------------------------------------------------

/// Sanitize binary output for display/storage (JS `sanitizeBinaryOutput`).
pub fn sanitize_binary_output(value: &str) -> String {
    value
        .chars()
        .filter(|ch| {
            let code = *ch as u32;
            if code == 0x09 || code == 0x0a || code == 0x0d {
                return true;
            }
            if code <= 0x1f {
                return false;
            }
            if (0xfff9..=0xfffb).contains(&code) {
                return false;
            }
            true
        })
        .collect()
}

/// Convert Git Bash/MSYS/Cygwin/WSL drive paths to native Windows form (JS
/// `normalizeWindowsShellPath`).
pub fn normalize_windows_shell_path(file_path: &str) -> String {
    if !file_path.starts_with('/') || file_path.starts_with("//") || file_path.contains('\\') {
        return file_path.to_string();
    }
    let after_slash = &file_path[1..];
    let rest = after_slash
        .strip_prefix("mnt/")
        .or_else(|| after_slash.strip_prefix("cygdrive/"))
        .unwrap_or(after_slash);
    let drive_char = rest.chars().next().unwrap_or('c');
    if !drive_char.is_ascii_alphabetic() {
        return file_path.to_string();
    }
    let after_drive = &rest[drive_char.len_utf8()..];
    let suffix = after_drive.strip_prefix('/').unwrap_or(after_drive);
    let suffix = suffix.replace('/', "\\");
    format!("{}:\\{suffix}", drive_char.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deprecation_warnings_emit_once() {
        clear_deprecation_warnings_for_tests();
        warn_deprecation("old thing");
        // Second call is a no-op; the set contains one entry.
        warn_deprecation("old thing");
        assert_eq!(EMITTED_DEPRECATION_WARNINGS.lock().unwrap().len(), 1);
    }

    #[test]
    fn strips_json_comments_and_trailing_commas() {
        let input = "{\n  // comment\n  \"a\": 1,\n  \"b\": \"x // not comment\",\n}";
        let result = strip_json_comments(input);
        assert!(!result.contains("// comment"));
        assert!(result.contains("\"a\": 1,"));
        assert!(result.contains("\"b\": \"x // not comment\""));
        assert!(!result.contains(",\n}"));
    }

    #[test]
    fn strips_ansi_codes() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("\x1b]0;title\x07plain"), "plain");
        assert_eq!(strip_ansi("no codes"), "no codes");
    }

    #[test]
    fn parses_frontmatter() {
        let (frontmatter, body) = parse_frontmatter("---\nname: test\ndescription: \"a: b\"\n---\n\n# Body");
        assert_eq!(frontmatter.get("name"), Some(&FrontmatterValue::String("test".to_string())));
        assert_eq!(
            frontmatter.get("description"),
            Some(&FrontmatterValue::String("a: b".to_string()))
        );
        assert!(body.starts_with("# Body"));
        assert_eq!(strip_frontmatter("plain"), "plain");
    }

    #[test]
    fn sanitizes_binary_output() {
        assert_eq!(sanitize_binary_output("a\u{0000}b\tc\n"), "ab\tc\n");
        assert_eq!(sanitize_binary_output("\u{fffa}x\u{fffb}"), "x");
    }

    #[test]
    fn normalizes_windows_shell_paths() {
        assert_eq!(normalize_windows_shell_path("/mnt/c/Users/x"), "C:\\Users\\x");
        assert_eq!(normalize_windows_shell_path("/cygdrive/d/tmp"), "D:\\tmp");
        // JS regex converts any /x/... path, treating the first char as a drive.
        assert_eq!(normalize_windows_shell_path("/plain/path"), "P:\\lain\\path");
        // Double-slash and backslash paths pass through unchanged.
        assert_eq!(normalize_windows_shell_path("//server/share"), "//server/share");
        assert_eq!(normalize_windows_shell_path("C:\\x"), "C:\\x");
    }

    #[test]
    fn user_agent_format() {
        let agent = get_pi_user_agent("1.2.3");
        assert!(agent.starts_with("pi/1.2.3 ("));
        assert!(agent.contains(')'));
    }
}
