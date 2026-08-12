//! Autocomplete provider, port of `packages/tui/src/autocomplete.ts`.
//!
//! Differences: the `fd` external tool is replaced by std::fs directory
//! scanning (no .gitignore awareness); walk is synchronous.

use std::path::{Path, PathBuf};

use crate::fuzzy::fuzzy_filter;

const PATH_DELIMITERS: &[char] = &[' ', '\t', '"', '\'', '='];

fn to_display_path(value: &str) -> String {
    value.replace('\\', "/")
}

fn escape_regex(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| {
            if ".*+?^${}()|[]\\".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}

pub fn build_fd_path_query(query: &str) -> String {
    let normalized = to_display_path(query);
    if !normalized.contains('/') {
        return normalized;
    }
    let has_trailing_separator = normalized.ends_with('/');
    let trimmed = normalized.trim_matches('/').to_string();
    if trimmed.is_empty() {
        return normalized;
    }
    let separator_pattern = "[\\\\/]";
    let segments: Vec<String> = trimmed
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(escape_regex)
        .collect();
    if segments.is_empty() {
        return normalized;
    }
    let mut pattern = segments.join(separator_pattern);
    if has_trailing_separator {
        pattern += separator_pattern;
    }
    pattern
}

fn find_last_delimiter(text: &str) -> isize {
    let chars: Vec<char> = text.chars().collect();
    for index in (0..chars.len()).rev() {
        if PATH_DELIMITERS.contains(&chars[index]) {
            return index as isize;
        }
    }
    -1
}

fn find_unclosed_quote_start(text: &str) -> Option<usize> {
    let mut in_quotes = false;
    let mut quote_start = -1isize;
    for (index, char) in text.chars().enumerate() {
        if char == '"' {
            in_quotes = !in_quotes;
            if in_quotes {
                quote_start = index as isize;
            }
        }
    }
    if in_quotes {
        Some(quote_start as usize)
    } else {
        None
    }
}

fn is_token_start(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    text.chars()
        .nth(index - 1)
        .map(|char| PATH_DELIMITERS.contains(&char))
        .unwrap_or(false)
}

fn extract_quoted_prefix(text: &str) -> Option<String> {
    let quote_start = find_unclosed_quote_start(text)?;
    let chars: Vec<char> = text.chars().collect();
    if quote_start > 0 && chars[quote_start - 1] == '@' {
        if !is_token_start(text, quote_start - 1) {
            return None;
        }
        return Some(text.chars().take(quote_start - 1).skip(0).collect::<String>().to_string() + "@\"");
    }
    if !is_token_start(text, quote_start) {
        return None;
    }
    Some(text[quote_start..].to_string())
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedPathPrefix {
    pub raw_prefix: String,
    pub is_at_prefix: bool,
    pub is_quoted_prefix: bool,
}

pub fn parse_path_prefix(prefix: &str) -> ParsedPathPrefix {
    if let Some(rest) = prefix.strip_prefix("@\"") {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('"') {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: false,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('@') {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: false,
        };
    }
    ParsedPathPrefix {
        raw_prefix: prefix.to_string(),
        is_at_prefix: false,
        is_quoted_prefix: false,
    }
}

fn build_completion_value(path: &str, options: &ParsedPathPrefix) -> String {
    let needs_quotes = options.is_quoted_prefix || path.contains(' ');
    let prefix = if options.is_at_prefix { "@" } else { "" };
    if !needs_quotes {
        return format!("{prefix}{path}");
    }
    format!("{prefix}\"{path}\"")
}

#[derive(Clone, Debug, PartialEq)]
pub struct AutocompleteItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AutocompleteSuggestions {
    pub items: Vec<AutocompleteItem>,
    pub prefix: String,
}

pub struct CombinedAutocompleteProvider {
    commands: Vec<SlashCommandOrItem>,
    base_path: String,
}

#[derive(Clone)]
pub enum SlashCommandOrItem {
    Command {
        name: String,
        description: Option<String>,
        argument_hint: Option<String>,
        get_argument_completions: Option<Arc<dyn Fn(&str) -> Option<Vec<AutocompleteItem>> + Send + Sync>>,
    },
    Item(AutocompleteItem),
}

impl std::fmt::Debug for SlashCommandOrItem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SlashCommandOrItem::Command { name, .. } => formatter
                .debug_struct("Command")
                .field("name", name)
                .finish_non_exhaustive(),
            SlashCommandOrItem::Item(item) => formatter.debug_tuple("Item").field(item).finish(),
        }
    }
}

