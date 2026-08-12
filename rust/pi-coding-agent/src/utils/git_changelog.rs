//! Git URL parsing and changelog utilities, ports of
//! `packages/coding-agent/src/utils/{git,changelog}.ts`.
//!
//! `hosted-git-info` is replaced by a built-in recognizer for the common
//! host forms (github.com/gitlab.com/bitbucket.org plus scp-like git@
//! and git:// / https:// / ssh:// URLs) — the npm package's full host
//! registry is not ported (documented).

#[derive(Clone, Debug, PartialEq)]
pub struct GitSource {
    pub repo: String,
    pub host: String,
    pub path: String,
    pub ref_: Option<String>,
    pub pinned: bool,
}

/// Split a git URL into repo and ref (JS `splitRef`).
fn split_ref(url: &str) -> (String, Option<String>) {
    if let Some(rest) = url.strip_prefix("git@") {
        let (host, path_with_maybe_ref) = rest.split_once(':').unwrap_or((rest, ""));
        if let Some(ref_separator) = path_with_maybe_ref.find('@') {
            let repo_path = &path_with_maybe_ref[..ref_separator];
            let ref_ = &path_with_maybe_ref[ref_separator + 1..];
            if !repo_path.is_empty() && !ref_.is_empty() {
                return (format!("git@{host}:{repo_path}"), Some(ref_.to_string()));
            }
        }
        return (url.to_string(), None);
    }

    if url.contains("://") {
        if let Some(scheme_end) = url.find("://") {
            let after_scheme = &url[scheme_end + 3..];
            if let Some(path_start) = after_scheme.find('/') {
                let path_with_maybe_ref = &after_scheme[path_start + 1..];
                if let Some(ref_separator) = path_with_maybe_ref.find('@') {
                    let repo_path = &path_with_maybe_ref[..ref_separator];
                    let ref_ = &path_with_maybe_ref[ref_separator + 1..];
                    if !repo_path.is_empty() && !ref_.is_empty() {
                        let scheme = &url[..scheme_end + 3];
                        let host_part = &after_scheme[..path_start];
                        return (format!("{scheme}{host_part}/{repo_path}").trim_end_matches('/').to_string(), Some(ref_.to_string()));
                    }
                }
            }
        }
        return (url.to_string(), None);
    }

    let Some(slash_index) = url.find('/') else {
        return (url.to_string(), None);
    };
    let host = &url[..slash_index];
    let path_with_maybe_ref = &url[slash_index + 1..];
    if let Some(ref_separator) = path_with_maybe_ref.find('@') {
        let repo_path = &path_with_maybe_ref[..ref_separator];
        let ref_ = &path_with_maybe_ref[ref_separator + 1..];
        if !repo_path.is_empty() && !ref_.is_empty() {
            return (format!("{host}/{repo_path}"), Some(ref_.to_string()));
        }
    }
    (url.to_string(), None)
}

fn decode_for_validation(value: &str) -> Option<String> {
    percent_decode(value)
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = hex_val(bytes[index + 1])?;
            let low = hex_val(bytes[index + 2])?;
            result.push(high * 16 + low);
            index += 3;
            continue;
        }
        result.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(result).ok()
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn has_unsafe_git_install_part(value: &str, allow_slash: bool) -> bool {
    let decoded = decode_for_validation(value);
    if decoded.is_none() {
        return true;
    }
    let decoded = decoded.unwrap();
    let candidates = [value.to_string(), decoded];
    for candidate in candidates {
        if candidate.contains('\0') || candidate.contains('\\') || candidate.starts_with('/') {
            return true;
        }
        if !allow_slash && candidate.contains('/') {
            return true;
        }
        if candidate.split('/').any(|part| part == "..") {
            return true;
        }
    }
    false
}

fn build_git_source(repo: &str, host: &str, path: &str, ref_: Option<&str>) -> Option<GitSource> {
    if path.starts_with('/') {
        return None;
    }
    let normalized_path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_start_matches('/')
        .to_string();
    if host.is_empty() || normalized_path.is_empty() || normalized_path.split('/').count() < 2 {
        return None;
    }
    if has_unsafe_git_install_part(host, false) || has_unsafe_git_install_part(&normalized_path, true) {
        return None;
    }
    Some(GitSource {
        repo: repo.to_string(),
        host: host.to_string(),
        path: normalized_path,
        ref_: ref_.map(|value| value.to_string()),
        pinned: ref_.is_some(),
    })
}

