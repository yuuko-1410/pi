//! Diff rendering, port of `components/diff.ts`.
//!
//! ponytail: JS uses the `diff` package's diffWords for intra-line diffing.
//! The Rust port implements a token-level LCS over word runs (whitespace
//! attached to the following word), matching diffWords grouping closely.

use crate::modes::interactive::theme::theme::theme;

/// Parse a diff line, mirroring the JS regex `/^([+\-\s])(\s*\d*)\s(.*)$/`:
/// prefix char, optional whitespace, optional line number, one separating
/// whitespace, then content.
fn parse_diff_line(line: &str) -> Option<(char, String, String)> {
    let mut chars = line.chars();
    let prefix = chars.next()?;
    if prefix != '+' && prefix != '-' && prefix != ' ' {
        return None;
    }
    let rest = &line[prefix.len_utf8()..];
    let bytes = rest.as_bytes();
    let mut idx = 0;
    // \s*
    while idx < bytes.len() && (bytes[idx] as char).is_whitespace() {
        idx += 1;
    }
    // \d*
    let digits_start = idx;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    let line_num = rest[digits_start..idx].to_string();
    // one separating whitespace
    if idx >= bytes.len() || !(bytes[idx] as char).is_whitespace() {
        return None;
    }
    idx += 1;
    let content = rest[idx..].to_string();
    Some((prefix, line_num, content))
}

fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

/// Tokenize into words with attached whitespace (diffWords grouping).
fn tokenize_words(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        let is_ws = ch.is_whitespace();
        // Token boundaries: whitespace runs and non-whitespace runs.
        while let Some(&(_, next_ch)) = chars.peek() {
            if next_ch.is_whitespace() == is_ws {
                chars.next();
            } else {
                break;
            }
        }
        let end = chars.peek().map(|&(j, _)| j).unwrap_or(text.len());
        let _ = i;
        tokens.push(&text[start..end]);
        start = end;
    }
    if start < text.len() {
        tokens.push(&text[start..]);
    }
    tokens
}

/// LCS diff of token lists; returns removed/added token runs in
/// interleaved order: an enum tag per token so common tokens can be
/// emitted to both lines in the correct position.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DiffOp {
    Common,
    Removed,
    Added,
}

fn lcs_diff_ops<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<(&'a str, DiffOp)> {
    let n = old.len();
    let m = new.len();
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if old[i] == new[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut ops: Vec<(&'a str, DiffOp)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            ops.push((old[i], DiffOp::Common));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            ops.push((old[i], DiffOp::Removed));
            i += 1;
        } else {
            ops.push((new[j], DiffOp::Added));
            j += 1;
        }
    }
    while i < n {
        ops.push((old[i], DiffOp::Removed));
        i += 1;
    }
    while j < m {
        ops.push((new[j], DiffOp::Added));
        j += 1;
    }
    ops
}

/// Compute word-level diff and render with inverse on changed parts.
fn render_intra_line_diff(old_content: &str, new_content: &str, inverse: &dyn Fn(&str) -> String) -> (String, String) {
    let old_tokens = tokenize_words(old_content);
    let new_tokens = tokenize_words(new_content);
    let ops = lcs_diff_ops(&old_tokens, &new_tokens);

    let mut removed_line = String::new();
    let mut added_line = String::new();
    let mut is_first_removed = true;
    let mut is_first_added = true;

    for (token, op) in ops {
        match op {
            DiffOp::Common => {
                removed_line.push_str(token);
                added_line.push_str(token);
            }
            DiffOp::Removed => {
                if is_first_removed {
                    let leading: String = token.chars().take_while(|c| c.is_whitespace()).collect();
                    let value = &token[leading.len()..];
                    removed_line.push_str(&leading);
                    if !value.is_empty() {
                        removed_line.push_str(&inverse(value));
                    }
                    is_first_removed = false;
                } else if !token.is_empty() {
                    removed_line.push_str(&inverse(token));
                }
            }
            DiffOp::Added => {
                if is_first_added {
                    let leading: String = token.chars().take_while(|c| c.is_whitespace()).collect();
                    let value = &token[leading.len()..];
                    added_line.push_str(&leading);
                    if !value.is_empty() {
                        added_line.push_str(&inverse(value));
                    }
                    is_first_added = false;
                } else if !token.is_empty() {
                    added_line.push_str(&inverse(token));
                }
            }
        }
    }

    (removed_line, added_line)
}

pub struct RenderDiffOptions {
    pub file_path: Option<String>,
}

impl Default for RenderDiffOptions {
    fn default() -> Self {
        Self { file_path: None }
    }
}

