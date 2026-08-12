//! Word navigation, port of `packages/tui/src/word-navigation.ts`.
//!
//! Difference: Intl.Segmenter word segmentation is replaced by a simple
//! classification (alphanumeric runs = word-like, whitespace runs = space,
//! everything else = punctuation), matching the common case.

use crate::utils::{is_punctuation_char, is_whitespace_char};

#[derive(Clone)]
pub struct WordSegment {
    pub segment: String,
    pub is_word_like: bool,
}

/// Approximate word segmentation: consecutive chars of the same class
/// (word-like alphanumeric, whitespace, punctuation).
pub fn segment_words(text: &str) -> Vec<WordSegment> {
    let mut segments: Vec<WordSegment> = Vec::new();
    for char in text.chars() {
        let class = if char.is_whitespace() {
            's'
        } else if char.is_alphanumeric() {
            'w'
        } else {
            'p'
        };
        match segments.last_mut() {
            Some(last) if {
                let last_class = if last.is_word_like {
                    'w'
                } else if last.segment.chars().all(|c| c.is_whitespace()) {
                    's'
                } else {
                    'p'
                };
                last_class == class
            } =>
            {
                last.segment.push(char);
            }
            _ => segments.push(WordSegment {
                segment: char.to_string(),
                is_word_like: class == 'w',
            }),
        }
    }
    segments
}

/// Find the cursor position after moving one word backward from `cursor`.
/// Pure function — does not mutate any state.
pub fn find_word_backward(
    text: &str,
    cursor: usize,
    is_atomic: Option<&dyn Fn(&str) -> bool>,
) -> usize {
    if cursor == 0 {
        return 0;
    }
    let text_before_cursor = &text[..cursor];
    let mut segments = segment_words(text_before_cursor);
    let mut new_cursor = cursor;

    // Skip trailing whitespace.
    while let Some(last) = segments.last() {
        let atomic = is_atomic.map(|f| f(&last.segment)).unwrap_or(false);
        if atomic || !is_whitespace_char(last.segment.chars().next().unwrap_or(' ')) {
            break;
        }
        new_cursor -= last.segment.chars().count();
        segments.pop();
    }

    if segments.is_empty() {
        return new_cursor;
    }

    let last = segments.last().cloned().unwrap();
    let atomic = is_atomic.map(|f| f(&last.segment)).unwrap_or(false);

    if atomic {
        // Skip one atomic segment.
        new_cursor -= last.segment.chars().count();
    } else if last.is_word_like {
        // Skip inside one word-like segment, preserving ASCII punctuation
        // boundaries.
        let segment = &last.segment;
        let punctuation_indices: Vec<usize> = segment
            .char_indices()
            .filter(|(_, c)| is_punctuation_char(*c))
            .map(|(index, _)| index)
            .collect();
        if punctuation_indices.is_empty() {
            new_cursor -= segment.chars().count();
        } else {
            let last_match = punctuation_indices[punctuation_indices.len() - 1];
            let char_count_before = segment[..last_match].chars().count();
            let punctuation_len = segment[last_match..].chars().next().unwrap().len_utf8();
            new_cursor -= segment.chars().count() - (char_count_before + punctuation_len);
        }
    } else {
        // Skip non-word non-whitespace run (punctuation).
        while let Some(last) = segments.last() {
            let atomic = is_atomic.map(|f| f(&last.segment)).unwrap_or(false);
            if atomic || last.is_word_like || is_whitespace_char(last.segment.chars().next().unwrap_or(' ')) {
                break;
            }
            new_cursor -= last.segment.chars().count();
            segments.pop();
        }
    }

    new_cursor
}

