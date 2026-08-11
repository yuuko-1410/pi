//! Shared diff computation utilities, port of
//! `packages/agent/src/harness/tools/edit-diff.ts`.
//!
//! Documented differences:
//! - NFKC normalization in `normalizeForFuzzyMatch` is omitted (Rust std has
//!   no NFKC; the remaining char-class transforms are byte-for-byte).
//! - `jsdiff` Myers diff is replaced by an O(nm) LCS line diff; identical
//!   minimal edit scripts on realistic inputs, hunk boundaries may differ on
//!   pathological tie cases.

/// Detect the dominant line ending of a file.
pub fn detect_line_ending(content: &str) -> &'static str {
    let crlf_index = content.find("\r\n");
    let lf_index = content.find('\n');
    match (crlf_index, lf_index) {
        (None, _) => "\n",
        (_, None) => "\n",
        (Some(crlf), Some(lf)) => {
            if crlf < lf {
                "\r\n"
            } else {
                "\n"
            }
        }
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

/// Normalize text for fuzzy matching. NFKC normalization is omitted (see
/// module docs); all other transforms match the JS implementation.
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let mut result = String::new();
    for line in text.split('\n') {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(line.trim_end());
    }
    result = result
        .replace(['\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'], "'")
        .replace(['\u{201C}', '\u{201D}', '\u{201E}', '\u{201F}'], "\"")
        .replace(['\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}'], "-")
        .replace(['\u{00A0}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}', '\u{2008}', '\u{2009}', '\u{200A}', '\u{202F}', '\u{205F}', '\u{3000}'], " ");
    result
}

fn split_lines_with_endings(content: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = Vec::new();
    let mut rest = content;
    while let Some(index) = rest.find('\n') {
        lines.push(&rest[..=index]);
        rest = &rest[index + 1..];
    }
    if !rest.is_empty() {
        lines.push(rest);
    }
    lines
}