/// hosted-git-info subset: recognize user/project/committish from common
/// forms.
struct HostedInfo {
    domain: String,
    user: String,
    project: String,
    committish: Option<String>,
}

fn hosted_git_info_from_url(candidate: &str) -> Option<HostedInfo> {
    let candidate = candidate.trim();
    // Strip #ref suffix.
    let (base, committish) = match candidate.find('#') {
        Some(index) => (&candidate[..index], Some(candidate[index + 1..].to_string())),
        None => (candidate, None),
    };
    let base = base.strip_suffix(".git").unwrap_or(base);

    // https://github.com/user/repo
    let mut domain = "";
    let mut path_part = "";
    if let Some(scheme_end) = base.find("://") {
        let after = &base[scheme_end + 3..];
        if let Some(slash) = after.find('/') {
            let candidate_domain = &after[..slash];
            if is_known_host(candidate_domain) {
                domain = candidate_domain;
                path_part = &after[slash + 1..];
            }
        }
    } else if let Some(rest) = base.strip_prefix("git@") {
        if let Some(colon) = rest.find(':') {
            let candidate_domain = &rest[..colon];
            if is_known_host(candidate_domain) {
                domain = candidate_domain;
                path_part = &rest[colon + 1..];
            }
        }
    } else if let Some(slash) = base.find('/') {
        let candidate_domain = &base[..slash];
        if is_known_host(candidate_domain) {
            domain = candidate_domain;
            path_part = &base[slash + 1..];
        }
    }

    if domain.is_empty() {
        return None;
    }
    let parts: Vec<&str> = path_part.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() < 2 {
        return None;
    }
    let project = parts[1].trim_end_matches(".git").to_string();
    Some(HostedInfo {
        domain: domain.to_string(),
        user: parts[0].to_string(),
        project,
        committish,
    })
}

fn is_known_host(domain: &str) -> bool {
    matches!(
        domain.to_ascii_lowercase().as_str(),
        "github.com" | "gitlab.com" | "bitbucket.org"
    )
}

/// Parse a git source (JS `parseGitUrl`).
pub fn parse_git_url(source: &str) -> Option<GitSource> {
    let trimmed = source.trim();
    let has_git_prefix = trimmed.starts_with("git:");
    let url = if has_git_prefix {
        trimmed[4..].trim()
    } else {
        trimmed
    };

    if !has_git_prefix {
        let has_protocol = url.starts_with("https://")
            || url.starts_with("http://")
            || url.starts_with("ssh://")
            || url.starts_with("git://");
        if !has_protocol {
            return None;
        }
    }

    let (repo_without_ref, ref_) = split_ref(url);

    // hosted candidates
    let mut candidates: Vec<String> = Vec::new();
    if let Some(ref_) = &ref_ {
        candidates.push(format!("{repo_without_ref}#{ref_}"));
    }
    candidates.push(url.to_string());
    for candidate in &candidates {
        if let Some(info) = hosted_git_info_from_url(candidate) {
            if ref_.is_some() && info.project.contains('@') {
                continue;
            }
            let use_https = !repo_without_ref.starts_with("http://")
                && !repo_without_ref.starts_with("https://")
                && !repo_without_ref.starts_with("ssh://")
                && !repo_without_ref.starts_with("git://")
                && !repo_without_ref.starts_with("git@");
            let repo = if use_https {
                format!("https://{repo_without_ref}")
            } else {
                repo_without_ref.clone()
            };
            let resolved_ref = info.committish.clone().or_else(|| ref_.clone());
            return build_git_source(&repo, &info.domain, &format!("{}/{}", info.user, info.project), resolved_ref.as_deref());
        }
    }

    // generic parsing
    parse_generic_git_url(url)
}