/// Render a diff string with colored lines and intra-line change highlighting.
pub fn render_diff(diff_text: &str, _options: RenderDiffOptions) -> String {
    let lines: Vec<&str> = diff_text.split('\n').collect();
    let mut result: Vec<String> = Vec::new();
    let mut i = 0;

    // Resolve color ANSI codes eagerly and release the theme mutex before
    // any nested rendering (render_intra_line_diff must not re-lock it).
    let t = theme();
    let t_ref = t.as_ref();
    let context_ansi = t_ref.map(|t| t.get_fg_ansi("toolDiffContext")).unwrap_or_default();
    let removed_ansi = t_ref.map(|t| t.get_fg_ansi("toolDiffRemoved")).unwrap_or_default();
    let added_ansi = t_ref.map(|t| t.get_fg_ansi("toolDiffAdded")).unwrap_or_default();
    drop(t);
    let colorize = |ansi: &str, text: &str| -> String {
        if ansi.is_empty() {
            text.to_string()
        } else {
            format!("{ansi}{text}\x1b[39m")
        }
    };
    let inverse = |text: &str| -> String { format!("\x1b[7m{text}\x1b[27m") };

    while i < lines.len() {
        let line = lines[i];
        let parsed = parse_diff_line(line);

        let Some((prefix, _, _)) = parsed else {
            result.push(colorize(&context_ansi, line));
            i += 1;
            continue;
        };

        if prefix == '-' {
            // Collect consecutive removed lines.
            let mut removed_lines: Vec<(String, String)> = Vec::new();
            while i < lines.len() {
                match parse_diff_line(lines[i]) {
                    Some(('-', line_num, content)) => {
                        removed_lines.push((line_num, content));
                        i += 1;
                    }
                    _ => break,
                }
            }
            // Collect consecutive added lines.
            let mut added_lines: Vec<(String, String)> = Vec::new();
            while i < lines.len() {
                match parse_diff_line(lines[i]) {
                    Some(('+', line_num, content)) => {
                        added_lines.push((line_num, content));
                        i += 1;
                    }
                    _ => break,
                }
            }

            if removed_lines.len() == 1 && added_lines.len() == 1 {
                let (removed_line_num, removed_content) = &removed_lines[0];
                let (added_line_num, added_content) = &added_lines[0];
                let (removed_line, added_line) = render_intra_line_diff(
                    &replace_tabs(removed_content),
                    &replace_tabs(added_content),
                    &inverse,
                );
                let removed_styled = colorize(&removed_ansi, &format!("-{removed_line_num} {removed_line}"));
                let added_styled = colorize(&added_ansi, &format!("+{added_line_num} {added_line}"));
                result.push(removed_styled);
                result.push(added_styled);
            } else {
                for (line_num, content) in &removed_lines {
                    result.push(colorize(&removed_ansi, &format!("-{line_num} {}", replace_tabs(content))));
                }
                for (line_num, content) in &added_lines {
                    result.push(colorize(&added_ansi, &format!("+{line_num} {}", replace_tabs(content))));
                }
            }
        } else if prefix == '+' {
            let (_, line_num, content) = parsed.unwrap();
            result.push(colorize(&added_ansi, &format!("+{line_num} {}", replace_tabs(&content))));
            i += 1;
        } else {
            let (_, line_num, content) = parsed.unwrap();
            result.push(colorize(&context_ansi, &format!(" {line_num} {}", replace_tabs(&content))));
            i += 1;
        }
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_diff_lines() {
        assert_eq!(parse_diff_line("+123 hello"), Some(('+', "123".to_string(), "hello".to_string())));
        assert_eq!(parse_diff_line("- 5  x"), Some(('-', "5".to_string(), " x".to_string())));
        assert_eq!(parse_diff_line(" 12  ctx"), Some((' ', "12".to_string(), " ctx".to_string())));
        assert_eq!(parse_diff_line("context"), None);
    }

    #[test]
    fn renders_context_line() {
        let result = render_diff(" 5 keep this line", RenderDiffOptions::default());
        assert!(result.contains("keep this line"));
        assert!(result.contains("5"));
    }

    #[test]
    fn renders_removed_and_added() {
        let diff = "-1 old line\n+1 new line\n";
        let result = render_diff(diff, RenderDiffOptions::default());
        // Intra-line diff inverts the changed word but keeps common text.
        assert!(result.contains("old"));
        assert!(result.contains("new"));
        assert!(result.contains("line"));
    }

    #[test]
    fn intra_line_diff_inverses_changed_word() {
        let inverse = |text: &str| format!("[7m{text}[27m");
        let (removed, added) = render_intra_line_diff("foo bar baz", "foo qux baz", &inverse);
        assert!(removed.contains("\x1b[7m"));
        assert!(removed.contains("foo"));
        assert!(removed.contains("bar"));
        assert!(added.contains("\x1b[7m"));
    }

    #[test]
    fn renders_unified_hunk() {
        let diff = "--- a\n+++ b\n@@ -1,2 +1,2 @@\n context\n-removed\n+added\n";
        let result = render_diff(diff, RenderDiffOptions::default());
        assert!(result.contains("removed"));
        assert!(result.contains("added"));
        assert!(result.contains("context"));
    }
}
