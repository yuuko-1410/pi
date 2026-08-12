//! Package manager, port of
//! `packages/coding-agent/src/core/package-manager.ts` (core logic: source
//! parsing, resource collection with gitignore rules, pattern filtering,
//! and resolution with precedence. npm/git install commands are wrapped
//! via spawn; glob/minimatch/ignore/semver are built-in subsets).

use std::collections::HashSet;
use std::path::Path;

use crate::utils::child_process::{resolve_path, PathInputOptions};
use crate::utils::git_changelog::parse_git_url;
use crate::utils::version_check::compare_package_versions;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct PathMetadata {
    pub source: String,
    pub scope: String,
    pub origin: String,
    pub base_dir: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedResource {
    pub path: String,
    pub enabled: bool,
    pub metadata: PathMetadata,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolvedPaths {
    pub extensions: Vec<ResolvedResource>,
    pub skills: Vec<ResolvedResource>,
    pub prompts: Vec<ResolvedResource>,
    pub themes: Vec<ResolvedResource>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParsedSource {
    Npm {
        spec: String,
        name: String,
        version: Option<String>,
        pinned: bool,
    },
    Git {
        repo: String,
        host: String,
        path: String,
        ref_: Option<String>,
        pinned: bool,
    },
    Local {
        path: String,
    },
}

pub const RESOURCE_TYPES: [&str; 4] = ["extensions", "skills", "prompts", "themes"];

fn is_local_path(value: &str) -> bool {
    let trimmed = value.trim();
    !(trimmed.starts_with("npm:")
        || trimmed.starts_with("git:")
        || trimmed.starts_with("github:")
        || trimmed.starts_with("http:")
        || trimmed.starts_with("https:")
        || trimmed.starts_with("ssh:"))
}

/// Parse an npm spec into name and version (JS `parseNpmSpec`).
pub fn parse_npm_spec(spec: &str) -> (String, Option<String>) {
    // ^(@?[^@]+(?:\/[^@]+)?)(?:@(.+))?$
    let scoped = spec.strip_prefix('@');
    let (name, version) = match scoped {
        Some(rest) => {
            // @scope/name or @scope/name@version
            if let Some((name, version)) = rest.split_once('@') {
                (format!("@{name}"), Some(version.to_string()))
            } else {
                (spec.to_string(), None)
            }
        }
        None => match spec.split_once('@') {
            Some((name, version)) if !name.is_empty() => (name.to_string(), Some(version.to_string())),
            _ => (spec.to_string(), None),
        },
    };
    if name.is_empty() {
        (spec.to_string(), None)
    } else {
        (name, version)
    }
}

fn is_exact_npm_version(version: Option<&str>) -> bool {
    version.map(|version| compare_package_versions(version, "0.0.0").is_some()).unwrap_or(false)
}

/// Parse a package source (JS `parseSource`).
pub fn parse_source(source: &str) -> ParsedSource {
    if let Some(rest) = source.strip_prefix("npm:") {
        let spec = rest.trim().to_string();
        let (name, version) = parse_npm_spec(&spec);
        let pinned = is_exact_npm_version(version.as_deref());
        return ParsedSource::Npm {
            spec,
            name,
            version,
            pinned,
        };
    }
    if is_local_path(source) {
        return ParsedSource::Local {
            path: source.to_string(),
        };
    }
    if let Some(git) = parse_git_url(source) {
        return ParsedSource::Git {
            repo: git.repo,
            host: git.host,
            path: git.path,
            ref_: git.ref_,
            pinned: git.pinned,
        };
    }
    ParsedSource::Local {
        path: source.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Glob (minimatch subset) and ignore (gitignore subset)
// ---------------------------------------------------------------------------

fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    fn match_at(pattern: &[char], text: &[char]) -> bool {
        match (pattern.first(), text.first()) {
            (None, None) => true,
            (Some('*'), _) => (0..=text.len()).any(|skip| match_at(&pattern[1..], &text[skip..])),
            (Some('?'), Some(_)) => match_at(&pattern[1..], &text[1..]),
            (Some('['), _) => {
                // Character class subset: [abc], [a-z].
                let mut index = 1;
                let mut negated = false;
                if pattern.get(index) == Some(&'^') || pattern.get(index) == Some(&'!') {
                    negated = true;
                    index += 1;
                }
                let mut matched = false;
                let mut has_range = false;
                while index < pattern.len() && pattern[index] != ']' {
                    if index + 2 < pattern.len() && pattern[index + 1] == '-' && pattern[index + 2] != ']' {
                        let start = pattern[index] as u32;
                        let end = pattern[index + 2] as u32;
                        if let Some(text_char) = text.first() {
                            let value = *text_char as u32;
                            if value >= start && value <= end {
                                matched = true;
                            }
                        }
                        has_range = true;
                        index += 3;
                        continue;
                    }
                    if text.first() == pattern.get(index) {
                        matched = true;
                    }
                    index += 1;
                }
                let _ = has_range;
                if index >= pattern.len() {
                    return false;
                }
                if matched == negated {
                    return false;
                }
                match_at(&pattern[index + 1..], &text[1..])
            }
            (Some(p), Some(t)) if p == t => match_at(&pattern[1..], &text[1..]),
            _ => false,
        }
    }
    match_at(&pattern, &text)
}

fn to_posix_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn relative_path(base: &str, path: &str) -> String {
    let base = Path::new(base);
    let path = Path::new(path);
    path.strip_prefix(base)
        .map(|relative| to_posix_path(&relative.to_string_lossy()))
        .unwrap_or_else(|_| to_posix_path(&path.to_string_lossy()))
}



/// Simplified gitignore matcher (comments, negation, directory rules, star/
/// question globs; `**` crosses separators via `*`).
pub struct IgnoreMatcher {
    rules: Vec<(bool, bool, String)>, // (negated, directory_only, pattern)
}

impl IgnoreMatcher {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn add(&mut self, patterns: Vec<String>) {
        for pattern in patterns {
            let mut rule = pattern;
            let mut negated = false;
            if let Some(rest) = rule.strip_prefix('!') {
                negated = true;
                rule = rest.to_string();
            }
            let directory_only = rule.ends_with('/');
            if directory_only {
                rule.pop();
            }
            self.rules.push((negated, directory_only, rule));
        }
    }

    pub fn ignores(&self, path: &str) -> bool {
        let path = path.trim_end_matches('/');
        let mut ignored = false;
        for (negated, directory_only, pattern) in &self.rules {
            let matched = if *directory_only {
                path == pattern
                    || path.starts_with(&format!("{pattern}/"))
                    || path.split('/').any(|component| component == pattern)
            } else {
                glob_match(pattern, path) || path.split('/').any(|component| glob_match(pattern, component))
            };
            if matched {
                ignored = !negated;
            }
        }
        ignored
    }
}

impl Default for IgnoreMatcher {
    fn default() -> Self {
        Self::new()
    }
}

const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".fdignore"];

fn prefix_ignore_pattern(line: &str, prefix: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("\\#") {
        return None;
    }
    let mut pattern = line.to_string();
    let mut negated = false;
    if pattern.starts_with('!') {
        negated = true;
        pattern = pattern[1..].to_string();
    } else if pattern.starts_with("\\!") {
        pattern = pattern[1..].to_string();
    }
    if pattern.starts_with('/') {
        pattern = pattern[1..].to_string();
    }
    let prefixed = format!("{prefix}{pattern}");
    Some(if negated { format!("!{prefixed}") } else { prefixed })
}

fn add_ignore_rules(ignore_matcher: &mut IgnoreMatcher, dir: &str, root_dir: &str) {
    let relative_dir = relative_path(root_dir, dir);
    let prefix = if relative_dir.is_empty() {
        String::new()
    } else {
        format!("{relative_dir}/")
    };
    for filename in IGNORE_FILE_NAMES {
        let ignore_path = Path::new(dir).join(filename);
        if !ignore_path.exists() {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&ignore_path) {
            let patterns: Vec<String> = content
                .split(|c| c == '\r' || c == '\n')
                .filter_map(|line| prefix_ignore_pattern(line, &prefix))
                .collect();
            if !patterns.is_empty() {
                ignore_matcher.add(patterns);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Resource collection
// ---------------------------------------------------------------------------

fn file_type_of(path: &Path) -> (bool, bool) {
    let metadata = std::fs::metadata(path);
    match metadata {
        Ok(metadata) => (metadata.is_file(), metadata.is_dir()),
        Err(_) => (path.is_file(), path.is_dir()),
    }
}

fn collect_files(dir: &str, extension_suffix: &str, ignore_matcher: Option<&mut IgnoreMatcher>, root_dir: &str) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    if !Path::new(dir).exists() {
        return files;
    }
    let root = if root_dir.is_empty() { dir.to_string() } else { root_dir.to_string() };
    let mut owned_ignore = IgnoreMatcher::new();
    let ig: &mut IgnoreMatcher = match ignore_matcher {
        Some(matcher) => matcher,
        None => &mut owned_ignore,
    };
    add_ignore_rules(ig, dir, &root);

    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if name == "node_modules" {
            continue;
        }
        let full_path = entry.path();
        let (is_file, is_dir) = file_type_of(&full_path);
        let rel_path = relative_path(&root, &full_path.to_string_lossy());
        let ignore_path = if is_dir { format!("{rel_path}/") } else { rel_path.clone() };
        if ig.ignores(&ignore_path) {
            continue;
        }
        if is_dir {
            files.extend(collect_files(&full_path.to_string_lossy(), extension_suffix, Some(ig), &root));
        } else if is_file && name.ends_with(extension_suffix) {
            files.push(full_path.to_string_lossy().to_string());
        }
    }
    files
}

/// Collect skill entries (SKILL.md at dir root or recursive; JS
/// `collectSkillEntries`).
fn collect_skill_entries(dir: &str, root_dir: &str) -> Vec<String> {
    let mut entries: Vec<String> = Vec::new();
    if !Path::new(dir).exists() {
        return entries;
    }
    let root = if root_dir.is_empty() { dir.to_string() } else { root_dir.to_string() };
    let mut ig = IgnoreMatcher::new();
    add_ignore_rules(&mut ig, dir, &root);

    let Ok(dir_entries) = std::fs::read_dir(dir) else {
        return entries;
    };
    let listed: Vec<_> = dir_entries.flatten().collect();

    for entry in &listed {
        if entry.file_name().to_string_lossy() != "SKILL.md" {
            continue;
        }
        let full_path = entry.path();
        let (is_file, _) = file_type_of(&full_path);
        let rel_path = relative_path(&root, &full_path.to_string_lossy());
        if is_file && !ig.ignores(&rel_path) {
            entries.push(full_path.to_string_lossy().to_string());
            return entries;
        }
    }

    for entry in listed {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let full_path = entry.path();
        let (is_file, is_dir) = file_type_of(&full_path);
        let rel_path = relative_path(&root, &full_path.to_string_lossy());
        if dir == root && is_file && name.ends_with(".md") && !ig.ignores(&rel_path) {
            entries.push(full_path.to_string_lossy().to_string());
            continue;
        }
        if !is_dir {
            continue;
        }
        if ig.ignores(&format!("{rel_path}/")) {
            continue;
        }
        entries.extend(collect_skill_entries(&full_path.to_string_lossy(), &root));
    }
    entries
}

/// Collect resource files from a directory by resource type (JS
/// `collectResourceFiles`).
pub fn collect_resource_files(dir: &str, resource_type: &str) -> Vec<String> {
    match resource_type {
        "skills" => collect_skill_entries(dir, ""),
        "extensions" => collect_auto_extension_entries(dir),
        "prompts" => collect_files(dir, ".md", None, ""),
        "themes" => collect_files(dir, ".json", None, ""),
        _ => Vec::new(),
    }
}

fn resolve_extension_entries(dir: &str) -> Option<Vec<String>> {
    let package_json_path = format!("{dir}/package.json");
    if Path::new(&package_json_path).exists() {
        if let Some(manifest) = crate::core::pi_manifest::read_pi_manifest(&package_json_path) {
            if let Some(extensions) = manifest.extensions {
                let mut entries: Vec<String> = Vec::new();
                for ext_path in extensions {
                    let resolved = Path::new(dir).join(&ext_path);
                    if resolved.exists() {
                        entries.push(resolved.to_string_lossy().to_string());
                    }
                }
                if !entries.is_empty() {
                    return Some(entries);
                }
            }
        }
    }
    let index_ts = format!("{dir}/index.ts");
    let index_js = format!("{dir}/index.js");
    if Path::new(&index_ts).exists() {
        return Some(vec![index_ts]);
    }
    if Path::new(&index_js).exists() {
        return Some(vec![index_js]);
    }
    None
}

/// Collect auto-discovered extension entries (JS
/// `collectAutoExtensionEntries`).
pub fn collect_auto_extension_entries(dir: &str) -> Vec<String> {
    if !Path::new(dir).exists() {
        return vec![];
    }
    if let Some(entries) = resolve_extension_entries(dir) {
        return entries;
    }
    let mut entries: Vec<String> = Vec::new();
    let mut ig = IgnoreMatcher::new();
    add_ignore_rules(&mut ig, dir, dir);
    let Ok(dir_entries) = std::fs::read_dir(dir) else {
        return entries;
    };
    for entry in dir_entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let full_path = entry.path();
        let (is_file, is_dir) = file_type_of(&full_path);
        let rel_path = relative_path(dir, &full_path.to_string_lossy());
        let ignore_path = if is_dir { format!("{rel_path}/") } else { rel_path };
        if ig.ignores(&ignore_path) {
            continue;
        }
        if is_file && (name.ends_with(".ts") || name.ends_with(".js")) {
            entries.push(full_path.to_string_lossy().to_string());
        } else if is_dir {
            if let Some(resolved) = resolve_extension_entries(&full_path.to_string_lossy()) {
                entries.extend(resolved);
            }
        }
    }
    entries
}

// ---------------------------------------------------------------------------
// Pattern filtering
// ---------------------------------------------------------------------------





fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn dirname(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn matches_any_pattern(file_path: &str, patterns: &[String], base_dir: &str) -> bool {
    let rel = relative_path(base_dir, file_path);
    let name = basename(file_path);
    let file_path_posix = to_posix_path(file_path);
    let is_skill_file = name == "SKILL.md";
    let parent_dir = if is_skill_file { Some(dirname(file_path)) } else { None };
    let parent_rel = parent_dir.as_deref().map(|dir| relative_path(base_dir, dir));
    let parent_name = parent_dir.as_deref().map(|dir| basename(dir));
    let parent_dir_posix = parent_dir.as_deref().map(|dir| to_posix_path(dir));

    patterns.iter().any(|pattern| {
        let normalized = to_posix_path(pattern);
        if glob_match(&normalized, &rel)
            || glob_match(&normalized, &name)
            || glob_match(&normalized, &file_path_posix)
        {
            return true;
        }
        if !is_skill_file {
            return false;
        }
        glob_match(&normalized, parent_rel.as_deref().unwrap_or(""))
            || glob_match(&normalized, parent_name.as_deref().unwrap_or(""))
            || glob_match(&normalized, parent_dir_posix.as_deref().unwrap_or(""))
    })
}

fn normalize_exact_pattern(pattern: &str) -> String {
    let normalized = pattern
        .strip_prefix("./")
        .or_else(|| pattern.strip_prefix(".\\"))
        .unwrap_or(pattern);
    to_posix_path(normalized)
}

fn matches_any_exact_pattern(file_path: &str, patterns: &[String], base_dir: &str) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let rel = relative_path(base_dir, file_path);
    let file_path_posix = to_posix_path(file_path);
    let is_skill_file = basename(file_path) == "SKILL.md";
    let parent_dir = if is_skill_file { Some(dirname(file_path)) } else { None };
    let parent_rel = parent_dir.as_deref().map(|dir| relative_path(base_dir, dir));
    let parent_dir_posix = parent_dir.as_deref().map(|dir| to_posix_path(dir));

    patterns.iter().any(|pattern| {
        let normalized = normalize_exact_pattern(pattern);
        if normalized == rel || normalized == file_path_posix {
            return true;
        }
        if !is_skill_file {
            return false;
        }
        normalized == parent_rel.as_deref().unwrap_or("") || normalized == parent_dir_posix.as_deref().unwrap_or("")
    })
}

/// Apply include/exclude/force patterns to paths (JS `applyPatterns`).
pub fn apply_patterns(all_paths: &[String], patterns: &[String], base_dir: &str) -> HashSet<String> {
    let mut includes: Vec<String> = Vec::new();
    let mut excludes: Vec<String> = Vec::new();
    let mut force_includes: Vec<String> = Vec::new();
    let mut force_excludes: Vec<String> = Vec::new();

    for pattern in patterns {
        if let Some(rest) = pattern.strip_prefix('+') {
            force_includes.push(rest.to_string());
        } else if let Some(rest) = pattern.strip_prefix('-') {
            force_excludes.push(rest.to_string());
        } else if let Some(rest) = pattern.strip_prefix('!') {
            excludes.push(rest.to_string());
        } else {
            includes.push(pattern.clone());
        }
    }

    let mut result: Vec<String> = if includes.is_empty() {
        all_paths.to_vec()
    } else {
        all_paths
            .iter()
            .filter(|file_path| matches_any_pattern(file_path, &includes, base_dir))
            .cloned()
            .collect()
    };

    if !excludes.is_empty() {
        result.retain(|file_path| !matches_any_pattern(file_path, &excludes, base_dir));
    }

    if !force_includes.is_empty() {
        for file_path in all_paths {
            if !result.contains(file_path) && matches_any_exact_pattern(file_path, &force_includes, base_dir) {
                result.push(file_path.clone());
            }
        }
    }

    if !force_excludes.is_empty() {
        result.retain(|file_path| !matches_any_exact_pattern(file_path, &force_excludes, base_dir));
    }

    result.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Path metadata and precedence
// ---------------------------------------------------------------------------

/// Compute precedence rank (lower = higher precedence; JS
/// `resourcePrecedenceRank`).
pub fn resource_precedence_rank(metadata: &PathMetadata) -> u8 {
    if metadata.origin == "package" {
        return 4;
    }
    let scope_base: u8 = if metadata.scope == "project" { 0 } else { 2 };
    let source_offset: u8 = if metadata.source == "local" { 0 } else { 1 };
    scope_base + source_offset
}

/// Resolve a local entry path against a base directory (JS
/// `resolvePathFromBase`): tilde/relative handling with cloud-sync ignore.
pub fn resolve_path_from_base(input: &str, base_dir: &str) -> String {
    let resolved = resolve_path(input, base_dir, &PathInputOptions {
        expand_tilde: true,
        ..PathInputOptions::default()
    });
    resolved
}

/// Get the extension temp folder (JS `getExtensionTempFolder`).
pub fn get_extension_temp_folder(agent_dir: &str) -> String {
    let temp_folder = Path::new(agent_dir).join("tmp").join("extensions");
    let _ = std::fs::create_dir_all(&temp_folder);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp_folder, std::fs::Permissions::from_mode(0o700));
    }
    temp_folder.to_string_lossy().to_string()
}

/// Find the git repository root for a directory (JS `findGitRepoRoot`).
pub fn find_git_repo_root(start_dir: &str) -> Option<String> {
    let mut dir = resolve_path(start_dir, start_dir, &PathInputOptions::default());
    loop {
        if Path::new(&dir).join(".git").exists() {
            return Some(dir);
        }
        let parent = Path::new(&dir)
            .parent()
            .map(|parent| parent.to_string_lossy().to_string());
        match parent {
            Some(parent) if parent == dir => return None,
            Some(parent) => dir = parent,
            None => return None,
        }
    }
}

/// Collect ancestor .agents/skills directories (JS
/// `collectAncestorAgentsSkillDirs`).
pub fn collect_ancestor_agents_skill_dirs(start_dir: &str) -> Vec<String> {
    let mut skill_dirs: Vec<String> = Vec::new();
    let resolved = resolve_path(start_dir, start_dir, &PathInputOptions::default());
    let git_repo_root = find_git_repo_root(&resolved);
    let mut dir = resolved;
    loop {
        skill_dirs.push(Path::new(&dir).join(".agents").join("skills").to_string_lossy().to_string());
        if let Some(git_repo_root) = &git_repo_root {
            if dir == *git_repo_root {
                break;
            }
        }
        let parent = Path::new(&dir).parent().map(|parent| parent.to_string_lossy().to_string());
        match parent {
            Some(parent) if parent == dir => break,
            Some(parent) => dir = parent,
            None => break,
        }
    }
    skill_dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sources() {
        let source = parse_source("npm:lodash@4.17.21");
        match source {
            ParsedSource::Npm { name, version, pinned, .. } => {
                assert_eq!(name, "lodash");
                assert_eq!(version.as_deref(), Some("4.17.21"));
                assert!(pinned);
            }
            _ => panic!("expected npm"),
        }
        let source = parse_source("npm:@scope/pkg@^2.0.0");
        match source {
            ParsedSource::Npm { name, version, .. } => {
                assert_eq!(name, "@scope/pkg");
                assert_eq!(version.as_deref(), Some("^2.0.0"));
            }
            _ => panic!("expected npm"),
        }
        assert!(matches!(parse_source("./local/dir"), ParsedSource::Local { .. }));
        assert!(matches!(parse_source("git:github.com/user/repo"), ParsedSource::Git { .. }));
        assert!(matches!(parse_source("https://github.com/user/repo"), ParsedSource::Git { .. }));
    }

    #[test]
    fn npm_spec_parsing() {
        assert_eq!(parse_npm_spec("lodash"), ("lodash".to_string(), None));
        assert_eq!(parse_npm_spec("lodash@1.2.3"), ("lodash".to_string(), Some("1.2.3".to_string())));
        assert_eq!(parse_npm_spec("@scope/pkg"), ("@scope/pkg".to_string(), None));
        assert_eq!(parse_npm_spec("@scope/pkg@2.0.0"), ("@scope/pkg".to_string(), Some("2.0.0".to_string())));
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("*.md", "readme.md"));
        assert!(glob_match("src/**", "src/a/b.ts"));
        assert!(glob_match("?at", "cat"));
        assert!(!glob_match("*.md", "readme.txt"));
        assert!(glob_match("[abc].ts", "a.ts"));
        assert!(glob_match("[a-c].ts", "b.ts"));
    }

    #[test]
    fn ignore_matcher_rules() {
        let mut matcher = IgnoreMatcher::new();
        matcher.add(vec!["node_modules/".to_string(), "*.log".to_string(), "!keep.log".to_string()]);
        assert!(matcher.ignores("node_modules/pkg/index.js"));
        assert!(matcher.ignores("foo/bar.log"));
        assert!(!matcher.ignores("foo/keep.log"));
        assert!(!matcher.ignores("src/main.ts"));
    }

    #[test]
    fn collects_skill_entries() {
        let dir = std::env::temp_dir().join(format!("pi-skills-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("SKILL.md"), "x").unwrap();
        std::fs::write(dir.join("sub/SKILL.md"), "y").unwrap();
        let entries = collect_resource_files(&dir.to_string_lossy(), "skills");
        assert_eq!(entries.len(), 1); // root SKILL.md short-circuits
        assert!(entries[0].ends_with("SKILL.md"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pattern_filtering() {
        let paths = vec!["/tmp/a.md".to_string(), "/tmp/b.md".to_string(), "/tmp/c.json".to_string()];
        let enabled = apply_patterns(&paths, &["*.md".to_string()], "/tmp");
        assert_eq!(enabled.len(), 2);
        let enabled = apply_patterns(&paths, &["!a.md".to_string()], "/tmp");
        assert_eq!(enabled.len(), 2);
        assert!(!enabled.contains("/tmp/a.md"));
        // Force include beats exclusion.
        let enabled = apply_patterns(&paths, &["!*.md".to_string(), "+a.md".to_string()], "/tmp");
        assert!(enabled.contains("/tmp/a.md"));
    }

    #[test]
    fn precedence_ranking() {
        let project_local = PathMetadata {
            source: "local".to_string(),
            scope: "project".to_string(),
            origin: "top-level".to_string(),
            base_dir: None,
        };
        let user_auto = PathMetadata {
            source: "auto".to_string(),
            scope: "user".to_string(),
            origin: "top-level".to_string(),
            base_dir: None,
        };
        let package = PathMetadata {
            source: "pkg".to_string(),
            scope: "user".to_string(),
            origin: "package".to_string(),
            base_dir: None,
        };
        assert!(resource_precedence_rank(&project_local) < resource_precedence_rank(&user_auto));
        assert!(resource_precedence_rank(&user_auto) < resource_precedence_rank(&package));
    }

    #[test]
    fn extension_temp_folder_created() {
        let dir = std::env::temp_dir().join(format!("pi-ext-tmp-{}", std::process::id()));
        let folder = get_extension_temp_folder(&dir.to_string_lossy());
        assert!(Path::new(&folder).exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
