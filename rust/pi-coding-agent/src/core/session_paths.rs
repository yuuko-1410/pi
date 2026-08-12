//! Path helpers used by the session manager, mirroring
//! `packages/coding-agent/src/utils/paths.ts`.
//! ponytail: minimal subset (normalize/resolve/join); if the full utils
//! port lands, switch imports to it.

use std::path::{Component, Path, PathBuf};

/// Expand a leading `~` and unicode-space normalization, matching
/// normalizePath() defaults (expandTilde true, trim false).
pub fn normalize_path(input: &str) -> String {
    let mut normalized = input.replace(
        ['\u{00A0}', '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}',
         '\u{2007}', '\u{2008}', '\u{2009}', '\u{200A}', '\u{202F}', '\u{205F}', '\u{3000}'],
        " ",
    );
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() {
        if normalized == "~" {
            return home;
        }
        if let Some(rest) = normalized.strip_prefix("~/") {
            normalized = Path::new(&home).join(rest).to_string_lossy().to_string();
        }
    }
    normalized
}

/// Resolve a path against the current working directory (or an explicit
/// base), lexically, matching node's path.resolve semantics closely enough
/// for session paths.
pub fn resolve_path(input: &str, base_dir: Option<&str>) -> String {
    let normalized = normalize_path(input);
    let path = Path::new(&normalized);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let base = base_dir
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        base.join(path)
    };
    lexical_normalize(&absolute).to_string_lossy().to_string()
}

/// Lexical normalization (no symlink resolution; node's resolve is lexical).
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    result.push("..");
                }
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

pub fn join(base: &str, path: &str) -> String {
    Path::new(base).join(path).to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_and_absolute() {
        let abs = resolve_path("/a/b/../c", None);
        assert_eq!(abs, "/a/c");
        let rel = resolve_path("x/./y", Some("/base"));
        assert_eq!(rel, "/base/x/y");
    }

    #[test]
    fn tilde_expansion() {
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            assert_eq!(normalize_path("~"), home);
            assert_eq!(normalize_path("~/x"), format!("{home}/x"));
        }
    }

    #[test]
    fn unicode_spaces() {
        assert_eq!(normalize_path("a\u{00A0}b"), "a b");
    }
}
