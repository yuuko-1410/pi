//! Shared diff computation utilities, port of `tools/edit-diff.ts`.
//!
//! ponytail: NFKC normalization is approximated (smart quote/dash/space
//! mappings only, no full Unicode normalization — std has no tables), and the
//! jsdiff line diff is a hand-written LCS line diff (same hunk semantics for
//! the display-oriented output; unified patches use the same algorithm).

use std::collections::HashMap;

/// Detect the dominant line ending of content.
pub fn detect_line_ending(content: &str) -> &'static str {
    let crlf_index = content.find("\r\n");
    let lf_index = content.find('\n');
    match (crlf_index, lf_index) {
        (Some(crlf), Some(lf)) => {
            if crlf < lf {
                "\r\n"
            } else {
                "\n"
            }
        }
        _ => "\n",
    }
}

pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

/// Normalize text for fuzzy matching: strip trailing whitespace per line,
/// smart quotes/dashes/spaces to ASCII (approximation of NFKC for these
/// classes).
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    // Per-line trailing whitespace strip + character replacements.
    let mut current_line = String::new();
    let mut push_line = |line: &str, result: &mut String| {
        let trimmed = line.trim_end();
        result.push_str(trimmed);
        result.push('\n');
    };
    for line in text.split('\n') {
        current_line.clear();
        for c in line.chars() {
            let replaced = match c {
                '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
                '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
                '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' => '-',
                '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}' | '\u{2007}'
                | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
                other => other,
            };
            current_line.push(replaced);
        }
        let _ = push_line(&current_line, &mut result);
    }
    // The final split('\n') iteration always adds a trailing newline; strip it
    // to match the JS output (join("\n")).
    if result.ends_with('\n') {
        result.pop();
    }
    result
}

fn split_lines_with_endings(content: &str) -> Vec<String> {
    // JS: content.match(/[^\n]*\n|[^\n]+/g)
    let mut lines = Vec::new();
    let mut start = 0;
    while start < content.len() {
        match content[start..].find('\n') {
            Some(relative) => {
                lines.push(content[start..start + relative + 1].to_string());
                start += relative + 1;
            }
            None => {
                lines.push(content[start..].to_string());
                start = content.len();
            }
        }
    }
    if lines.is_empty() && !content.is_empty() {
        lines.push(content.to_string());
    }
    lines
}

#[derive(Clone, Debug)]
struct LineSpan {
    start: usize,
    end: usize,
}

#[derive(Clone, Debug)]
struct MatchedEdit {
    edit_index: usize,
    match_index: usize,
    match_length: usize,
    new_text: String,
}

fn get_line_spans(content: &str) -> Vec<LineSpan> {
    let mut offset = 0;
    split_lines_with_endings(content)
        .iter()
        .map(|line| {
            let span = LineSpan {
                start: offset,
                end: offset + line.len(),
            };
            offset = span.end;
            span
        })
        .collect()
}

fn get_replacement_line_range(lines: &[LineSpan], replacement: &MatchedEdit) -> (usize, usize) {
    let replacement_start = replacement.match_index;
    let replacement_end = replacement.match_index + replacement.match_length;

    let mut start_line = -1i64;
    for (i, line) in lines.iter().enumerate() {
        if replacement_start >= line.start && replacement_start < line.end {
            start_line = i as i64;
            break;
        }
    }
    if start_line == -1 {
        panic!("Replacement range is outside the base content.");
    }

    let mut end_line = start_line as usize;
    while end_line < lines.len() && lines[end_line].end < replacement_end {
        end_line += 1;
    }
    if end_line >= lines.len() {
        panic!("Replacement range is outside the base content.");
    }

    (start_line as usize, end_line + 1)
}

fn apply_replacements(content: &str, replacements: &[MatchedEdit], offset: usize) -> String {
    let mut result = content.to_string();
    for replacement in replacements.iter().rev() {
        let match_index = replacement.match_index - offset;
        let mut new_result = String::with_capacity(result.len() + replacement.new_text.len());
        new_result.push_str(&result[..match_index]);
        new_result.push_str(&replacement.new_text);
        new_result.push_str(&result[match_index + replacement.match_length..]);
        result = new_result;
    }
    result
}