fn parse_generic_git_url(url: &str) -> Option<GitSource> {
    let (repo_without_ref, ref_) = split_ref(url);
    let mut repo = repo_without_ref.clone();
    let mut host = String::new();
    let mut path = String::new();

    if let Some(rest) = repo_without_ref.strip_prefix("git@") {
        if let Some(colon) = rest.find(':') {
            host = rest[..colon].to_string();
            path = rest[colon + 1..].to_string();
        } else {
            return None;
        }
    } else if repo_without_ref.starts_with("https://")
        || repo_without_ref.starts_with("http://")
        || repo_without_ref.starts_with("ssh://")
        || repo_without_ref.starts_with("git://")
    {
        if let Some(scheme_end) = repo_without_ref.find("://") {
            let after = &repo_without_ref[scheme_end + 3..];
            let (host_part, path_part) = match after.find('/') {
                Some(slash) => (&after[..slash], &after[slash + 1..]),
                None => (after, ""),
            };
            host = host_part.to_string();
            path = path_part.to_string();
        }
    } else {
        let Some(slash_index) = repo_without_ref.find('/') else {
            return None;
        };
        host = repo_without_ref[..slash_index].to_string();
        path = repo_without_ref[slash_index + 1..].to_string();
        if !host.contains('.') && host != "localhost" {
            return None;
        }
        repo = format!("https://{repo_without_ref}");
    }

    build_git_source(&repo, &host, &path, ref_.as_deref())
}

// ---------------------------------------------------------------------------
// changelog.ts
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct ChangelogEntry {
    pub major: i64,
    pub minor: i64,
    pub patch: i64,
    pub content: String,
}

impl ChangelogEntry {
    pub fn version_string(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
}

const GITHUB_REPO: &str = "earendil-works/pi";
const CHANGELOG_LINK_BASE_PATH: &str = "packages/coding-agent";

fn normalize_tag(version: &str) -> String {
    if version.starts_with('v') {
        version.to_string()
    } else {
        format!("v{version}")
    }
}

fn split_local_target(target: &str) -> (String, String, String) {
    let (before_hash, fragment) = match target.find('#') {
        Some(index) => (&target[..index], target[index..].to_string()),
        None => (target, String::new()),
    };
    match before_hash.find('?') {
        Some(index) => (
            fragment,
            before_hash[..index].to_string(),
            before_hash[index..].to_string(),
        ),
        None => (fragment, before_hash.to_string(), String::new()),
    }
}

fn resolve_repository_path(target_path: &str) -> Option<String> {
    let normalized = target_path.replace('\\', "/");
    let joined = if normalized.starts_with('/') {
        std::path::Path::new("/")
            .join(normalized.trim_start_matches('/'))
            .to_string_lossy()
            .to_string()
    } else {
        std::path::Path::new(CHANGELOG_LINK_BASE_PATH)
            .join(&normalized)
            .to_string_lossy()
            .to_string()
    };
    if joined == "." || joined == ".." || joined.starts_with("../") {
        None
    } else {
        Some(joined)
    }
}

fn is_directory_target(original_path: &str, repository_path: &str) -> bool {
    if original_path.ends_with('/') {
        return true;
    }
    let basename = std::path::Path::new(repository_path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    !basename.contains('.')
}

fn normalize_changelog_link_target(target: &str, tag: &str) -> String {
    let mut canonical = target
        .replacen("https://github.com/badlogic/pi-mono", &format!("https://github.com/{GITHUB_REPO}"), 1)
        .replacen("https://github.com/earendil-works/pi-mono", &format!("https://github.com/{GITHUB_REPO}"), 1);
    let repo_url = format!("https://github.com/{GITHUB_REPO}");
    for route in ["blob", "tree"] {
        for branch in ["main", "master"] {
            let prefix = format!("{repo_url}/{route}/{branch}/");
            if let Some(rest) = canonical.strip_prefix(&prefix) {
                canonical = format!("{repo_url}/{route}/{tag}/{rest}");
            }
        }
    }
    if canonical.starts_with('#') || canonical.starts_with("//") || scheme_match(&canonical) {
        return canonical;
    }
    let (fragment, path_part, query) = split_local_target(&canonical);
    if path_part.is_empty() {
        return canonical;
    }
    let Some(repository_path) = resolve_repository_path(&path_part) else {
        return canonical;
    };
    let route = if is_directory_target(&path_part, &repository_path) {
        "tree"
    } else {
        "blob"
    };
    let encoded = percent_encode_path(&repository_path);
    format!("{repo_url}/{route}/{tag}/{encoded}{query}{fragment}")
}

fn scheme_match(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b':' {
            return true;
        }
        if !(byte.is_ascii_alphanumeric() || *byte == b'+' || *byte == b'-' || *byte == b'.') {
            return false;
        }
        let _ = index;
    }
    false
}

fn percent_encode_path(path: &str) -> String {
    let mut result = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
            result.push(byte as char);
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    result
}

/// Normalize markdown changelog links to version-tagged GitHub URLs (JS
/// `normalizeChangelogLinks`).
pub fn normalize_changelog_links(markdown: &str, version: &str) -> String {
    let tag = normalize_tag(version);
    let mut result = String::new();
    let mut rest = markdown;
    loop {
        let Some(open) = find_markdown_link(rest) else {
            result.push_str(rest);
            break;
        };
        let (prefix, target, suffix, consumed) = open;
        result.push_str(&rest[..consumed]);
        result.push_str(&prefix);
        result.push_str(&normalize_changelog_link_target(&target, &tag));
        result.push_str(&suffix);
        rest = &rest[consumed..];
    }
    result
}

/// Find the next inline markdown link `([...](target ...))`; returns the
/// prefix, target, suffix, and consumed offset (JS regex match).
fn find_markdown_link(text: &str) -> Option<(String, String, String, usize)> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'!' && index + 1 < bytes.len() && bytes[index + 1] == b'[' {
            // Skip image alt text.
            index += 2;
            while index < bytes.len() && bytes[index] != b']' {
                index += 1;
            }
            if index >= bytes.len() {
                return None;
            }
            index += 1;
            continue;
        }
        if bytes[index] == b'[' {
            // Find closing ] and following (.
            let mut close = index + 1;
            while close < bytes.len() && bytes[close] != b']' {
                if bytes[close] == b'\n' {
                    break;
                }
                close += 1;
            }
            if close >= bytes.len() || bytes[close] != b']' || close + 1 >= bytes.len() || bytes[close + 1] != b'(' {
                index += 1;
                continue;
            }
            let target_start = close + 2;
            let mut target_end = target_start;
            while target_end < bytes.len() && bytes[target_end] != b')' && bytes[target_end] != b' ' && bytes[target_end] != b'\n' {
                target_end += 1;
            }
            if target_end >= bytes.len() || target_end == target_start {
                index += 1;
                continue;
            }
            let target = text[target_start..target_end].to_string();
            // Find the matching closing paren allowing optional title.
            let mut paren_end = target_end;
            while paren_end < bytes.len() && bytes[paren_end] != b')' {
                if bytes[paren_end] == b'\n' {
                    return None;
                }
                paren_end += 1;
            }
            if paren_end >= bytes.len() {
                index += 1;
                continue;
            }
            let prefix = text[index..close + 1].to_string();
            let suffix = text[paren_end..paren_end + 1].to_string();
            return Some((prefix, target, suffix, paren_end + 1));
        }
        index += 1;
    }
    None
}

