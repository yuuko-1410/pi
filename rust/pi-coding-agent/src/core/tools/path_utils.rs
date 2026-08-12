//! Tool path utilities, port of `tools/path-utils.ts`. Async variants are
//! synchronous; NFD normalization is approximated (Rust std has no Unicode
//! normalization tables) and marked as a difference.

use std::fs;
use std::path::Path;

use crate::core::session_paths::resolve_path;

const NARROW_NO_BREAK_SPACE: char = '\u{202F}';

fn try_macos_screenshot_path(file_path: &str) -> String {
    // " (AM|PM)." -> NNBSP before AM/PM (JS regex is case-insensitive).
    // JS: replace(/ (AM|PM)\./gi, "\u202F$1."). The replacement drops the
    // space, so "a (AM).png" becomes "a\u202FAM.png".
    let mut result = file_path.to_string();
    for suffix in ["AM.", "am.", "PM.", "pm."] {
        let needle = format!(" {suffix}");
        while let Some(index) = result.find(&needle) {
            let mut replaced = String::with_capacity(result.len());
            replaced.push_str(&result[..index]);
            replaced.push(NARROW_NO_BREAK_SPACE);
            replaced.push_str(&suffix.to_uppercase());
            replaced.push_str(&result[index + needle.len()..]);
            result = replaced;
        }
    }
    result
}

fn try_nfd_variant(file_path: &str) -> String {
    // ponytail: std lacks NFD normalization; precomposed characters pass
    // through unchanged (most macOS filenames are stored NFD but ASCII-heavy
    // paths are unaffected). Add a unicode-normalization dependency only if
    // non-ASCII screenshot paths become a real issue.
    file_path.to_string()
}

fn try_curly_quote_variant(file_path: &str) -> String {
    file_path.replace('\'', "\u{2019}")
}

fn file_exists(file_path: &str) -> bool {
    Path::new(file_path).exists()
}

/// Expand a path (unicode-space normalization + optional leading @ strip).
pub fn expand_path(file_path: &str) -> String {
    let mut value = file_path.replace(
        ['\u{00A0}', '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}',
         '\u{2007}', '\u{2008}', '\u{2009}', '\u{200A}', '\u{202F}', '\u{205F}', '\u{3000}'],
        " ",
    );
    if let Some(stripped) = value.strip_prefix('@') {
        value = stripped.to_string();
    }
    value
}

/// Resolve a path relative to the given cwd (tilde + absolute handling).
pub fn resolve_to_cwd(file_path: &str, cwd: &str) -> String {
    resolve_path(file_path, Some(cwd))
}

/// Resolve a read path with macOS screenshot-name variants.
pub fn resolve_read_path(file_path: &str, cwd: &str) -> String {
    let resolved = resolve_to_cwd(file_path, cwd);

    if file_exists(&resolved) {
        return resolved;
    }

    let am_pm_variant = try_macos_screenshot_path(&resolved);
    if am_pm_variant != resolved && file_exists(&am_pm_variant) {
        return am_pm_variant;
    }

    let nfd_variant = try_nfd_variant(&resolved);
    if nfd_variant != resolved && file_exists(&nfd_variant) {
        return nfd_variant;
    }

    let curly_variant = try_curly_quote_variant(&resolved);
    if curly_variant != resolved && file_exists(&curly_variant) {
        return curly_variant;
    }

    let nfd_curly_variant = try_curly_quote_variant(&nfd_variant);
    if nfd_curly_variant != resolved && file_exists(&nfd_curly_variant) {
        return nfd_curly_variant;
    }

    resolved
}

/// Whether a path is readable (synchronous access check).
pub fn is_readable(file_path: &str) -> bool {
    fs::metadata(file_path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_path_variants() {
        assert_eq!(expand_path("a\u{00A0}b"), "a b");
        assert_eq!(expand_path("@file.txt"), "file.txt");
        assert_eq!(expand_path("plain"), "plain");
    }

    #[test]
    fn screenshot_variants() {
        // JS matches " AM." (space + AM + dot), as in macOS screenshot names.
        assert_eq!(try_macos_screenshot_path("a AM.png"), "a\u{202F}AM.png");
        assert_eq!(try_macos_screenshot_path("a PM.png"), "a\u{202F}PM.png");
        assert_eq!(try_macos_screenshot_path("a am.png"), "a\u{202F}AM.png");
        assert_eq!(try_macos_screenshot_path("plain.png"), "plain.png");
        assert_eq!(try_curly_quote_variant("d'écran"), "d\u{2019}écran");
    }

    #[test]
    fn resolve_read_path_falls_through_variants() {
        // Missing file: returns the resolved path.
        let resolved = resolve_read_path("/definitely/not/here/file.txt", "/tmp");
        assert!(resolved.ends_with("file.txt"));

        // Existing file: direct hit.
        let existing = resolve_read_path("/tmp", "/tmp");
        assert_eq!(existing, "/tmp");
    }
}
