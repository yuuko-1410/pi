//! Skill loading, port of `packages/agent/src/harness/skills.ts`.
//!
//! Differences from JS (documented):
//! - `ignore` npm package → simplified gitignore matcher (comments, blank
//!   lines, `!` negation, directory trailing `/`, `*`/`?` globs; no `**`
//!   segment semantics beyond `*` crossing separators).
//! - `yaml` frontmatter parse → subset parser for scalar values (strings
//!   with quotes, booleans, numbers, `key: value` lines; no nested YAML).
//! - `ExecutionEnv`/`Result`/`FileError` from harness/types.ts are
//!   represented by the minimal synchronous `SkillEnv` trait here; the full
//!   env types are ported with env/nodejs.ts.

/// Stable skill name used for lookup and model-visible listings.
#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub content: String,
    pub file_path: String,
    pub disable_model_invocation: Option<bool>,
}

/// Minimal environment surface used by skill loading.
pub trait SkillEnv {
    fn file_info(&self, path: &str) -> Result<SkillFileInfo, String>;
    fn list_dir(&self, path: &str) -> Result<Vec<SkillFileInfo>, String>;
    fn read_text_file(&self, path: &str) -> Result<String, String>;
    fn join_path(&self, parts: &[&str]) -> Result<String, String>;
    fn canonical_path(&self, path: &str) -> Result<String, String>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct SkillFileInfo {
    pub name: String,
    pub path: String,
    pub kind: SkillFileKind,
    pub size: f64,
    pub mtime_ms: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SkillFileKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// Warning produced while loading skills.
#[derive(Clone, Debug, PartialEq)]
pub struct SkillDiagnostic {
    pub kind: String, // "warning"
    pub code: String,
    pub message: String,
    pub path: String,
}

const MAX_NAME_LENGTH: usize = 64;
const MAX_DESCRIPTION_LENGTH: usize = 1024;
const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".fdignore"];

/// Format a skill invocation prompt, optionally appending additional user
/// instructions.
pub fn format_skill_invocation(skill: &Skill, additional_instructions: Option<&str>) -> String {
    let skill_block = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        skill.name,
        skill.file_path,
        dirname_env_path(&skill.file_path),
        skill.content
    );
    match additional_instructions {
        Some(instructions) => format!("{skill_block}\n\n{instructions}"),
        None => skill_block,
    }
}

/// Load skills from one or more directories. Missing input directories are
/// skipped.
pub fn load_skills(env: &dyn SkillEnv, dirs: &[String]) -> (Vec<Skill>, Vec<SkillDiagnostic>) {
    let mut skills: Vec<Skill> = Vec::new();
    let mut diagnostics: Vec<SkillDiagnostic> = Vec::new();
    for dir in dirs {
        let root_info = match env.file_info(dir) {
            Ok(info) => info,
            Err(error) => {
                if error != "not_found" {
                    diagnostics.push(diagnostic("file_info_failed", &error, dir));
                }
                continue;
            }
        };
        if resolve_kind(env, &root_info, &mut diagnostics) != Some(SkillFileKind::Directory) {
            continue;
        }
        let mut ignore_matcher = IgnoreMatcher::new();
        let result = load_skills_from_dir_internal(env, &root_info.path, true, &mut ignore_matcher, &root_info.path);
        skills.extend(result.0);
        diagnostics.extend(result.1);
    }
    (skills, diagnostics)
}

fn diagnostic(code: &str, message: &str, path: &str) -> SkillDiagnostic {
    SkillDiagnostic {
        kind: "warning".to_string(),
        code: code.to_string(),
        message: message.to_string(),
        path: path.to_string(),
    }
}