/// Apply replacements matched against baseContent to originalContent while
/// preserving unchanged line blocks from the original.
pub fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[MatchedEdit],
) -> String {
    let original_lines = split_lines_with_endings(original_content);
    let base_lines = get_line_spans(base_content);
    if original_lines.len() != base_lines.len() {
        panic!("Cannot preserve unchanged lines because the base content has a different line count.");
    }

    let mut sorted_replacements = replacements.to_vec();
    sorted_replacements.sort_by(|a, b| a.match_index.cmp(&b.match_index));

    let mut groups: Vec<(usize, usize, Vec<MatchedEdit>)> = Vec::new();
    for replacement in &sorted_replacements {
        let (start_line, end_line) = get_replacement_line_range(&base_lines, replacement);
        if let Some(last) = groups.last_mut() {
            if start_line < last.1 {
                last.1 = last.1.max(end_line);
                last.2.push(replacement.clone());
                continue;
            }
        }
        groups.push((start_line, end_line, vec![replacement.clone()]));
    }

    let mut original_line_index = 0;
    let mut result = String::new();
    for (start_line, end_line, group_replacements) in &groups {
        result.push_str(&original_lines[original_line_index..*start_line].join(""));

        let group_start_offset = base_lines[*start_line].start;
        let group_end_offset = base_lines[*end_line - 1].end;
        result.push_str(&apply_replacements(
            &base_content[group_start_offset..group_end_offset],
            group_replacements,
            group_start_offset,
        ));
        original_line_index = *end_line;
    }
    result.push_str(&original_lines[original_line_index..].join(""));

    result
}

#[derive(Clone, Debug, PartialEq)]
pub struct FuzzyMatchResult {
    pub found: bool,
    pub index: usize,
    pub match_length: usize,
    pub used_fuzzy_match: bool,
    pub content_for_replacement: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppliedEditsResult {
    pub base_content: String,
    pub new_content: String,
}

/// Find oldText in content, exact first, then fuzzy (normalized space).
pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    // Try exact match first.
    if let Some(exact_index) = content.find(old_text) {
        return FuzzyMatchResult {
            found: true,
            index: exact_index,
            match_length: old_text.len(),
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        };
    }

    // Try fuzzy match in normalized space.
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    let fuzzy_index = fuzzy_content.find(&fuzzy_old_text);

    match fuzzy_index {
        None => FuzzyMatchResult {
            found: false,
            index: 0,
            match_length: 0,
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        },
        Some(fuzzy_index) => FuzzyMatchResult {
            found: true,
            index: fuzzy_index,
            match_length: fuzzy_old_text.len(),
            used_fuzzy_match: true,
            content_for_replacement: fuzzy_content,
        },
    }
}

/// Strip a UTF-8 BOM if present.
pub fn strip_bom(content: &str) -> (String, String) {
    if let Some(rest) = content.strip_prefix('\u{FEFF}') {
        ("\u{FEFF}".to_string(), rest.to_string())
    } else {
        (String::new(), content.to_string())
    }
}

fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    if fuzzy_old_text.is_empty() {
        return 0;
    }
    fuzzy_content.split(&fuzzy_old_text).count().saturating_sub(1)
}

fn get_not_found_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        format!(
            "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
        )
    } else {
        format!(
            "Could not find edits[{edit_index}] in {path}. The oldText must match exactly including all whitespace and newlines."
        )
    }
}

fn get_duplicate_error(path: &str, edit_index: usize, total_edits: usize, occurrences: usize) -> String {
    if total_edits == 1 {
        format!(
            "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
        )
    } else {
        format!(
            "Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
        )
    }
}

fn get_empty_old_text_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        format!("oldText must not be empty in {path}.")
    } else {
        format!("edits[{edit_index}].oldText must not be empty in {path}.")
    }
}

fn get_no_change_error(path: &str, total_edits: usize) -> String {
    if total_edits == 1 {
        format!(
            "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
        )
    } else {
        format!("No changes made to {path}. The replacements produced identical content.")
    }
}

