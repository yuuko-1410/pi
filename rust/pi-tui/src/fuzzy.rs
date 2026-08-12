//! Fuzzy matching utilities, port of `packages/tui/src/fuzzy.ts`.
//!
//! Matches if all query characters appear in order (not necessarily
//! consecutive). Lower score = better match.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FuzzyMatch {
    pub matches: bool,
    pub score: f64,
}

fn match_query(normalized_query: &str, text_lower: &str) -> FuzzyMatch {
    if normalized_query.is_empty() {
        return FuzzyMatch {
            matches: true,
            score: 0.0,
        };
    }

    if normalized_query.len() > text_lower.len() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    let query_chars: Vec<char> = normalized_query.chars().collect();
    let text_chars: Vec<char> = text_lower.chars().collect();
    let mut query_index = 0usize;
    let mut score = 0.0f64;
    let mut last_match_index = -1isize;
    let mut consecutive_matches = 0usize;

    for (index, &char) in text_chars.iter().enumerate() {
        if query_index >= query_chars.len() {
            break;
        }
        if char == query_chars[query_index] {
            let is_word_boundary = index == 0
                || matches!(
                    text_chars.get(index - 1),
                    Some(c) if c.is_whitespace() || matches!(c, '-' | '_' | '.' | '/' | ':')
                );

            // Reward consecutive matches.
            if last_match_index == index as isize - 1 {
                consecutive_matches += 1;
                score -= consecutive_matches as f64 * 5.0;
            } else {
                consecutive_matches = 0;
                // Penalize gaps.
                if last_match_index >= 0 {
                    score += (index as isize - last_match_index - 1) as f64 * 2.0;
                }
            }

            // Reward word boundary matches.
            if is_word_boundary {
                score -= 10.0;
            }

            // Slight penalty for later matches.
            score += index as f64 * 0.1;

            last_match_index = index as isize;
            query_index += 1;
        }
    }

    if query_index < query_chars.len() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    if normalized_query == text_lower {
        score -= 100.0;
    }

    FuzzyMatch {
        matches: true,
        score,
    }
}

/// Split a query like `abc123` or `123abc` into (letters, digits). The
/// order is decided by the caller via the original first character.
fn split_letters_digits(query: &str) -> Option<(String, String)> {
    let mut letters = String::new();
    let mut digits = String::new();
    for char in query.chars() {
        if char.is_ascii_digit() {
            digits.push(char);
        } else if char.is_ascii_alphabetic() {
            letters.push(char);
        } else {
            return None;
        }
    }
    if letters.is_empty() || digits.is_empty() {
        None
    } else {
        Some((letters, digits))
    }
}

/// Fuzzy-match a query against text. Tries the primary match first, then a
/// swapped alphanumeric form (e.g. `g1` ↔ `1g`).
pub fn fuzzy_match(query: &str, text: &str) -> FuzzyMatch {
    let query_lower = query.to_lowercase();
    let text_lower = text.to_lowercase();

    let primary_match = match_query(&query_lower, &text_lower);
    if primary_match.matches {
        return primary_match;
    }

    // `^(letters)(digits)$` or `^(digits)(letters)$` swap.
    let swapped_query = match split_letters_digits(&query_lower) {
        Some((letters, digits)) => {
            // Both forms are handled by the same split; decide the swap by
            // which part came first in the original.
            let original_first_digit = query_lower.chars().next().is_some_and(|c| c.is_ascii_digit());
            if original_first_digit {
                format!("{letters}{digits}")
            } else {
                format!("{digits}{letters}")
            }
        }
        None => String::new(),
    };

    if swapped_query.is_empty() {
        return primary_match;
    }

    let swapped_match = match_query(&swapped_query, &text_lower);
    if !swapped_match.matches {
        return primary_match;
    }

    FuzzyMatch {
        matches: true,
        score: swapped_match.score + 5.0,
    }
}

/// Filter and sort items by fuzzy match quality (best matches first).
/// Supports whitespace- and slash-separated tokens: all tokens must match.
pub fn fuzzy_filter<T, F>(items: &[T], query: &str, get_text: F) -> Vec<T>
where
    T: Clone,
    F: Fn(&T) -> String,
{
    if query.trim().is_empty() {
        return items.to_vec();
    }

    let tokens: Vec<String> = query
        .trim()
        .split(|c: char| c.is_whitespace() || c == '/')
        .filter(|token| !token.is_empty())
        .map(|token| token.to_string())
        .collect();

    if tokens.is_empty() {
        return items.to_vec();
    }

    let mut results: Vec<(T, f64)> = Vec::new();
    for item in items {
        let text = get_text(item);
        let mut total_score = 0.0f64;
        let mut all_match = true;
        for token in &tokens {
            let match_result = fuzzy_match(token, &text);
            if match_result.matches {
                total_score += match_result.score;
            } else {
                all_match = false;
                break;
            }
        }
        if all_match {
            results.push((item.clone(), total_score));
        }
    }

    results.sort_by(|left, right| {
        left.1
            .partial_cmp(&right.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.into_iter().map(|(item, _)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_with_zero_score() {
        let result = fuzzy_match("", "anything");
        assert!(result.matches);
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn exact_match_gets_bonus() {
        // Same query, same match positions: the exact match carries the
        // -100 bonus over the non-exact one.
        let exact = fuzzy_match("hello", "hello");
        let not_exact = fuzzy_match("hello", "hello there");
        assert!(exact.matches);
        assert!(not_exact.matches);
        assert!(exact.score < not_exact.score);
        assert!((exact.score - (not_exact.score - 100.0)).abs() < 1e-9);
    }

    #[test]
    fn out_of_order_fails() {
        let result = fuzzy_match("ba", "ab");
        assert!(!result.matches);
    }

    #[test]
    fn word_boundary_rewarded() {
        let boundary = fuzzy_match("cb", "my-cat-box");
        let no_boundary = fuzzy_match("cb", "accent bar");
        assert!(boundary.matches);
        assert!(no_boundary.matches);
        assert!(boundary.score < no_boundary.score);
    }

    #[test]
    fn consecutive_matches_rewarded() {
        let consecutive = fuzzy_match("hel", "hello");
        let gapped = fuzzy_match("hlo", "hello");
        assert!(consecutive.matches);
        assert!(gapped.matches);
        assert!(consecutive.score < gapped.score);
    }

    #[test]
    fn alphanumeric_swap_matches() {
        let direct = fuzzy_match("g1", "group1");
        assert!(direct.matches);
        let swapped = fuzzy_match("1g", "group1");
        assert!(swapped.matches);
        // Swapped match carries a +5 penalty.
        assert!(swapped.score > direct.score);
    }

    #[test]
    fn fuzzy_filter_tokenizes() {
        let items = vec!["src/main.ts", "src/lib.ts", "tests/main.test.ts"];
        let filtered = fuzzy_filter(&items, "src ts", |item| item.to_string());
        assert!(filtered.contains(&"src/main.ts"));
        assert!(!filtered.contains(&"tests/main.test.ts"));

        let filtered = fuzzy_filter(&items, "main ts", |item| item.to_string());
        assert_eq!(filtered, vec!["src/main.ts", "tests/main.test.ts"]);

        // Empty query returns items unchanged.
        assert_eq!(fuzzy_filter(&items, "  ", |item| item.to_string()), items);
    }
}