fn load_skills_from_dir_internal(
    env: &dyn SkillEnv,
    dir: &str,
    include_root_files: bool,
    ignore_matcher: &mut IgnoreMatcher,
    root_dir: &str,
) -> (Vec<Skill>, Vec<SkillDiagnostic>) {
    let mut skills: Vec<Skill> = Vec::new();
    let mut diagnostics: Vec<SkillDiagnostic> = Vec::new();

    let dir_info = match env.file_info(dir) {
        Ok(info) => info,
        Err(error) => {
            if error != "not_found" {
                diagnostics.push(diagnostic("file_info_failed", &error, dir));
            }
            return (skills, diagnostics);
        }
    };
    if resolve_kind(env, &dir_info, &mut diagnostics) != Some(SkillFileKind::Directory) {
        return (skills, diagnostics);
    }

    add_ignore_rules(env, ignore_matcher, dir, root_dir, &mut diagnostics);

    let entries = match env.list_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            diagnostics.push(diagnostic("list_failed", &error, dir));
            return (skills, diagnostics);
        }
    };
    // A SKILL.md at the root of this directory is the directory's skill;
    // return after loading it (JS returns after the first SKILL.md).
    for entry in &entries {
        if entry.name != "SKILL.md" {
            continue;
        }
        if resolve_kind(env, entry, &mut diagnostics) != Some(SkillFileKind::File) {
            continue;
        }
        let rel_path = relative_env_path(root_dir, &entry.path);
        if ignore_matcher.ignores(&rel_path) {
            continue;
        }
        let result = load_skill_from_file(env, &entry.path, &dir_info.name);
        if let Some(skill) = result.0 {
            skills.push(skill);
        }
        diagnostics.extend(result.1);
        return (skills, diagnostics);
    }

    let mut sorted = entries;
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for entry in sorted {
        if entry.name.starts_with('.') || entry.name == "node_modules" {
            continue;
        }
        let kind = resolve_kind(env, &entry, &mut diagnostics);
        let Some(kind) = kind else { continue };

        let rel_path = relative_env_path(root_dir, &entry.path);
        let ignore_path = if kind == SkillFileKind::Directory {
            format!("{rel_path}/")
        } else {
            rel_path
        };
        if ignore_matcher.ignores(&ignore_path) {
            continue;
        }

        if kind == SkillFileKind::Directory {
            let result =
                load_skills_from_dir_internal(env, &entry.path, false, ignore_matcher, root_dir);
            skills.extend(result.0);
            diagnostics.extend(result.1);
            continue;
        }

        if kind != SkillFileKind::File || !include_root_files || !entry.name.ends_with(".md") {
            continue;
        }
        let result = load_skill_from_file(env, &entry.path, &dir_info.name);
        if let Some(skill) = result.0 {
            skills.push(skill);
        }
        diagnostics.extend(result.1);
    }

    (skills, diagnostics)
}

fn add_ignore_rules(
    env: &dyn SkillEnv,
    ignore_matcher: &mut IgnoreMatcher,
    dir: &str,
    root_dir: &str,
    diagnostics: &mut Vec<SkillDiagnostic>,
) {
    let relative_dir = relative_env_path(root_dir, dir);
    let prefix = if relative_dir.is_empty() {
        String::new()
    } else {
        format!("{relative_dir}/")
    };

    for filename in IGNORE_FILE_NAMES {
        let ignore_path = match env.join_path(&[dir, filename]) {
            Ok(path) => path,
            Err(error) => {
                diagnostics.push(diagnostic("file_info_failed", &error, dir));
                continue;
            }
        };
        let info = match env.file_info(&ignore_path) {
            Ok(info) => info,
            Err(error) => {
                if error != "not_found" {
                    diagnostics.push(diagnostic("file_info_failed", &error, &ignore_path));
                }
                continue;
            }
        };
        if info.kind != SkillFileKind::File {
            continue;
        }
        let content = match env.read_text_file(&ignore_path) {
            Ok(content) => content,
            Err(error) => {
                diagnostics.push(diagnostic("read_failed", &error, &ignore_path));
                continue;
            }
        };
        let patterns: Vec<String> = content
            .split(|c| c == '\r' || c == '\n')
            .filter_map(|line| prefix_ignore_pattern(line, &prefix))
            .collect();
        if !patterns.is_empty() {
            ignore_matcher.add(patterns);
        }
    }
}

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