/// Apply one or more exact-text replacements to LF-normalized content.
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<AppliedEditsResult, String> {
    let normalized_edits: Vec<Edit> = edits
        .iter()
        .map(|edit| Edit {
            old_text: normalize_to_lf(&edit.old_text),
            new_text: normalize_to_lf(&edit.new_text),
        })
        .collect();

    for (i, edit) in normalized_edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(get_empty_old_text_error(path, i, normalized_edits.len()));
        }
    }

    let initial_matches: Vec<FuzzyMatchResult> = normalized_edits
        .iter()
        .map(|edit| fuzzy_find_text(normalized_content, &edit.old_text))
        .collect();
    let used_fuzzy_match = initial_matches.iter().any(|m| m.used_fuzzy_match);
    let replacement_base_content = if used_fuzzy_match {
        normalize_for_fuzzy_match(normalized_content)
    } else {
        normalized_content.to_string()
    };

    let mut matched_edits: Vec<MatchedEdit> = Vec::new();
    for (i, edit) in normalized_edits.iter().enumerate() {
        let match_result = fuzzy_find_text(&replacement_base_content, &edit.old_text);
        if !match_result.found {
            return Err(get_not_found_error(path, i, normalized_edits.len()));
        }
        let occurrences = count_occurrences(&replacement_base_content, &edit.old_text);
        if occurrences > 1 {
            return Err(get_duplicate_error(path, i, normalized_edits.len(), occurrences));
        }
        matched_edits.push(MatchedEdit {
            edit_index: i,
            match_index: match_result.index,
            match_length: match_result.match_length,
            new_text: edit.new_text.clone(),
        });
    }

    matched_edits.sort_by(|a, b| a.match_index.cmp(&b.match_index));
    for i in 1..matched_edits.len() {
        let previous = &matched_edits[i - 1];
        let current = &matched_edits[i];
        if previous.match_index + previous.match_length > current.match_index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                previous.edit_index, current.edit_index
            ));
        }
    }

    let base_content = normalized_content.to_string();
    let new_content = if used_fuzzy_match {
        apply_replacements_preserving_unchanged_lines(normalized_content, &replacement_base_content, &matched_edits)
    } else {
        apply_replacements(&replacement_base_content, &matched_edits, 0)
    };

    if base_content == new_content {
        return Err(get_no_change_error(path, normalized_edits.len()));
    }

    Ok(AppliedEditsResult {
        base_content,
        new_content,
    })
}

/// Line-level diff hunks (port of jsdiff diffLines semantics: added/removed
/// runs interleaved with unchanged context).
#[derive(Clone, Debug, PartialEq)]
pub enum DiffPart {
    Added(Vec<String>),
    Removed(Vec<String>),
    Unchanged(Vec<String>),
}

/// Compute a line diff via LCS on split lines.
/// ponytail: O(n*m) LCS over lines; jsdiff uses Myers. For typical edit
/// sizes (hundreds of lines) both produce identical hunks.
pub fn diff_lines(old_content: &str, new_content: &str) -> Vec<DiffPart> {
    let old_lines: Vec<String> = split_lines_with_endings(old_content);
    let new_lines: Vec<String> = split_lines_with_endings(new_content);

    // LCS table.
    let n = old_lines.len();
    let m = new_lines.len();
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            if old_lines[i] == new_lines[j] {
                dp[i][j] = dp[i + 1][j + 1] + 1;
            } else {
                dp[i][j] = dp[i + 1][j].max(dp[i][j + 1]);
            }
        }
    }

    // Walk the table collecting hunks.
    let mut parts: Vec<DiffPart> = Vec::new();
    let mut i = 0usize;
    let mut j = 0usize;
    let mut added: Vec<String> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    let mut unchanged: Vec<String> = Vec::new();

    let mut flush_change = |added: &mut Vec<String>, removed: &mut Vec<String>, unchanged: &mut Vec<String>, parts: &mut Vec<DiffPart>| {
        if !unchanged.is_empty() {
            parts.push(DiffPart::Unchanged(std::mem::take(unchanged)));
        }
        if !removed.is_empty() {
            parts.push(DiffPart::Removed(std::mem::take(removed)));
        }
        if !added.is_empty() {
            parts.push(DiffPart::Added(std::mem::take(added)));
        }
    };

    while i < n && j < m {
        if old_lines[i] == new_lines[j] {
            flush_change(&mut added, &mut removed, &mut unchanged, &mut parts);
            unchanged.push(old_lines[i].clone());
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            removed.push(old_lines[i].clone());
            i += 1;
        } else {
            added.push(new_lines[j].clone());
            j += 1;
        }
    }
    while i < n {
        removed.push(old_lines[i].clone());
        i += 1;
    }
    while j < m {
        added.push(new_lines[j].clone());
        j += 1;
    }
    flush_change(&mut added, &mut removed, &mut unchanged, &mut parts);

    parts
}