impl SlashCommandOrItem {
    fn name(&self) -> &str {
        match self {
            SlashCommandOrItem::Command { name, .. } => name,
            SlashCommandOrItem::Item(item) => &item.value,
        }
    }
}

use std::sync::Arc;

impl CombinedAutocompleteProvider {
    pub fn new(commands: Vec<SlashCommandOrItem>, base_path: String) -> Self {
        Self { commands, base_path }
    }

    pub fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        force: bool,
    ) -> Option<AutocompleteSuggestions> {
        let current_line = lines.get(cursor_line).cloned().unwrap_or_default();
        let text_before_cursor: String = current_line.chars().take(cursor_col).collect();

        // @ prefix for fuzzy file suggestions.
        if let Some(at_prefix) = self.extract_at_prefix(&text_before_cursor) {
            let parsed = parse_path_prefix(&at_prefix);
            let suggestions = self.get_fuzzy_file_suggestions(&parsed.raw_prefix, parsed.is_quoted_prefix);
            if suggestions.is_empty() {
                return None;
            }
            return Some(AutocompleteSuggestions {
                items: suggestions,
                prefix: at_prefix,
            });
        }

        if !force && text_before_cursor.starts_with('/') {
            let space_index = text_before_cursor.find(' ');
            match space_index {
                None => {
                    let prefix = &text_before_cursor[1..];
                    let command_items: Vec<AutocompleteItem> = self
                        .commands
                        .iter()
                        .map(|cmd| {
                            let name = cmd.name().to_string();
                            let hint = match cmd {
                                SlashCommandOrItem::Command {
                                    argument_hint, ..
                                } => argument_hint.clone(),
                                _ => None,
                            };
                            let desc = match cmd {
                                SlashCommandOrItem::Command { description, .. } => description.clone(),
                                SlashCommandOrItem::Item(item) => item.description.clone(),
                            };
                            let full_desc = match (&hint, &desc) {
                                (Some(hint), Some(desc)) if !desc.is_empty() => format!("{hint} — {desc}"),
                                (Some(hint), _) => hint.clone(),
                                (_, desc) => desc.clone().unwrap_or_default(),
                            };
                            AutocompleteItem {
                                value: name.clone(),
                                label: name,
                                description: if full_desc.is_empty() {
                                    None
                                } else {
                                    Some(full_desc)
                                },
                            }
                        })
                        .collect();
                    let filtered = fuzzy_filter(&command_items, prefix, |item| item.value.clone());
                    if filtered.is_empty() {
                        return None;
                    }
                    return Some(AutocompleteSuggestions {
                        items: filtered,
                        prefix: text_before_cursor.clone(),
                    });
                }
                Some(space_index) => {
                    let command_name = &text_before_cursor[1..space_index];
                    let argument_text = &text_before_cursor[space_index + 1..];
                    let command = self
                        .commands
                        .iter()
                        .find(|cmd| cmd.name() == command_name);
                    let Some(command) = command else {
                        return None;
                    };
                    let SlashCommandOrItem::Command {
                        get_argument_completions,
                        ..
                    } = command
                    else {
                        return None;
                    };
                    let Some(get_argument_completions) = get_argument_completions else {
                        return None;
                    };
                    let argument_suggestions = get_argument_completions(argument_text);
                    let Some(argument_suggestions) = argument_suggestions else {
                        return None;
                    };
                    if argument_suggestions.is_empty() {
                        return None;
                    }
                    return Some(AutocompleteSuggestions {
                        items: argument_suggestions,
                        prefix: argument_text.to_string(),
                    });
                }
            }
        }

        let path_match = self.extract_path_prefix(&text_before_cursor, force)?;
        let suggestions = self.get_file_suggestions(&path_match);
        if suggestions.is_empty() {
            return None;
        }
        Some(AutocompleteSuggestions {
            items: suggestions,
            prefix: path_match,
        })
    }

    pub fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> (Vec<String>, usize, usize) {
        let mut new_lines = lines.to_vec();
        let current_line = new_lines.get(cursor_line).cloned().unwrap_or_default();
        let prefix_chars = prefix.chars().count();
        let before_prefix: String = current_line.chars().take(cursor_col - prefix_chars.min(cursor_col)).collect();
        let after_cursor: String = current_line.chars().skip(cursor_col).collect();
        let is_quoted_prefix = prefix.starts_with('"') || prefix.starts_with("@\"");
        let has_leading_quote_after_cursor = after_cursor.starts_with('"');
        let has_trailing_quote_in_item = item.value.ends_with('"');
        let adjusted_after_cursor = if is_quoted_prefix
            && has_trailing_quote_in_item
            && has_leading_quote_after_cursor
        {
            after_cursor.chars().skip(1).collect()
        } else {
            after_cursor
        };

        let is_slash_command =
            prefix.starts_with('/') && before_prefix.trim().is_empty() && !prefix[1..].contains('/');
        if is_slash_command {
            let new_line = format!("{before_prefix}/{item_value} {adjusted_after_cursor}", item_value = item.value);
            let new_lines = {
                let mut lines = new_lines;
                lines[cursor_line] = new_line;
                lines
            };
            return (
                new_lines,
                cursor_line,
                before_prefix.chars().count() + item.value.chars().count() + 2,
            );
        }

        if prefix.starts_with('@') {
            let is_directory = item.label.ends_with('/');
            let suffix = if is_directory { "" } else { " " };
            let new_line = format!("{}{}{adjusted_after_cursor}", &before_prefix, &item.value);
            let new_line = format!("{new_line}{suffix}");
            new_lines[cursor_line] = new_line;
            let has_trailing_quote = item.value.ends_with('"');
            let cursor_offset = if is_directory && has_trailing_quote {
                item.value.chars().count() - 1
            } else {
                item.value.chars().count()
            };
            return (
                new_lines,
                cursor_line,
                before_prefix.chars().count() + cursor_offset + suffix.chars().count(),
            );
        }

        let text_before_cursor: String = current_line.chars().take(cursor_col).collect();
        if text_before_cursor.contains('/') && text_before_cursor.contains(' ') {
            let new_line = format!("{before_prefix}{}{adjusted_after_cursor}", item.value);
            new_lines[cursor_line] = new_line;
            let is_directory = item.label.ends_with('/');
            let has_trailing_quote = item.value.ends_with('"');
            let cursor_offset = if is_directory && has_trailing_quote {
                item.value.chars().count() - 1
            } else {
                item.value.chars().count()
            };
            return (
                new_lines,
                cursor_line,
                before_prefix.chars().count() + cursor_offset,
            );
        }

        let new_line = format!("{before_prefix}{}{adjusted_after_cursor}", item.value);
        new_lines[cursor_line] = new_line;
        let is_directory = item.label.ends_with('/');
        let has_trailing_quote = item.value.ends_with('"');
        let cursor_offset = if is_directory && has_trailing_quote {
            item.value.chars().count() - 1
        } else {
            item.value.chars().count()
        };
        (
            new_lines,
            cursor_line,
            before_prefix.chars().count() + cursor_offset,
        )
    }

    fn extract_at_prefix(&self, text: &str) -> Option<String> {
        if let Some(quoted_prefix) = extract_quoted_prefix(text) {
            if quoted_prefix.starts_with("@\"") {
                return Some(quoted_prefix);
            }
        }
        let last_delimiter_index = find_last_delimiter(text);
        let token_start = if last_delimiter_index == -1 {
            0
        } else {
            last_delimiter_index as usize + 1
        };
        if text.chars().nth(token_start) == Some('@') {
            return Some(text.chars().skip(token_start).collect());
        }
        None
    }

    fn extract_path_prefix(&self, text: &str, force_extract: bool) -> Option<String> {
        if let Some(quoted_prefix) = extract_quoted_prefix(text) {
            return Some(quoted_prefix);
        }
        let last_delimiter_index = find_last_delimiter(text);
        let path_prefix = if last_delimiter_index == -1 {
            text.to_string()
        } else {
            text.chars().skip(last_delimiter_index as usize + 1).collect()
        };
        if force_extract {
            return Some(path_prefix);
        }
        if path_prefix.contains('/') || path_prefix.starts_with('.') || path_prefix.starts_with("~/") {
            return Some(path_prefix);
        }
        if path_prefix.is_empty() && text.ends_with(' ') {
            return Some(path_prefix);
        }
        None
    }

    fn expand_home_path(&self, path: &str) -> String {
        if let Some(rest) = path.strip_prefix("~/") {
            let home = std::env::var("HOME").unwrap_or_default();
            let expanded = PathBuf::from(&home).join(rest);
            let mut result = expanded.to_string_lossy().to_string();
            if path.ends_with('/') && !result.ends_with('/') {
                result.push('/');
            }
            result
        } else if path == "~" {
            std::env::var("HOME").unwrap_or_default()
        } else {
            path.to_string()
        }
    }

    /// Directory/file suggestions for a path prefix (std::fs based).
    pub fn get_file_suggestions(&self, prefix: &str) -> Vec<AutocompleteItem> {
        let parsed = parse_path_prefix(prefix);
        let mut expanded_prefix = parsed.raw_prefix.clone();
        if expanded_prefix.starts_with('~') {
            expanded_prefix = self.expand_home_path(&expanded_prefix);
        }

        let is_root_prefix = matches!(
            parsed.raw_prefix.as_str(),
            "" | "./" | "../" | "~" | "~/" | "/"
        ) || (parsed.is_at_prefix && parsed.raw_prefix.is_empty());

        let (search_dir, search_prefix): (PathBuf, String) = if is_root_prefix {
            if parsed.raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                (PathBuf::from(&expanded_prefix), String::new())
            } else {
                (PathBuf::from(&self.base_path).join(&expanded_prefix), String::new())
            }
        } else if parsed.raw_prefix.ends_with('/') {
            if parsed.raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                (PathBuf::from(&expanded_prefix), String::new())
            } else {
                (PathBuf::from(&self.base_path).join(&expanded_prefix), String::new())
            }
        } else {
            let dir = Path::new(&expanded_prefix)
                .parent()
                .map(|dir| dir.to_string_lossy().to_string())
                .unwrap_or_default();
            let file = Path::new(&expanded_prefix)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            if parsed.raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                (PathBuf::from(dir), file)
            } else {
                (PathBuf::from(&self.base_path).join(dir), file)
            }
        };

        let Ok(entries) = std::fs::read_dir(&search_dir) else {
            return Vec::new();
        };
        let mut suggestions: Vec<AutocompleteItem> = Vec::new();
        let mut entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
        entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        for entry in entries {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.to_lowercase().starts_with(&search_prefix.to_lowercase()) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else { continue };
            let is_directory = file_type.is_dir()
                || (file_type.is_symlink()
                    && std::fs::metadata(entry.path())
                        .map(|metadata| metadata.is_dir())
                        .unwrap_or(false));

            let display_prefix = &parsed.raw_prefix;
            let relative_path = if display_prefix.ends_with('/') {
                format!("{display_prefix}{name}")
            } else if display_prefix.contains('/') || display_prefix.contains('\\') {
                if display_prefix.starts_with("~/") {
                    let home_relative_dir = &display_prefix[2..];
                    let dir = Path::new(home_relative_dir)
                        .parent()
                        .map(|dir| dir.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if dir == "." || dir.is_empty() {
                        format!("~/{name}")
                    } else {
                        format!("~/{dir}/{name}")
                    }
                } else if display_prefix.starts_with('/') {
                    let dir = Path::new(display_prefix)
                        .parent()
                        .map(|dir| dir.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if dir == "/" {
                        format!("/{name}")
                    } else {
                        format!("{dir}/{name}")
                    }
                } else {
                    let dir = Path::new(display_prefix)
                        .parent()
                        .map(|dir| dir.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let mut relative = format!("{dir}/{name}");
                    if display_prefix.starts_with("./") && !relative.starts_with("./") {
                        relative = format!("./{relative}");
                    }
                    relative
                }
            } else if display_prefix.starts_with('~') {
                format!("~/{name}")
            } else {
                name.clone()
            };

            let relative_path = to_display_path(&relative_path);
            let path_value = if is_directory {
                format!("{relative_path}/")
            } else {
                relative_path.clone()
            };
            let value = build_completion_value(&path_value, &parsed);
            suggestions.push(AutocompleteItem {
                value,
                label: format!("{name}{}", if is_directory { "/" } else { "" }),
                description: None,
            });
        }

        suggestions.sort_by(|a, b| {
            let a_is_dir = a.value.ends_with('/');
            let b_is_dir = b.value.ends_with('/');
            if a_is_dir && !b_is_dir {
                std::cmp::Ordering::Less
            } else if !a_is_dir && b_is_dir {
                std::cmp::Ordering::Greater
            } else {
                a.label.cmp(&b.label)
            }
        });
        suggestions
    }

    /// Fuzzy file suggestions via directory scan (fd replaced by std::fs).
    fn get_fuzzy_file_suggestions(&self, query: &str, is_quoted_prefix: bool) -> Vec<AutocompleteItem> {
        let base_dir = self.base_path.clone();
        let mut suggestions: Vec<AutocompleteItem> = Vec::new();
        let max_depth = 4usize;

        fn walk(
            dir: &Path,
            query_lower: &str,
            display_base: &str,
            depth: usize,
            is_quoted_prefix: bool,
            out: &mut Vec<AutocompleteItem>,
        ) {
            if depth == 0 {
                return;
            }
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name == ".git" {
                    continue;
                }
                let Ok(file_type) = entry.file_type() else { continue };
                let is_directory = file_type.is_dir();
                if !file_type.is_dir() && !file_type.is_file() {
                    continue;
                }
                let full = entry.path().to_string_lossy().to_string();
                let display_path = format!("{display_base}{name}");
                if query_lower.is_empty() || full.to_lowercase().contains(query_lower) {
                    let completion_path = if is_directory {
                        format!("{display_path}/")
                    } else {
                        display_path.clone()
                    };
                    let value = build_completion_value(
                        &completion_path,
                        &ParsedPathPrefix {
                            raw_prefix: String::new(),
                            is_at_prefix: true,
                            is_quoted_prefix,
                        },
                    );
                    out.push(AutocompleteItem {
                        value,
                        label: format!("{name}{}", if is_directory { "/" } else { "" }),
                        description: Some(display_path.clone()),
                    });
                }
                if is_directory && depth > 1 {
                    walk(&entry.path(), query_lower, &format!("{display_path}/"), depth - 1, is_quoted_prefix, out);
                }
            }
        }

        // Scope: use the directory portion of the query.
        let (base_dir, query_part, display_base) = if let Some(slash) = query.rfind('/') {
            let display_base = query[..=slash].to_string();
            let query_part = query[slash + 1..].to_string();
            let expanded = if display_base.starts_with("~/") {
                PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(&display_base[2..])
            } else if display_base.starts_with('/') {
                PathBuf::from(&display_base)
            } else {
                PathBuf::from(&base_dir).join(&display_base)
            };
            if !expanded.is_dir() {
                return suggestions;
            }
            (expanded, query_part, display_base)
        } else {
            (PathBuf::from(base_dir), query.to_string(), String::new())
        };

        walk(&base_dir, &query_part.to_lowercase(), &display_base, max_depth, is_quoted_prefix, &mut suggestions);
        suggestions.truncate(20);
        suggestions
    }

    pub fn should_trigger_file_completion(&self, lines: &[String], cursor_line: usize, cursor_col: usize) -> bool {
        let current_line = lines.get(cursor_line).cloned().unwrap_or_default();
        let text_before_cursor: String = current_line.chars().take(cursor_col).collect();
        if text_before_cursor.trim().starts_with('/') && !text_before_cursor.trim().contains(' ') {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_path_prefixes() {
        let parsed = parse_path_prefix("src/");
        assert_eq!(parsed.raw_prefix, "src/");
        assert!(!parsed.is_at_prefix);
        let parsed = parse_path_prefix("@\"a file\"");
        // JS slices after the opening quote, keeping the trailing quote.
        assert_eq!(parsed.raw_prefix, "a file\"");
        assert!(parsed.is_at_prefix);
        assert!(parsed.is_quoted_prefix);
        let parsed = parse_path_prefix("@src");
        assert_eq!(parsed.raw_prefix, "src");
        assert!(parsed.is_at_prefix);
    }

    #[test]
    fn finds_last_delimiter() {
        assert_eq!(find_last_delimiter("a b c"), 3);
        assert_eq!(find_last_delimiter("abc"), -1);
        assert_eq!(find_last_delimiter("a=b"), 1);
    }

    #[test]
    fn detects_unclosed_quotes() {
        assert_eq!(find_unclosed_quote_start("a \"b"), Some(2));
        assert_eq!(find_unclosed_quote_start("a \"b\" c"), None);
    }

    #[test]
    fn builds_fd_query() {
        assert_eq!(build_fd_path_query("src/main"), "src[\\\\/]main");
        assert_eq!(build_fd_path_query("src/main/"), "src[\\\\/]main[\\\\/]");
        assert_eq!(build_fd_path_query("simple"), "simple");
    }

    #[test]
    fn applies_slash_command_completion() {
        let provider = CombinedAutocompleteProvider::new(Vec::new(), "/tmp".to_string());
        let lines = vec!["/hel".to_string()];
        let (new_lines, _, col) = provider.apply_completion(
            &lines,
            0,
            4,
            &AutocompleteItem {
                value: "help".to_string(),
                label: "help".to_string(),
                description: None,
            },
            "/hel",
        );
        assert_eq!(new_lines[0], "/help ");
        assert_eq!(col, 6);
    }

    #[test]
    fn applies_file_completion() {
        let provider = CombinedAutocompleteProvider::new(Vec::new(), "/tmp".to_string());
        let lines = vec!["cd src".to_string()];
        let (new_lines, _, col) = provider.apply_completion(
            &lines,
            0,
            6,
            &AutocompleteItem {
                value: "src/main.ts".to_string(),
                label: "main.ts".to_string(),
                description: None,
            },
            "src",
        );
        assert_eq!(new_lines[0], "cd src/main.ts");
        assert_eq!(col, 14);
    }

    #[test]
    fn at_prefix_completion() {
        let provider = CombinedAutocompleteProvider::new(Vec::new(), "/tmp".to_string());
        let lines = vec!["read @".to_string()];
        let (new_lines, _, col) = provider.apply_completion(
            &lines,
            0,
            6,
            &AutocompleteItem {
                value: "\"a file.txt\"".to_string(),
                label: "a file.txt".to_string(),
                description: None,
            },
            "@",
        );
        assert!(new_lines[0].contains("a file.txt"));
        assert_eq!(col, 18);
    }

    #[test]
    fn file_suggestions_from_directory() {
        let dir = std::env::temp_dir().join(format!("pi-tui-ac-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("main.ts"), "x").unwrap();
        std::fs::write(dir.join("lib.ts"), "x").unwrap();
        let provider = CombinedAutocompleteProvider::new(Vec::new(), dir.to_string_lossy().to_string());
        let suggestions = provider.get_file_suggestions("");
        let labels: Vec<&str> = suggestions.iter().map(|item| item.label.as_str()).collect();
        assert!(labels.contains(&"main.ts"));
        assert!(labels.contains(&"sub/"));
        // Directories sort first.
        assert!(suggestions[0].label.ends_with('/'));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn quoted_suggestion_values() {
        let provider = CombinedAutocompleteProvider::new(Vec::new(), "/tmp".to_string());
        let suggestions = provider.get_file_suggestions("@\"src\"");
        // Should not panic and produce quoted values when files exist.
        for item in suggestions {
            assert!(item.value.starts_with('@'));
        }
    }

    #[test]
    fn should_trigger_excludes_slash_commands() {
        let provider = CombinedAutocompleteProvider::new(Vec::new(), "/tmp".to_string());
        assert!(!provider.should_trigger_file_completion(&["/help".to_string()], 0, 5));
        assert!(provider.should_trigger_file_completion(&["cd src".to_string()], 0, 6));
    }
}