fn load_skill_from_file(
    env: &dyn SkillEnv,
    file_path: &str,
    parent_dir_name: &str,
) -> (Option<Skill>, Vec<SkillDiagnostic>) {
    let mut diagnostics: Vec<SkillDiagnostic> = Vec::new();
    let raw_content = match env.read_text_file(file_path) {
        Ok(content) => content,
        Err(error) => {
            diagnostics.push(diagnostic("read_failed", &error, file_path));
            return (None, diagnostics);
        }
    };

    let parsed = parse_frontmatter(&raw_content);
    if let Err(error) = parsed {
        diagnostics.push(diagnostic("parse_failed", &error, file_path));
        return (None, diagnostics);
    }
    let (frontmatter, body) = parsed.unwrap();

    let description = frontmatter
        .get("description")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());

    for error in validate_description(description.as_deref()) {
        diagnostics.push(diagnostic("invalid_metadata", &error, file_path));
    }

    let frontmatter_name = frontmatter.get("name").and_then(|value| value.as_str());
    let name = frontmatter_name.unwrap_or(parent_dir_name).to_string();
    for error in validate_name(&name, parent_dir_name) {
        diagnostics.push(diagnostic("invalid_metadata", &error, file_path));
    }

    let Some(description) = description.filter(|description| !description.trim().is_empty()) else {
        return (None, diagnostics);
    };

    (
        Some(Skill {
            name,
            description,
            content: body,
            file_path: file_path.to_string(),
            disable_model_invocation: frontmatter
                .get("disable-model-invocation")
                .and_then(|value| value.as_bool())
                .copied()
                .filter(|value| *value)
                .map(|_| true),
        }),
        diagnostics,
    )
}

fn validate_name(name: &str, parent_dir_name: &str) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();
    if name != parent_dir_name {
        errors.push(format!(
            "name \"{name}\" does not match parent directory \"{parent_dir_name}\""
        ));
    }
    if name.len() > MAX_NAME_LENGTH {
        errors.push(format!("name exceeds {MAX_NAME_LENGTH} characters ({})", name.len()));
    }
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') || name.is_empty() {
        errors.push("name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)".to_string());
    }
    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".to_string());
    }
    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".to_string());
    }
    errors
}

fn validate_description(description: Option<&str>) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();
    match description {
        None | Some("") => errors.push("description is required".to_string()),
        Some(description) if description.trim().is_empty() => {
            errors.push("description is required".to_string());
        }
        Some(description) if description.len() > MAX_DESCRIPTION_LENGTH => {
            errors.push(format!(
                "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({})",
                description.len()
            ));
        }
        _ => {}
    }
    errors
}

/// Scalar YAML value (subset).
#[derive(Clone, Debug, PartialEq)]
pub enum YamlValue {
    String(String),
    Bool(bool),
    Number(f64),
    Null,
}

impl YamlValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            YamlValue::String(value) => Some(value),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<&bool> {
        match self {
            YamlValue::Bool(value) => Some(value),
            _ => None,
        }
    }
}

/// Parse frontmatter. JS uses the full `yaml` package; this subset handles
/// the scalar key-value frontmatter skills actually use (quoted or plain
/// strings, booleans, numbers, comments). `[key: value]` shape preserved.
pub fn parse_frontmatter(content: &str) -> Result<(std::collections::HashMap<String, YamlValue>, String), String> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.starts_with("---") {
        return Ok((std::collections::HashMap::new(), normalized));
    }
    let end_index = normalized.find("\n---").map(|index| index + 1); // position of "\n---"
    let Some(end_index) = end_index else {
        return Ok((std::collections::HashMap::new(), normalized));
    };
    let yaml_string = &normalized[4..end_index];
    let body = normalized[end_index + 4..].trim().to_string();

    let mut frontmatter = std::collections::HashMap::new();
    for line in yaml_string.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(colon) = line.find(':') else {
            return Err(format!("invalid frontmatter line: {line}"));
        };
        let key = line[..colon].trim().to_string();
        let raw = line[colon + 1..].trim();
        if raw.is_empty() {
            continue;
        }
        let value = parse_yaml_scalar(raw);
        frontmatter.insert(key, value);
    }

    Ok((frontmatter, body))
}