#[derive(Clone, Debug, PartialEq)]
struct LineSpan {
    start: usize,
    end: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchedEdit {
    pub edit_index: usize,
    pub match_index: usize,
    pub match_length: usize,
    pub new_text: String,
}

pub type TextReplacement = MatchedEdit;

fn get_line_spans(content: &str) -> Vec<LineSpan> {
    let mut offset = 0usize;
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

fn get_replacement_line_range(lines: &[LineSpan], replacement: &TextReplacement) -> Option<(usize, usize)> {
    let replacement_start = replacement.match_index;
    let replacement_end = replacement.match_index + replacement.match_length;

    let mut start_line = None;
    for (index, line) in lines.iter().enumerate() {
        if replacement_start >= line.start && replacement_start < line.end {
            start_line = Some(index);
            break;
        }
    }
    let start_line = start_line?;

    let mut end_line = start_line;
    while end_line < lines.len() && lines[end_line].end < replacement_end {
        end_line += 1;
    }
    if end_line >= lines.len() {
        return None;
    }

    Some((start_line, end_line + 1))
}

fn apply_replacements(content: &str, replacements: &[TextReplacement], offset: usize) -> String {
    let mut result = content.to_string();
    for replacement in replacements.iter().rev() {
        let match_index = replacement.match_index - offset;
        result = format!(
            "{}{}{}",
            &result[..match_index],
            replacement.new_text,
            &result[match_index + replacement.match_length..]
        );
    }
    result
}

/// Apply replacements matched against `base_content` to `original_content`
/// while preserving unchanged line blocks from the original.
pub fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[TextReplacement],
) -> Result<String, String> {
    let original_lines = split_lines_with_endings(original_content);
    let base_lines = get_line_spans(base_content);
    if original_lines.len() != base_lines.len() {
        return Err("Cannot preserve unchanged lines because the base content has a different line count."
            .to_string());
    }

    // Groups of overlapping line ranges.
    struct Group {
        start_line: usize,
        end_line: usize,
        replacements: Vec<TextReplacement>,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut sorted_replacements = replacements.to_vec();
    sorted_replacements.sort_by_key(|replacement| replacement.match_index);
    for replacement in sorted_replacements {
        let Some((start_line, end_line)) = get_replacement_line_range(&base_lines, &replacement) else {
            return Err("Replacement range is outside the base content.".to_string());
        };
        match groups.last_mut() {
            Some(current) if start_line < current.end_line => {
                current.end_line = current.end_line.max(end_line);
                current.replacements.push(replacement);
            }
            _ => groups.push(Group {
                start_line,
                end_line,
                replacements: vec![replacement],
            }),
        }
    }

    let mut original_line_index = 0usize;
    let mut result = String::new();
    for group in &groups {
        result += &original_lines[original_line_index..group.start_line].join("");

        let group_start_offset = base_lines[group.start_line].start;
        let group_end_offset = base_lines[group.end_line - 1].end;
        result += &apply_replacements(
            &base_content[group_start_offset..group_end_offset],
            &group.replacements,
            group_start_offset,
        );
        original_line_index = group.end_line;
    }
    result += &original_lines[original_line_index..].join("");

    Ok(result)
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

/// Find old_text in content, exact match first, then fuzzy. When fuzzy
/// matching is used, `content_for_replacement` is the normalized content.
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

    // Try fuzzy match - work entirely in normalized space.
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    let Some(fuzzy_index) = fuzzy_content.find(&fuzzy_old_text) else {
        return FuzzyMatchResult {
            found: false,
            index: 0,
            match_length: 0,
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        };
    };

    FuzzyMatchResult {
        found: true,
        index: fuzzy_index,
        match_length: fuzzy_old_text.len(),
        used_fuzzy_match: true,
        content_for_replacement: fuzzy_content,
    }
}

/// Strip UTF-8 BOM if present.
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
    fuzzy_content.split(&fuzzy_old_text).count() - 1
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

    for (index, edit) in normalized_edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(get_empty_old_text_error(path, index, normalized_edits.len()));
        }
    }

    let initial_matches: Vec<FuzzyMatchResult> = normalized_edits
        .iter()
        .map(|edit| fuzzy_find_text(normalized_content, &edit.old_text))
        .collect();
    let used_fuzzy_match = initial_matches.iter().any(|match_result| match_result.used_fuzzy_match);
    let replacement_base_content = if used_fuzzy_match {
        normalize_for_fuzzy_match(normalized_content)
    } else {
        normalized_content.to_string()
    };

    let mut matched_edits: Vec<MatchedEdit> = Vec::new();
    for (index, edit) in normalized_edits.iter().enumerate() {
        let match_result = fuzzy_find_text(&replacement_base_content, &edit.old_text);
        if !match_result.found {
            return Err(get_not_found_error(path, index, normalized_edits.len()));
        }

        let occurrences = count_occurrences(&replacement_base_content, &edit.old_text);
        if occurrences > 1 {
            return Err(get_duplicate_error(path, index, normalized_edits.len(), occurrences));
        }

        matched_edits.push(MatchedEdit {
            edit_index: index,
            match_index: match_result.index,
            match_length: match_result.match_length,
            new_text: edit.new_text.clone(),
        });
    }

    matched_edits.sort_by_key(|edit| edit.match_index);
    for index in 1..matched_edits.len() {
        let previous = &matched_edits[index - 1];
        let current = &matched_edits[index];
        if previous.match_index + previous.match_length > current.match_index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                previous.edit_index, current.edit_index
            ));
        }
    }

    let base_content = normalized_content.to_string();
    let new_content = if used_fuzzy_match {
        apply_replacements_preserving_unchanged_lines(normalized_content, &replacement_base_content, &matched_edits)?
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

// ---------------------------------------------------------------------------
// Line diff (LCS, replacing jsdiff's Myers) and patch generation
// ---------------------------------------------------------------------------

/// One side of a line diff.
#[derive(Clone, Debug, PartialEq)]
pub enum DiffSide {
    Added,
    Removed,
}

/// A line diff chunk (jsdiff `diffLines` part shape).
#[derive(Clone, Debug, PartialEq)]
pub struct DiffPart {
    pub side: Option<DiffSide>,
    pub value: String,
}

/// Line-level diff of two texts; a line is a full line including its ending.
pub fn diff_lines(old_content: &str, new_content: &str) -> Vec<DiffPart> {
    let old_lines = split_lines_with_endings(old_content);
    let new_lines = split_lines_with_endings(new_content);

    // LCS DP over lines.
    let n = old_lines.len();
    let m = new_lines.len();
    let mut dp = vec![0usize; (m + 1) * (n + 1)];
    let width = m + 1;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * width + j] = if old_lines[i] == new_lines[j] {
                dp[(i + 1) * width + j + 1] + 1
            } else {
                dp[(i + 1) * width + j].max(dp[i * width + j + 1])
            };
        }
    }

    // Walk back to collect parts, grouping adjacent same-side lines.
    let mut parts: Vec<DiffPart> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old_lines[i] == new_lines[j] {
            push_part(&mut parts, None, old_lines[i]);
            i += 1;
            j += 1;
        } else if dp[(i + 1) * width + j] >= dp[i * width + j + 1] {
            push_part(&mut parts, Some(DiffSide::Removed), old_lines[i]);
            i += 1;
        } else {
            push_part(&mut parts, Some(DiffSide::Added), new_lines[j]);
            j += 1;
        }
    }
    while i < n {
        push_part(&mut parts, Some(DiffSide::Removed), old_lines[i]);
        i += 1;
    }
    while j < m {
        push_part(&mut parts, Some(DiffSide::Added), new_lines[j]);
        j += 1;
    }

    parts
}