/// Parse changelog entries from a CHANGELOG.md path (JS `parseChangelog`).
pub fn parse_changelog(changelog_path: &str) -> Vec<ChangelogEntry> {
    let content = match std::fs::read_to_string(changelog_path) {
        Ok(content) => content,
        Err(_) => return vec![],
    };
    let lines: Vec<&str> = content.split('\n').collect();
    let mut entries: Vec<ChangelogEntry> = Vec::new();
    let mut current_lines: Vec<String> = Vec::new();
    let mut current_version: Option<(i64, i64, i64)> = None;

    for line in lines {
        if line.starts_with("## ") {
            if let Some((major, minor, patch)) = current_version {
                if !current_lines.is_empty() {
                    entries.push(ChangelogEntry {
                        major,
                        minor,
                        patch,
                        content: current_lines.join("\n").trim().to_string(),
                    });
                }
            }
            let version_match = parse_version_header(line);
            match version_match {
                Some(version) => {
                    current_version = Some(version);
                    current_lines = vec![line.to_string()];
                }
                None => {
                    current_version = None;
                    current_lines = Vec::new();
                }
            }
        } else if current_version.is_some() {
            current_lines.push(line.to_string());
        }
    }
    if let Some((major, minor, patch)) = current_version {
        if !current_lines.is_empty() {
            entries.push(ChangelogEntry {
                major,
                minor,
                patch,
                content: current_lines.join("\n").trim().to_string(),
            });
        }
    }
    entries
}

fn parse_version_header(line: &str) -> Option<(i64, i64, i64)> {
    let after_h2 = line[3..].trim_start();
    let after_bracket = after_h2.strip_prefix('[').unwrap_or(after_h2);
    let end = after_bracket.find(|ch: char| !ch.is_ascii_digit() && ch != '.')?;
    let version = &after_bracket[..end];
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() < 3 {
        return None;
    }
    Some((
        parts[0].parse().unwrap_or(0),
        parts[1].parse().unwrap_or(0),
        parts[2].parse().unwrap_or(0),
    ))
}