fn parse_yaml_scalar(raw: &str) -> YamlValue {
    // Strip comments (naive: not inside quotes).
    let raw = raw.split('#').next().unwrap_or("").trim();
    if raw == "null" || raw == "~" {
        return YamlValue::Null;
    }
    if raw == "true" {
        return YamlValue::Bool(true);
    }
    if raw == "false" {
        return YamlValue::Bool(false);
    }
    if let Ok(number) = raw.parse::<f64>() {
        return YamlValue::Number(number);
    }
    if (raw.starts_with('"') && raw.ends_with('"')) || (raw.starts_with('\'') && raw.ends_with('\'')) {
        return YamlValue::String(raw[1..raw.len() - 1].to_string());
    }
    YamlValue::String(raw.to_string())
}

fn resolve_kind(
    env: &dyn SkillEnv,
    info: &SkillFileInfo,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Option<SkillFileKind> {
    if info.kind == SkillFileKind::File || info.kind == SkillFileKind::Directory {
        return Some(info.kind.clone());
    }
    let canonical_path = match env.canonical_path(&info.path) {
        Ok(path) => path,
        Err(error) => {
            if error != "not_found" {
                diagnostics.push(diagnostic("file_info_failed", &error, &info.path));
            }
            return None;
        }
    };
    let target = match env.file_info(&canonical_path) {
        Ok(info) => info,
        Err(error) => {
            if error != "not_found" {
                diagnostics.push(diagnostic("file_info_failed", &error, &info.path));
            }
            return None;
        }
    };
    match target.kind {
        SkillFileKind::File | SkillFileKind::Directory => Some(target.kind),
        _ => None,
    }
}

fn dirname_env_path(path: &str) -> String {
    let normalized = path.trim_end_matches(['/', '\\']);
    let separator_index = normalized
        .rfind('/')
        .map(|index| index as isize)
        .or_else(|| normalized.rfind('\\').map(|index| index as isize));
    match separator_index {
        Some(2) if normalized.as_bytes().get(1) == Some(&b':') => normalized[..3].to_string(),
        Some(index) if index <= 0 => "/".to_string(),
        Some(index) => normalized[..index as usize].to_string(),
        None => "/".to_string(),
    }
}

fn relative_env_path(root: &str, path: &str) -> String {
    let normalized_root = root.replace('\\', "/").trim_end_matches('/').to_string();
    let normalized_path = path.replace('\\', "/").trim_end_matches('/').to_string();
    if normalized_path == normalized_root {
        return String::new();
    }
    if let Some(rest) = normalized_path.strip_prefix(&format!("{normalized_root}/")) {
        rest.to_string()
    } else {
        normalized_path.trim_start_matches('/').to_string()
    }
}

// ---------------------------------------------------------------------------
// Simplified gitignore matcher (see module docs)
// ---------------------------------------------------------------------------

struct IgnoreRule {
    negated: bool,
    directory_only: bool,
    pattern: String,
}

/// Simplified gitignore matcher.
pub struct IgnoreMatcher {
    rules: Vec<IgnoreRule>,
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
            self.rules.push(IgnoreRule {
                negated,
                directory_only,
                pattern: rule,
            });
        }
    }

    pub fn ignores(&self, path: &str) -> bool {
        let path = path.trim_end_matches('/');
        let mut ignored = false;
        for rule in &self.rules {
            let matched = if rule.directory_only {
                // Directory rule matches the directory itself or anything
                // under it.
                path == rule.pattern
                    || path.starts_with(&format!("{}/", rule.pattern))
                    || matches_glob(&rule.pattern, path)
                    || path
                        .split('/')
                        .any(|component| component == rule.pattern)
            } else {
                // A pattern matches any path component (basename match) or
                // the full path.
                matches_glob(&rule.pattern, path)
                    || path
                        .split('/')
                        .any(|component| matches_glob(&rule.pattern, component))
            };
            if matched {
                ignored = !rule.negated;
            }
        }
        ignored
    }
}