/// Find the cursor position after moving one word forward from `cursor`.
/// Pure function — does not mutate any state.
pub fn find_word_forward(
    text: &str,
    cursor: usize,
    is_atomic: Option<&dyn Fn(&str) -> bool>,
) -> usize {
    if cursor >= text.chars().count() {
        return text.chars().count();
    }
    let text_after_cursor: String = text.chars().skip(cursor).collect();
    let segments = segment_words(&text_after_cursor);
    let mut iterator = segments.into_iter();
    let mut next = iterator.next();
    let mut new_cursor = cursor;

    // Skip leading whitespace.
    while let Some(segment) = &next {
        let atomic = is_atomic.map(|f| f(&segment.segment)).unwrap_or(false);
        if atomic || !is_whitespace_char(segment.segment.chars().next().unwrap_or(' ')) {
            break;
        }
        new_cursor += segment.segment.chars().count();
        next = iterator.next();
    }

    let Some(segment) = next else {
        return new_cursor;
    };
    let atomic = is_atomic.map(|f| f(&segment.segment)).unwrap_or(false);

    if atomic {
        new_cursor += segment.segment.chars().count();
    } else if segment.is_word_like {
        // Skip inside one word-like segment, preserving ASCII punctuation
        // boundaries.
        let punctuation_index = segment
            .segment
            .char_indices()
            .find(|(_, c)| is_punctuation_char(*c))
            .map(|(index, _)| segment.segment[..index].chars().count());
        new_cursor += punctuation_index.unwrap_or(segment.segment.chars().count());
    } else {
        // Skip non-word non-whitespace run (punctuation): the current
        // segment plus any following punctuation segments.
        new_cursor += segment.segment.chars().count();
        while let Some(next_segment) = iterator.next() {
            let atomic = is_atomic.map(|f| f(&next_segment.segment)).unwrap_or(false);
            if atomic
                || next_segment.is_word_like
                || is_whitespace_char(next_segment.segment.chars().next().unwrap_or(' '))
            {
                break;
            }
            new_cursor += next_segment.segment.chars().count();
        }
    }

    new_cursor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_backward_basic() {
        assert_eq!(find_word_backward("hello world", 11, None), 6);
        assert_eq!(find_word_backward("hello", 5, None), 0);
        assert_eq!(find_word_backward("hello world", 0, None), 0);
    }

    #[test]
    fn word_backward_skips_whitespace() {
        assert_eq!(find_word_backward("hello   world", 13, None), 8);
        assert_eq!(find_word_backward("hello world  ", 13, None), 6);
    }

    #[test]
    fn word_forward_basic() {
        assert_eq!(find_word_forward("hello world", 0, None), 5);
        assert_eq!(find_word_forward("hello world", 6, None), 11);
        assert_eq!(find_word_forward("hello", 5, None), 5);
    }

    #[test]
    fn word_forward_skips_whitespace() {
        assert_eq!(find_word_forward("hello   world", 0, None), 5);
        // From the whitespace gap, the full next word is skipped (JS
        // behavior: cursor lands after the word).
        assert_eq!(find_word_forward("hello   world", 5, None), 13);
    }

    #[test]
    fn punctuation_boundaries() {
        // Backward from after "world!": the "!" punctuation run is skipped.
        assert_eq!(find_word_backward("hello world!", 12, None), 11);
        // Forward into "hello,world" stops before punctuation.
        assert_eq!(find_word_forward("hello,world", 0, None), 5);
        // After the comma: the punctuation run is skipped, landing at 6.
        assert_eq!(find_word_forward("hello,world", 5, None), 6);
        assert_eq!(find_word_forward("hello, world", 5, None), 6);
    }

    #[test]
    fn atomic_segments_skipped_whole() {
        let atomic = |segment: &str| segment.starts_with("**");
        // The trailing "**" is one atomic segment.
        assert_eq!(find_word_backward("foo **bar**", 11, Some(&atomic)), 9);
        // Without atomic handling the trailing punctuation run is skipped.
        assert_eq!(find_word_backward("foo **bar**", 11, None), 9);
    }

    #[test]
    fn segment_words_classifies() {
        // Intl.Segmenter word granularity splits whitespace from
        // alphanumeric runs.
        let segments = segment_words("ab 12,cd");
        let kinds: Vec<bool> = segments.iter().map(|s| s.is_word_like).collect();
        assert_eq!(kinds, vec![true, false, true, false, true]);
        let texts: Vec<&str> = segments.iter().map(|s| s.segment.as_str()).collect();
        assert_eq!(texts, vec!["ab", " ", "12", ",", "cd"]);
    }
}