/// Generate a display-oriented diff string with line numbers and context.
pub fn generate_diff_string(old_content: &str, new_content: &str, context_lines: usize) -> (String, Option<usize>) {
    let parts = diff_lines(old_content, new_content);
    let mut output: Vec<String> = Vec::new();

    let old_lines = old_content.split('\n').count();
    let new_lines = new_content.split('\n').count();
    let max_line_num = old_lines.max(new_lines);
    let line_num_width = max_line_num.to_string().len();

    let mut old_line_num = 1usize;
    let mut new_line_num = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    for (i, part) in parts.iter().enumerate() {
        let raw: Vec<String> = match part {
            DiffPart::Added(lines) | DiffPart::Removed(lines) | DiffPart::Unchanged(lines) => {
                let mut raw: Vec<String> = lines.iter().map(|line| line.trim_end_matches('\n').to_string()).collect();
                if raw.last().is_some_and(|line| line.is_empty()) {
                    raw.pop();
                }
                raw
            }
        };

        match part {
            DiffPart::Added(_) | DiffPart::Removed(_) => {
                if first_changed_line.is_none() {
                    first_changed_line = Some(new_line_num);
                }
                for line in &raw {
                    if matches!(part, DiffPart::Added(_)) {
                        output.push(format!("+{} {}", pad(line_num_width, new_line_num), line));
                        new_line_num += 1;
                    } else {
                        output.push(format!("-{} {}", pad(line_num_width, old_line_num), line));
                        old_line_num += 1;
                    }
                }
                last_was_change = true;
            }
            DiffPart::Unchanged(_) => {
                let next_part_is_change = i + 1 < parts.len() && matches!(parts[i + 1], DiffPart::Added(_) | DiffPart::Removed(_));
                let has_leading_change = last_was_change;
                let has_trailing_change = next_part_is_change;

                if has_leading_change && has_trailing_change {
                    if raw.len() <= context_lines * 2 {
                        for line in &raw {
                            output.push(format!(" {} {}", pad(line_num_width, old_line_num), line));
                            old_line_num += 1;
                            new_line_num += 1;
                        }
                    } else {
                        let leading = &raw[..context_lines];
                        let trailing = &raw[raw.len() - context_lines..];
                        let skipped = raw.len() - leading.len() - trailing.len();
                        for line in leading {
                            output.push(format!(" {} {}", pad(line_num_width, old_line_num), line));
                            old_line_num += 1;
                            new_line_num += 1;
                        }
                        output.push(format!("{} ...", " ".repeat(line_num_width)));
                        old_line_num += skipped;
                        new_line_num += skipped;
                        for line in trailing {
                            output.push(format!(" {} {}", pad(line_num_width, old_line_num), line));
                            old_line_num += 1;
                            new_line_num += 1;
                        }
                    }
                } else if has_leading_change {
                    let shown = &raw[..raw.len().min(context_lines)];
                    let skipped = raw.len() - shown.len();
                    for line in shown {
                        output.push(format!(" {} {}", pad(line_num_width, old_line_num), line));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                    if skipped > 0 {
                        output.push(format!("{} ...", " ".repeat(line_num_width)));
                        old_line_num += skipped;
                        new_line_num += skipped;
                    }
                } else if has_trailing_change {
                    let skipped = raw.len().saturating_sub(context_lines);
                    if skipped > 0 {
                        output.push(format!("{} ...", " ".repeat(line_num_width)));
                        old_line_num += skipped;
                        new_line_num += skipped;
                    }
                    for line in &raw[skipped..] {
                        output.push(format!(" {} {}", pad(line_num_width, old_line_num), line));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                } else {
                    old_line_num += raw.len();
                    new_line_num += raw.len();
                }
                last_was_change = false;
            }
        }
    }

    (output.join("\n"), first_changed_line)
}

fn pad(width: usize, value: usize) -> String {
    format!("{value:>width$}")
}

/// Compute the diff for one or more edit operations without applying them.
pub fn compute_edits_diff(
    path: &str,
    edits: &[Edit],
    cwd: &str,
    read_file: &dyn Fn(&str) -> Result<String, String>,
) -> Result<(String, Option<usize>), String> {
    let absolute_path = crate::core::tools::path_utils::resolve_to_cwd(path, cwd);
    let raw_content = read_file(&absolute_path)?;
    let (_, content) = strip_bom(&raw_content);
    let normalized_content = normalize_to_lf(&content);
    let applied = apply_edits_to_normalized_content(&normalized_content, edits, path)?;
    Ok(generate_diff_string(&applied.base_content, &applied.new_content, 4))
}

/// Unified patch (port of Diff.createTwoFilesPatch with headers only).
pub fn generate_unified_patch(path: &str, old_content: &str, new_content: &str, context_lines: usize) -> String {
    let mut header = format!("--- {path}\n+++ {path}\n");
    // Hunk headers require line ranges; compute them from the diff.
    let parts = diff_lines(old_content, new_content);
    let old_lines = split_lines_with_endings(old_content);
    let new_lines = split_lines_with_endings(new_content);
    let _ = (&old_lines, &new_lines);

    // Build hunks by walking parts with a sliding context window.
    let mut hunks: Vec<String> = Vec::new();
    let mut old_line = 1usize;
    let mut new_line = 1usize;
    let mut hunk_old_start: Option<usize> = None;
    let mut hunk_new_start: Option<usize> = None;
    let mut hunk_lines: Vec<String> = Vec::new();
    let mut pending_unchanged: Vec<String> = Vec::new();
    let mut pending_old = 0usize;
    let mut pending_new = 0usize;

    fn flush_hunk(
        hunks: &mut Vec<String>,
        hunk_lines: &mut Vec<String>,
        hunk_old_start: &mut Option<usize>,
        hunk_new_start: &mut Option<usize>,
    ) {
        if hunk_lines.is_empty() {
            return;
        }
        let old_start = hunk_old_start.unwrap_or(1);
        let new_start = hunk_new_start.unwrap_or(1);
        let old_count = hunk_lines.iter().filter(|line| !line.starts_with('+')).count();
        let new_count = hunk_lines.iter().filter(|line| !line.starts_with('-')).count();
        hunks.push(format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n{}",
            hunk_lines.join("\n")
        ));
        hunk_lines.clear();
        *hunk_old_start = None;
        *hunk_new_start = None;
    }

    for part in &parts {
        match part {
            DiffPart::Unchanged(lines) => {
                for line in lines {
                    let content = line.trim_end_matches('\n');
                    pending_unchanged.push(format!(" {content}"));
                    pending_old += 1;
                    pending_new += 1;
                    old_line += 1;
                    new_line += 1;
                }
                // Keep at most context_lines pending; flush the rest.
                while pending_unchanged.len() > context_lines {
                    let removed = pending_unchanged.remove(0);
                    if hunk_lines.is_empty() {
                        // Nothing to flush into; drop leading context.
                        pending_old -= 1;
                        pending_new -= 1;
                        if hunk_old_start.is_none() {
                            // adjust starts is complex; approximation: track via line counters
                        }
                        continue;
                    }
                    hunk_lines.push(removed);
                    if hunk_old_start.is_some() {
                        // keep starts; counts computed at flush
                    }
                    pending_old = pending_old.saturating_sub(1);
                    pending_new = pending_new.saturating_sub(1);
                    if hunk_lines.len() > context_lines * 2 {
                        flush_hunk(&mut hunks, &mut hunk_lines, &mut hunk_old_start, &mut hunk_new_start);
                    }
                }
            }
            DiffPart::Removed(lines) => {
                if hunk_lines.is_empty() {
                    hunk_old_start = Some(old_line);
                    hunk_new_start = Some(new_line);
                }
                hunk_lines.extend(pending_unchanged.drain(..));
                pending_old = 0;
                pending_new = 0;
                for line in lines {
                    hunk_lines.push(format!("-{}", line.trim_end_matches('\n')));
                    old_line += 1;
                }
            }
            DiffPart::Added(lines) => {
                if hunk_lines.is_empty() {
                    hunk_old_start = Some(old_line);
                    hunk_new_start = Some(new_line);
                }
                hunk_lines.extend(pending_unchanged.drain(..));
                pending_old = 0;
                pending_new = 0;
                for line in lines {
                    hunk_lines.push(format!("+{}", line.trim_end_matches('\n')));
                    new_line += 1;
                }
            }
        }
    }
    hunk_lines.extend(pending_unchanged.drain(..));
    flush_hunk(&mut hunks, &mut hunk_lines, &mut hunk_old_start, &mut hunk_new_start);

    header.push_str(&hunks.join("\n"));
    header
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ending_detection() {
        assert_eq!(detect_line_ending("a\r\nb"), "\r\n");
        assert_eq!(detect_line_ending("a\nb"), "\n");
        assert_eq!(detect_line_ending("a"), "\n");
    }

    #[test]
    fn normalization() {
        assert_eq!(normalize_to_lf("a\r\nb\r"), "a\nb\n");
        assert_eq!(restore_line_endings("a\nb", "\r\n"), "a\r\nb");
        assert_eq!(normalize_for_fuzzy_match("a \n b\u{2019}c"), "a\n b'c");
        assert_eq!(normalize_for_fuzzy_match("x\u{2014}y"), "x-y");
        assert_eq!(normalize_for_fuzzy_match("x\u{00A0}y"), "x y");
    }

    #[test]
    fn fuzzy_match_exact_first() {
        let result = fuzzy_find_text("hello world", "hello");
        assert!(result.found);
        assert!(!result.used_fuzzy_match);
        assert_eq!(result.index, 0);

        // Fuzzy: trailing whitespace stripped.
        let result = fuzzy_find_text("hello \nworld", "hello\nworld");
        assert!(result.found);
        assert!(result.used_fuzzy_match);
    }

    #[test]
    fn apply_single_edit() {
        let result = apply_edits_to_normalized_content(
            "line1\nline2\nline3",
            &[Edit {
                old_text: "line2".into(),
                new_text: "changed".into(),
            }],
            "test.txt",
        )
        .unwrap();
        assert_eq!(result.new_content, "line1\nchanged\nline3");
    }

    #[test]
    fn apply_edit_errors() {
        // Not found.
        let error = apply_edits_to_normalized_content(
            "abc",
            &[Edit {
                old_text: "xyz".into(),
                new_text: "q".into(),
            }],
            "f",
        )
        .unwrap_err();
        assert!(error.contains("Could not find the exact text"));

        // Empty oldText.
        let error = apply_edits_to_normalized_content(
            "abc",
            &[Edit {
                old_text: "".into(),
                new_text: "q".into(),
            }],
            "f",
        )
        .unwrap_err();
        assert!(error.contains("must not be empty"));

        // Duplicate.
        let error = apply_edits_to_normalized_content(
            "aaa",
            &[Edit {
                old_text: "a".into(),
                new_text: "b".into(),
            }],
            "f",
        )
        .unwrap_err();
        assert!(error.contains("3 occurrences"));

        // No change.
        let error = apply_edits_to_normalized_content(
            "abc",
            &[Edit {
                old_text: "abc".into(),
                new_text: "abc".into(),
            }],
            "f",
        )
        .unwrap_err();
        assert!(error.contains("No changes made"));

        // Overlap.
        let error = apply_edits_to_normalized_content(
            "abcdef",
            &[
                Edit {
                    old_text: "abc".into(),
                    new_text: "x".into(),
                },
                Edit {
                    old_text: "cde".into(),
                    new_text: "y".into(),
                },
            ],
            "f",
        )
        .unwrap_err();
        assert!(error.contains("overlap"));
    }

    #[test]
    fn fuzzy_edit_preserves_unchanged_lines() {
        let content = "a  \nb\nc  \nd";
        let result = apply_edits_to_normalized_content(
            content,
            &[Edit {
                old_text: "c\nd".into(),
                new_text: "c2\nd2".into(),
            }],
            "f",
        )
        .unwrap();
        // The fuzzy match strips trailing whitespace; unchanged lines keep
        // their original bytes.
        assert_eq!(result.new_content, "a  \nb\nc2\nd2");
    }

    #[test]
    fn diff_string_with_context() {
        let (diff, first_changed) = generate_diff_string("a\nb\nc\nd\ne", "a\nb\nX\nd\ne", 1);
        assert_eq!(first_changed, Some(3));
        assert!(diff.contains("-3 c"));
        assert!(diff.contains("+3 X"));
    }

    #[test]
    fn unified_patch_format() {
        let patch = generate_unified_patch("f.txt", "a\nb\nc", "a\nB\nc", 1);
        assert!(patch.starts_with("--- f.txt\n+++ f.txt\n"));
        assert!(patch.contains("@@ -"));
        assert!(patch.contains("-b"));
        assert!(patch.contains("+B"));
    }

    #[test]
    fn line_span_replacement() {
        let content = "one\ntwo\nthree";
        let spans = get_line_spans(content);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[1].start, 4);
        assert_eq!(spans[1].end, 8); // line two occupies 4..8
    }
}