fn push_part(parts: &mut Vec<DiffPart>, side: Option<DiffSide>, line: &str) {
    if let Some(last) = parts.last_mut() {
        if last.side == side {
            last.value.push_str(line);
            return;
        }
    }
    parts.push(DiffPart {
        side,
        value: line.to_string(),
    });
}

/// Generate a standard unified patch (createTwoFilesPatch with headers only).
pub fn generate_unified_patch(
    path: &str,
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> String {
    let mut output = format!("--- {path}\n+++ {path}\n");

    let old_lines: Vec<&str> = old_content.split('\n').collect();
    let new_lines: Vec<&str> = new_content.split('\n').collect();

    // Build the change list: (old_start, new_start, old_lines, new_lines)
    let parts = diff_lines(old_content, new_content);
    let mut changes: Vec<(usize, usize, Vec<&str>, Vec<&str>)> = Vec::new();
    let mut old_line = 1usize;
    let mut new_line = 1usize;
    for part in &parts {
        let lines: Vec<&str> = part.value.split('\n').collect();
        match part.side {
            None => {
                old_line += lines.len().max(1);
                new_line += lines.len().max(1);
            }
            Some(DiffSide::Removed) => {
                changes.push((old_line, new_line, lines.clone(), Vec::new()));
                old_line += lines.len().max(1);
            }
            Some(DiffSide::Added) => {
                changes.push((old_line, new_line, Vec::new(), lines.clone()));
                new_line += lines.len().max(1);
            }
        }
    }

    // Merge changes within 2*context_lines of each other into hunks, then
    // expand each hunk with context lines (jsdiff createTwoFilesPatch).
    let mut hunks: Vec<(usize, usize, usize, usize)> = Vec::new(); // (old_start, new_start, old_end, new_end) exclusive ends
    let mut current: Option<(usize, usize, usize, usize)> = None;
    for (old_start, new_start, old_chunk_len, new_chunk_len) in changes
        .iter()
        .map(|(os, ns, o, n)| (*os, *ns, o.len(), n.len()))
    {
        let old_end = old_start + old_chunk_len;
        let new_end = new_start + new_chunk_len;
        let merged = match &mut current {
            None => false,
            Some((_, _, cur_old_end, cur_new_end)) => {
                let old_gap = old_start.saturating_sub(*cur_old_end);
                let new_gap = new_start.saturating_sub(*cur_new_end);
                if old_gap < context_lines * 2 && new_gap < context_lines * 2 {
                    *cur_old_end = old_end;
                    *cur_new_end = new_end;
                    true
                } else {
                    false
                }
            }
        };
        if merged {
            continue;
        }
        if let Some(hunk) = current.take() {
            hunks.push(hunk);
        }
        current = Some((old_start, new_start, old_end, new_end));
    }
    if let Some(hunk) = current {
        hunks.push(hunk);
    }

    let old_total = old_lines.len().max(1);
    let new_total = new_lines.len().max(1);
    for (old_start, new_start, old_end, new_end) in hunks {
        let old_hunk_start = old_start.saturating_sub(context_lines).max(1);
        let new_hunk_start = new_start.saturating_sub(context_lines).max(1);
        let old_hunk_end = (old_end + context_lines).min(old_total);
        let new_hunk_end = (new_end + context_lines).min(new_total);
        let old_count = old_hunk_end - old_hunk_start + 1;
        let new_count = new_hunk_end - new_hunk_start + 1;
        let old_range = if old_count == 1 {
            format!("{}", old_hunk_start)
        } else {
            format!("{},{}", old_hunk_start, old_count)
        };
        let new_range = if new_count == 1 {
            format!("{}", new_hunk_start)
        } else {
            format!("{},{}", new_hunk_start, new_count)
        };
        output += &format!("@@ -{old_range} +{new_range} @@\n");
        for (index, line) in old_lines.iter().enumerate() {
            if index + 1 >= old_hunk_start && index + 1 <= old_hunk_end {
                output += &format!(" {line}\n");
            }
        }
        // Change lines: walk the diff parts within this hunk's ranges.
        for part in &parts {
            let lines: Vec<&str> = part.value.split('\n').collect();
            match part.side {
                Some(DiffSide::Removed) => {
                    output += &format!("-{}\n", lines.join("\n"));
                }
                Some(DiffSide::Added) => {
                    output += &format!("+{}\n", lines.join("\n"));
                }
                None => {}
            }
        }
    }

    output
}

/// Generate a display-oriented diff string with line numbers and context.
/// Returns the diff string and the first changed line number (in the new
/// file).
pub fn generate_diff_string(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> (String, Option<usize>) {
    let parts = diff_lines(old_content, new_content);
    let mut output: Vec<String> = Vec::new();

    let old_lines: Vec<&str> = old_content.split('\n').collect();
    let new_lines: Vec<&str> = new_content.split('\n').collect();
    let max_line_num = old_lines.len().max(new_lines.len());
    let line_num_width = max_line_num.to_string().len();

    let mut old_line_num = 1usize;
    let mut new_line_num = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    for (index, part) in parts.iter().enumerate() {
        let mut raw: Vec<&str> = part.value.split('\n').collect();
        if raw.last() == Some(&"") {
            raw.pop();
        }

        if part.side.is_some() {
            if first_changed_line.is_none() {
                first_changed_line = Some(new_line_num);
            }

            for line in &raw {
                if part.side == Some(DiffSide::Added) {
                    let line_num = format!("{:>width$}", new_line_num, width = line_num_width);
                    output.push(format!("+{line_num} {line}"));
                    new_line_num += 1;
                } else {
                    let line_num = format!("{:>width$}", old_line_num, width = line_num_width);
                    output.push(format!("-{line_num} {line}"));
                    old_line_num += 1;
                }
            }
            last_was_change = true;
        } else {
            // Context lines - only show a few before/after changes.
            let next_part_is_change = index + 1 < parts.len() && parts[index + 1].side.is_some();
            let has_leading_change = last_was_change;
            let has_trailing_change = next_part_is_change;

            if has_leading_change && has_trailing_change {
                if raw.len() <= context_lines * 2 {
                    for line in &raw {
                        let line_num = format!("{:>width$}", old_line_num, width = line_num_width);
                        output.push(format!(" {line_num} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                } else {
                    let leading_lines = &raw[..context_lines.min(raw.len())];
                    let trailing_lines = &raw[raw.len().saturating_sub(context_lines)..];
                    let skipped_lines = raw.len() - leading_lines.len() - trailing_lines.len();

                    for line in leading_lines {
                        let line_num = format!("{:>width$}", old_line_num, width = line_num_width);
                        output.push(format!(" {line_num} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }

                    output.push(format!(" {:>width$} ...", "", width = line_num_width));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;

                    for line in trailing_lines {
                        let line_num = format!("{:>width$}", old_line_num, width = line_num_width);
                        output.push(format!(" {line_num} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                }
            } else if has_leading_change {
                let shown_lines = &raw[..context_lines.min(raw.len())];
                let skipped_lines = raw.len() - shown_lines.len();

                for line in shown_lines {
                    let line_num = format!("{:>width$}", old_line_num, width = line_num_width);
                    output.push(format!(" {line_num} {line}"));
                    old_line_num += 1;
                    new_line_num += 1;
                }

                if skipped_lines > 0 {
                    output.push(format!(" {:>width$} ...", "", width = line_num_width));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;
                }
            } else if has_trailing_change {
                let skipped_lines = raw.len().saturating_sub(context_lines);
                if skipped_lines > 0 {
                    output.push(format!(" {:>width$} ...", "", width = line_num_width));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;
                }

                for line in &raw[skipped_lines..] {
                    let line_num = format!("{:>width$}", old_line_num, width = line_num_width);
                    output.push(format!(" {line_num} {line}"));
                    old_line_num += 1;
                    new_line_num += 1;
                }
            } else {
                // Skip these context lines entirely.
                old_line_num += raw.len();
                new_line_num += raw.len();
            }

            last_was_change = false;
        }
    }

    (output.join("\n"), first_changed_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endings() {
        assert_eq!(detect_line_ending("a\nb"), "\n");
        assert_eq!(detect_line_ending("a\r\nb"), "\r\n");
        assert_eq!(detect_line_ending("a\r\nb\nc"), "\r\n");
        assert_eq!(normalize_to_lf("a\r\nb\rc"), "a\nb\nc");
        assert_eq!(restore_line_endings("a\nb", "\r\n"), "a\r\nb");
        assert_eq!(restore_line_endings("a\nb", "\n"), "a\nb");
    }

    #[test]
    fn fuzzy_normalization() {
        assert_eq!(normalize_for_fuzzy_match("a  \nb\t"), "a\nb");
        assert_eq!(normalize_for_fuzzy_match("\u{201C}hi\u{201D}"), "\"hi\"");
        assert_eq!(normalize_for_fuzzy_match("x\u{2014}y"), "x-y");
        assert_eq!(normalize_for_fuzzy_match("a\u{00A0}b"), "a b");
    }

    #[test]
    fn fuzzy_find_exact_first() {
        let content = "line one\nline two\n";
        let result = fuzzy_find_text(content, "line two");
        assert!(result.found);
        assert!(!result.used_fuzzy_match);
        assert_eq!(&content[result.index..result.index + result.match_length], "line two");

        // Fuzzy: trailing whitespace mismatch.
        let result = fuzzy_find_text("line one\nline two  \n", "line two\n");
        assert!(result.found);
        assert!(result.used_fuzzy_match);
    }

    #[test]
    fn fuzzy_find_not_found() {
        let result = fuzzy_find_text("abc", "xyz");
        assert!(!result.found);
    }

    #[test]
    fn strip_bom_handling() {
        assert_eq!(strip_bom("\u{FEFF}text"), ("\u{FEFF}".to_string(), "text".to_string()));
        assert_eq!(strip_bom("text"), (String::new(), "text".to_string()));
    }

    #[test]
    fn applies_single_edit() {
        let result = apply_edits_to_normalized_content(
            "hello world",
            &[Edit {
                old_text: "world".to_string(),
                new_text: "rust".to_string(),
            }],
            "test.txt",
        )
        .unwrap();
        assert_eq!(result.new_content, "hello rust");
    }

    #[test]
    fn applies_reverse_order_multiple_edits() {
        let result = apply_edits_to_normalized_content(
            "a\nb\nc\nd",
            &[
                Edit {
                    old_text: "b".to_string(),
                    new_text: "B".to_string(),
                },
                Edit {
                    old_text: "c".to_string(),
                    new_text: "C".to_string(),
                },
            ],
            "t",
        )
        .unwrap();
        assert_eq!(result.new_content, "a\nB\nC\nd");
    }

    #[test]
    fn rejects_empty_duplicate_overlap() {
        let error = apply_edits_to_normalized_content(
            "abc",
            &[Edit {
                old_text: "".to_string(),
                new_text: "x".to_string(),
            }],
            "t",
        )
        .unwrap_err();
        assert!(error.contains("oldText must not be empty"));

        let error = apply_edits_to_normalized_content(
            "x y x",
            &[Edit {
                old_text: "x".to_string(),
                new_text: "z".to_string(),
            }],
            "t",
        )
        .unwrap_err();
        assert!(error.contains("occurrences"));

        let error = apply_edits_to_normalized_content(
            "abcdef",
            &[
                Edit {
                    old_text: "abc".to_string(),
                    new_text: "X".to_string(),
                },
                Edit {
                    old_text: "cde".to_string(),
                    new_text: "Y".to_string(),
                },
            ],
            "t",
        )
        .unwrap_err();
        assert!(error.contains("overlap"));

        let error = apply_edits_to_normalized_content(
            "same",
            &[Edit {
                old_text: "zzz".to_string(),
                new_text: "yyy".to_string(),
            }],
            "t",
        )
        .unwrap_err();
        assert!(error.contains("Could not find"));
    }

    #[test]
    fn no_change_is_an_error() {
        let error = apply_edits_to_normalized_content(
            "abc",
            &[Edit {
                old_text: "b".to_string(),
                new_text: "b".to_string(),
            }],
            "t",
        )
        .unwrap_err();
        assert!(error.contains("No changes made"));
    }

    #[test]
    fn fuzzy_edit_preserves_unchanged_lines() {
        // CRLF-ish content with trailing whitespace; fuzzy matching rewrites
        // only the touched lines.
        let result = apply_edits_to_normalized_content(
            "keep me\nold line  \nkeep too",
            &[Edit {
                old_text: "old line".to_string(),
                new_text: "new line".to_string(),
            }],
            "t",
        )
        .unwrap();
        // Exact prefix match wins (JS behavior): only the matched span is
        // replaced, trailing whitespace on the line is preserved.
        assert_eq!(result.new_content, "keep me\nnew line  \nkeep too");
    }

    #[test]
    fn generates_unified_patch() {
        let patch = generate_unified_patch("a.txt", "one\ntwo\nthree", "one\nTWO\nthree", 4);
        assert!(patch.starts_with("--- a.txt\n+++ a.txt\n"));
        assert!(patch.contains("@@ -1,3 +1,3 @@"));
        assert!(patch.contains("-two"));
        assert!(patch.contains("+TWO"));
    }

    #[test]
    fn generates_display_diff_with_line_numbers() {
        let (diff, first_changed) = generate_diff_string("a\nb\nc", "a\nB\nc", 1);
        assert_eq!(first_changed, Some(2));
        assert!(diff.contains("+2 B"));
        assert!(diff.contains("-2 b"));
        assert!(diff.contains(" 1 a"));
    }

    #[test]
    fn diff_lines_groups_adjacent_sides() {
        let parts = diff_lines("a\nb", "a\nx\ny");
        assert_eq!(
            parts,
            vec![
                DiffPart {
                    side: None,
                    value: "a\n".to_string(),
                },
                DiffPart {
                    side: Some(DiffSide::Removed),
                    value: "b".to_string(),
                },
                DiffPart {
                    side: Some(DiffSide::Added),
                    value: "x\ny".to_string(),
                },
            ]
        );
    }
}