fn matches_glob(pattern: &str, text: &str) -> bool {
    // glob matching with `*` (crosses separators, like the ignore package
    // for rooted patterns) and `?`; no character classes or `**` handling.
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    fn match_at(pattern: &[char], text: &[char]) -> bool {
        match (pattern.first(), text.first()) {
            (None, None) => true,
            (Some('*'), _) => {
                // `*` can match zero or more chars; try skipping text.
                (0..=text.len()).any(|skip| match_at(&pattern[1..], &text[skip..]))
            }
            (Some('?'), Some(_)) => match_at(&pattern[1..], &text[1..]),
            (Some(p), Some(t)) if p == t => match_at(&pattern[1..], &text[1..]),
            _ => false,
        }
    }
    match_at(&pattern, &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory SkillEnv for tests.
    struct MemEnv {
        files: Mutex<HashMap<String, String>>,
        dirs: Mutex<HashMap<String, Vec<SkillFileInfo>>>,
    }

    impl MemEnv {
        fn new() -> Self {
            Self {
                files: Mutex::new(HashMap::new()),
                dirs: Mutex::new(HashMap::new()),
            }
        }
        fn add_file(&self, path: &str, content: &str) {
            self.files.lock().unwrap().insert(path.to_string(), content.to_string());
        }
        fn add_dir(&self, path: &str, children: Vec<SkillFileInfo>) {
            self.dirs.lock().unwrap().insert(path.to_string(), children);
        }
    }

    fn file_info(name: &str, path: &str) -> SkillFileInfo {
        SkillFileInfo {
            name: name.to_string(),
            path: path.to_string(),
            kind: SkillFileKind::File,
            size: 0.0,
            mtime_ms: 0.0,
        }
    }

    fn dir_info(name: &str, path: &str) -> SkillFileInfo {
        SkillFileInfo {
            name: name.to_string(),
            path: path.to_string(),
            kind: SkillFileKind::Directory,
            size: 0.0,
            mtime_ms: 0.0,
        }
    }

    impl SkillEnv for MemEnv {
        fn file_info(&self, path: &str) -> Result<SkillFileInfo, String> {
            if self.dirs.lock().unwrap().contains_key(path) {
                let name = path.rsplit('/').next().unwrap_or(path).to_string();
                return Ok(dir_info(&name, path));
            }
            if let Some(content) = self.files.lock().unwrap().get(path) {
                let name = path.rsplit('/').next().unwrap_or(path).to_string();
                return Ok(SkillFileInfo {
                    name,
                    path: path.to_string(),
                    kind: SkillFileKind::File,
                    size: content.len() as f64,
                    mtime_ms: 0.0,
                });
            }
            Err("not_found".to_string())
        }
        fn list_dir(&self, path: &str) -> Result<Vec<SkillFileInfo>, String> {
            self.dirs.lock().unwrap().get(path).cloned().ok_or_else(|| "not_found".to_string())
        }
        fn read_text_file(&self, path: &str) -> Result<String, String> {
            self.files.lock().unwrap().get(path).cloned().ok_or_else(|| "not_found".to_string())
        }
        fn join_path(&self, parts: &[&str]) -> Result<String, String> {
            Ok(parts.join("/"))
        }
        fn canonical_path(&self, path: &str) -> Result<String, String> {
            Ok(path.to_string())
        }
    }

    const SKILL_MD: &str = "---\nname: my-skill\ndescription: Does things\n---\n\n# Body\n\nInstructions.";

    #[test]
    fn loads_skill_from_directory() {
        let env = MemEnv::new();
        env.add_dir("/skills", vec![dir_info("my-skill", "/skills/my-skill")]);
        env.add_dir(
            "/skills/my-skill",
            vec![file_info("SKILL.md", "/skills/my-skill/SKILL.md")],
        );
        env.add_file("/skills/my-skill/SKILL.md", SKILL_MD);

        let (skills, diagnostics) = load_skills(&env, &["/skills".to_string()]);
        assert!(diagnostics.is_empty(), "unexpected: {diagnostics:?}");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "my-skill");
        assert_eq!(skills[0].description, "Does things");
        assert!(skills[0].content.contains("Instructions"));
    }

    #[test]
    fn loads_root_md_files_and_skips_dotdirs() {
        let env = MemEnv::new();
        env.add_dir(
            "/skills",
            vec![
                file_info("root-skill.md", "/skills/root-skill.md"),
                file_info(".hidden", "/skills/.hidden"),
                dir_info("node_modules", "/skills/node_modules"),
            ],
        );
        env.add_file(
            "/skills/root-skill.md",
            "---\nname: root-skill\ndescription: Root\n---\nbody",
        );

        let (skills, _) = load_skills(&env, &["/skills".to_string()]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "root-skill");
    }

    #[test]
    fn missing_directory_is_skipped_without_diagnostic() {
        let env = MemEnv::new();
        let (skills, diagnostics) = load_skills(&env, &["/missing".to_string()]);
        assert!(skills.is_empty());
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn frontmatter_parse_subset() {
        let (frontmatter, body) = parse_frontmatter(SKILL_MD).unwrap();
        assert_eq!(frontmatter.get("name").unwrap().as_str(), Some("my-skill"));
        assert_eq!(frontmatter.get("description").unwrap().as_str(), Some("Does things"));
        assert!(body.starts_with("# Body"));

        // No frontmatter: body unchanged.
        let (frontmatter, body) = parse_frontmatter("plain text").unwrap();
        assert!(frontmatter.is_empty());
        assert_eq!(body, "plain text");

        // Unclosed frontmatter: treated as body.
        let (frontmatter, body) = parse_frontmatter("---\nname: x").unwrap();
        assert!(frontmatter.is_empty());
        assert_eq!(body, "---\nname: x");
    }

    #[test]
    fn validates_name_and_description() {
        let errors = validate_name("My Skill", "My Skill");
        assert!(errors.iter().any(|e| e.contains("invalid characters")));
        let errors = validate_name("my-skill", "other-dir");
        assert!(errors.iter().any(|e| e.contains("does not match parent directory")));
        assert!(validate_name("my-skill", "my-skill").is_empty());
        assert!(validate_description(Some("desc")).is_empty());
        assert!(!validate_description(None).is_empty());
    }

    #[test]
    fn ignore_matcher_basic_rules() {
        let mut matcher = IgnoreMatcher::new();
        matcher.add(vec!["node_modules/".to_string(), "*.log".to_string(), "!keep.log".to_string()]);
        assert!(matcher.ignores("node_modules/pkg/index.js"));
        assert!(matcher.ignores("foo/bar.log"));
        assert!(!matcher.ignores("foo/keep.log"));
        assert!(!matcher.ignores("src/main.ts"));
    }

    #[test]
    fn prefix_ignore_pattern_rules() {
        assert_eq!(prefix_ignore_pattern("# comment", ""), None);
        assert_eq!(prefix_ignore_pattern("", ""), None);
        assert_eq!(prefix_ignore_pattern("foo", "sub/").as_deref(), Some("sub/foo"));
        assert_eq!(prefix_ignore_pattern("!foo", "sub/").as_deref(), Some("!sub/foo"));
        assert_eq!(prefix_ignore_pattern("/foo", "sub/").as_deref(), Some("sub/foo"));
        assert_eq!(prefix_ignore_pattern("\\!foo", "").as_deref(), Some("!foo"));
    }

    #[test]
    fn format_invocation_includes_location_and_additional_instructions() {
        let skill = Skill {
            name: "s".to_string(),
            description: "d".to_string(),
            content: "body".to_string(),
            file_path: "/a/b/SKILL.md".to_string(),
            disable_model_invocation: None,
        };
        let formatted = format_skill_invocation(&skill, Some("extra instructions"));
        assert!(formatted.contains("<skill name=\"s\" location=\"/a/b/SKILL.md\">"));
        assert!(formatted.contains("References are relative to /a/b."));
        assert!(formatted.ends_with("extra instructions"));
        let formatted = format_skill_invocation(&skill, None);
        assert!(!formatted.contains("extra instructions"));
        assert!(formatted.ends_with("</skill>"));
    }
}