/// Compare versions: -1/0/1 (JS `compareVersions`).
pub fn compare_versions(v1: &ChangelogEntry, v2: &ChangelogEntry) -> i64 {
    if v1.major != v2.major {
        return v1.major - v2.major;
    }
    if v1.minor != v2.minor {
        return v1.minor - v2.minor;
    }
    v1.patch - v2.patch
}

/// Entries newer than last_version (JS `getNewEntries`).
pub fn get_new_entries(entries: &[ChangelogEntry], last_version: &str) -> Vec<ChangelogEntry> {
    let parts: Vec<i64> = last_version.split('.').map(|part| part.parse().unwrap_or(0)).collect();
    let last = ChangelogEntry {
        major: parts.first().copied().unwrap_or(0),
        minor: parts.get(1).copied().unwrap_or(0),
        patch: parts.get(2).copied().unwrap_or(0),
        content: String::new(),
    };
    entries
        .iter()
        .filter(|entry| compare_versions(entry, &last) > 0)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_urls() {
        let source = parse_git_url("git:github.com/user/repo").unwrap();
        assert_eq!(source.repo, "https://github.com/user/repo");
        assert_eq!(source.host, "github.com");
        assert_eq!(source.path, "user/repo");
        assert!(!source.pinned);

        // Scp-like URLs require the git: prefix (JS rejects bare git@).
        assert!(parse_git_url("git@github.com:user/repo.git").is_none());
        let source = parse_git_url("git:git@github.com:user/repo.git").unwrap();
        assert_eq!(source.host, "github.com");
        assert_eq!(source.path, "user/repo");

        let source = parse_git_url("https://github.com/user/repo#v1.2.3").unwrap();
        assert_eq!(source.ref_.as_deref(), Some("v1.2.3"));
        assert!(source.pinned);

        assert!(parse_git_url("plain/path").is_none());
        assert!(parse_git_url("npm:pkg").is_none());
    }

    #[test]
    fn unsafe_git_urls_rejected() {
        assert!(parse_git_url("git:github.com/../escape").is_none());
        assert!(parse_git_url("git:host/path").is_none()); // no dot, not localhost
        // JS also returns null: generic parsing requires >= 2 path parts.
        assert!(parse_git_url("git:localhost/repo").is_none());
    }

    #[test]
    fn changelog_links_normalized() {
        let markdown = "See [CHANGELOG](CHANGELOG.md#v1.2.0) and [src](src/)";
        let normalized = normalize_changelog_links(markdown, "1.2.3");
        assert!(normalized.contains("https://github.com/earendil-works/pi/blob/v1.2.3/packages/coding-agent/CHANGELOG.md#v1.2.0"));
        assert!(normalized.contains("https://github.com/earendil-works/pi/tree/v1.2.3/packages/coding-agent/src"));

        // External links untouched.
        let normalized = normalize_changelog_links("[npm](https://npmjs.com/pkg)", "1.0.0");
        assert!(normalized.contains("https://npmjs.com/pkg"));
    }

    #[test]
    fn parses_changelog_entries() {
        let path = std::env::temp_dir().join(format!("changelog-{}.md", std::process::id()));
        std::fs::write(&path, "## [0.3.0] - 2024\n- a\n\n## [0.2.0]\n- b\n\n## [Unreleased]\n- c\n").unwrap();
        let entries = parse_changelog(&path.to_string_lossy());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].major, 0);
        assert_eq!(entries[0].minor, 3);
        assert_eq!(entries[0].patch, 0);
        assert!(entries[0].content.contains("- a"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn compares_versions() {
        let v1 = ChangelogEntry {
            major: 1,
            minor: 2,
            patch: 3,
            content: String::new(),
        };
        let v2 = ChangelogEntry {
            major: 1,
            minor: 2,
            patch: 4,
            content: String::new(),
        };
        assert!(compare_versions(&v1, &v2) < 0);
        assert!(compare_versions(&v2, &v1) > 0);
        let newer = get_new_entries(&[v1.clone(), v2.clone()], "1.2.3");
        assert_eq!(newer.len(), 1);
        assert_eq!(newer[0].patch, 4);
    }
}
