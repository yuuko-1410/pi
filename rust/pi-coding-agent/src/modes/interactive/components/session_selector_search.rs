//! Session search/filter/sort logic, port of
//! `components/session-selector-search.ts`.

use pi_tui::fuzzy::fuzzy_match;

use crate::core::session_types::SessionInfo;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SortMode {
    Threaded,
    Recent,
    Relevance,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NameFilter {
    All,
    Named,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    Fuzzy,
    Phrase,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchToken {
    pub kind: TokenKind,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct ParsedSearchQuery {
    pub mode: SearchMode,
    pub tokens: Vec<SearchToken>,
    pub regex: Option<regex::Regex>,
    /// If set, parsing failed and the query should be treated as non-matching.
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SearchMode {
    Tokens,
    Regex,
}

pub struct MatchResult {
    pub matches: bool,
    /// Lower is better; only meaningful when matches == true.
    pub score: f64,
}

fn normalize_whitespace_lower(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn get_session_search_text(session: &SessionInfo) -> String {
    format!(
        "{} {} {} {}",
        session.id,
        session.name.as_deref().unwrap_or(""),
        session.all_messages_text,
        session.cwd
    )
}

pub fn has_session_name(session: &SessionInfo) -> bool {
    session.name.as_deref().is_some_and(|name| !name.trim().is_empty())
}

fn matches_name_filter(session: &SessionInfo, filter: NameFilter) -> bool {
    match filter {
        NameFilter::All => true,
        NameFilter::Named => has_session_name(session),
    }
}

pub fn parse_search_query(query: &str) -> ParsedSearchQuery {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return ParsedSearchQuery {
            mode: SearchMode::Tokens,
            tokens: Vec::new(),
            regex: None,
            error: None,
        };
    }

    // Regex mode: re:<pattern>
    if let Some(pattern) = trimmed.strip_prefix("re:") {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return ParsedSearchQuery {
                mode: SearchMode::Regex,
                tokens: Vec::new(),
                regex: None,
                error: Some("Empty regex".to_string()),
            };
        }
        match regex::Regex::new(&format!("(?i){pattern}")) {
            Ok(regex) => ParsedSearchQuery {
                mode: SearchMode::Regex,
                tokens: Vec::new(),
                regex: Some(regex),
                error: None,
            },
            Err(error) => ParsedSearchQuery {
                mode: SearchMode::Regex,
                tokens: Vec::new(),
                regex: None,
                error: Some(error.to_string()),
            },
        }
    } else {
        // Token mode with quote support.
        let mut tokens: Vec<SearchToken> = Vec::new();
        let mut buf = String::new();
        let mut in_quote = false;
        let mut had_unclosed_quote = false;

        for ch in trimmed.chars() {
            if ch == '"' {
                if in_quote {
                    flush_token(&mut buf, TokenKind::Phrase, &mut tokens);
                    in_quote = false;
                } else {
                    flush_token(&mut buf, TokenKind::Fuzzy, &mut tokens);
                    in_quote = true;
                }
                continue;
            }
            if !in_quote && ch.is_whitespace() {
                flush_token(&mut buf, TokenKind::Fuzzy, &mut tokens);
                continue;
            }
            buf.push(ch);
        }

        if in_quote {
            had_unclosed_quote = true;
        }

        // If quotes were unbalanced, fall back to plain whitespace tokenization.
        if had_unclosed_quote {
            let fallback: Vec<SearchToken> = trimmed
                .split_whitespace()
                .map(|t| SearchToken {
                    kind: TokenKind::Fuzzy,
                    value: t.to_string(),
                })
                .collect();
            return ParsedSearchQuery {
                mode: SearchMode::Tokens,
                tokens: fallback,
                regex: None,
                error: None,
            };
        }

        flush_token(&mut buf, TokenKind::Fuzzy, &mut tokens);
        ParsedSearchQuery {
            mode: SearchMode::Tokens,
            tokens,
            regex: None,
            error: None,
        }
    }
}

fn flush_token(buf: &mut String, kind: TokenKind, tokens: &mut Vec<SearchToken>) {
    let value = buf.trim().to_string();
    buf.clear();
    if value.is_empty() {
        return;
    }
    tokens.push(SearchToken { kind, value });
}

pub fn match_session(session: &SessionInfo, parsed: &ParsedSearchQuery) -> MatchResult {
    let text = get_session_search_text(session);

    if parsed.mode == SearchMode::Regex {
        let Some(regex) = &parsed.regex else {
            return MatchResult { matches: false, score: 0.0 };
        };
        return match regex.find(&text) {
            Some(m) => MatchResult {
                matches: true,
                score: m.start() as f64 * 0.1,
            },
            None => MatchResult { matches: false, score: 0.0 },
        };
    }

    if parsed.tokens.is_empty() {
        return MatchResult { matches: true, score: 0.0 };
    }

    let mut total_score = 0.0;
    let mut normalized_text: Option<String> = None;

    for token in &parsed.tokens {
        if token.kind == TokenKind::Phrase {
            let normalized = normalized_text.get_or_insert_with(|| normalize_whitespace_lower(&text));
            let phrase = normalize_whitespace_lower(&token.value);
            if phrase.is_empty() {
                continue;
            }
            match normalized.find(&phrase) {
                Some(idx) => total_score += idx as f64 * 0.1,
                None => return MatchResult { matches: false, score: 0.0 },
            }
            continue;
        }

        let m = fuzzy_match(&token.value, &text);
        if !m.matches {
            return MatchResult { matches: false, score: 0.0 };
        }
        total_score += m.score;
    }

    MatchResult { matches: true, score: total_score }
}

pub fn filter_and_sort_sessions(
    sessions: &[SessionInfo],
    query: &str,
    sort_mode: SortMode,
    name_filter: NameFilter,
) -> Vec<SessionInfo> {
    let name_filtered: Vec<SessionInfo> = sessions
        .iter()
        .filter(|session| matches_name_filter(session, name_filter))
        .cloned()
        .collect();
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return name_filtered;
    }

    let parsed = parse_search_query(query);
    if parsed.error.is_some() {
        return Vec::new();
    }

    // Recent mode: filter only, keep incoming order.
    if sort_mode == SortMode::Recent {
        let mut filtered: Vec<SessionInfo> = Vec::new();
        for session in &name_filtered {
            let result = match_session(session, &parsed);
            if result.matches {
                filtered.push(session.clone());
            }
        }
        return filtered;
    }

    // Relevance mode: sort by score, tie-break by modified desc.
    let mut scored: Vec<(SessionInfo, f64)> = Vec::new();
    for session in &name_filtered {
        let result = match_session(session, &parsed);
        if !result.matches {
            continue;
        }
        scored.push((session.clone(), result.score));
    }

    scored.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.0.modified_ms.partial_cmp(&a.0.modified_ms).unwrap_or(std::cmp::Ordering::Equal))
    });

    scored.into_iter().map(|(session, _)| session).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(id: &str, name: Option<&str>, text: &str, modified_ms: f64) -> SessionInfo {
        SessionInfo {
            path: format!("/tmp/{id}.jsonl"),
            id: id.to_string(),
            cwd: "/tmp".to_string(),
            name: name.map(|s| s.to_string()),
            parent_session_path: None,
            created_ms: 0.0,
            modified_ms,
            message_count: 1,
            first_message: text.to_string(),
            all_messages_text: text.to_string(),
        }
    }

    #[test]
    fn parses_quote_tokens() {
        let parsed = parse_search_query("foo \"node cve\" bar");
        assert_eq!(parsed.error, None);
        assert_eq!(parsed.tokens.len(), 3);
        assert_eq!(parsed.tokens[0].kind, TokenKind::Fuzzy);
        assert_eq!(parsed.tokens[0].value, "foo");
        assert_eq!(parsed.tokens[1].kind, TokenKind::Phrase);
        assert_eq!(parsed.tokens[1].value, "node cve");
        assert_eq!(parsed.tokens[2].value, "bar");
    }

    #[test]
    fn unclosed_quote_falls_back() {
        let parsed = parse_search_query("foo \"bar");
        assert_eq!(parsed.error, None);
        assert_eq!(parsed.tokens.len(), 2);
        assert!(parsed.tokens.iter().all(|t| t.kind == TokenKind::Fuzzy));
    }

    #[test]
    fn regex_mode() {
        let parsed = parse_search_query("re:hello");
        assert_eq!(parsed.mode, SearchMode::Regex);
        assert!(parsed.regex.is_some());
        let session = make_session("1", None, "hello world", 0.0);
        assert!(match_session(&session, &parsed).matches);
    }

    #[test]
    fn bad_regex_reports_error() {
        let parsed = parse_search_query("re:[");
        assert!(parsed.error.is_some());
        let session = make_session("1", None, "hello", 0.0);
        assert!(!match_session(&session, &parsed).matches);
    }

    #[test]
    fn empty_query_matches_all() {
        let parsed = parse_search_query("");
        let session = make_session("1", None, "anything", 0.0);
        assert!(match_session(&session, &parsed).matches);
    }

    #[test]
    fn phrase_matching_normalizes_whitespace() {
        let parsed = parse_search_query("\"node   cve\"");
        let session = make_session("1", None, "node cve here", 0.0);
        assert!(match_session(&session, &parsed).matches);
        let session2 = make_session("2", None, "node v2", 0.0);
        assert!(!match_session(&session2, &parsed).matches);
    }

    #[test]
    fn relevance_sort_tie_breaks_by_modified() {
        let sessions = vec![
            make_session("a", None, "rust cargo", 100.0),
            make_session("b", None, "rust cargo", 200.0),
            make_session("c", None, "python", 300.0),
        ];
        let sorted = filter_and_sort_sessions(&sessions, "rust", SortMode::Relevance, NameFilter::All);
        assert_eq!(sorted.len(), 2);
        assert_eq!(sorted[0].id, "b"); // later modified wins the tie
        assert_eq!(sorted[1].id, "a");
    }

    #[test]
    fn named_filter_only_names() {
        let sessions = vec![
            make_session("a", Some("named"), "text", 1.0),
            make_session("b", None, "text", 1.0),
        ];
        let filtered = filter_and_sort_sessions(&sessions, "", SortMode::Recent, NameFilter::Named);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "a");
    }
}
