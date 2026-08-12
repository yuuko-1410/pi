//! Skills loading and prompt formatting, port of `core/skills.ts`.
//! The gitignore matcher reuses the package-manager port.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::config::{get_agent_dir, CONFIG_DIR_NAME};
use crate::utils::basics::{parse_frontmatter, FrontmatterValue};
use crate::core::session_paths::resolve_path as resolve_path_public;
use crate::core::diagnostics::ResourceDiagnostic;
use crate::core::package_manager::IgnoreMatcher;
use crate::core::source_info::{create_synthetic_source_info, SourceInfo};

/// Max name length per spec.
const MAX_NAME_LENGTH: usize = 64;
/// Max description length per spec.
const MAX_DESCRIPTION_LENGTH: usize = 1024;

const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".fdignore"];

fn to_posix_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn relative_path(root: &str, path: &str) -> String {
    let root = Path::new(root);
    let path = Path::new(path);
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
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

    let prefixed = if prefix.is_empty() { pattern } else { format!("{prefix}{pattern}") };
    if negated {
        Some(format!("!{prefixed}"))
    } else {
        Some(prefixed)
    }
}

fn add_ignore_rules(ig: &mut IgnoreMatcher, dir: &str, root_dir: &str) {
    let relative_dir = relative_path(root_dir, dir);
    let prefix = if relative_dir.is_empty() {
        String::new()
    } else {
        format!("{}/", to_posix_path(&relative_dir))
    };

    for filename in IGNORE_FILE_NAMES {
        let ignore_path = Path::new(dir).join(filename);
        if !ignore_path.exists() {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&ignore_path) {
            let patterns: Vec<String> = content
                .split('\n')
                .filter_map(|line| prefix_ignore_pattern(line, &prefix))
                .collect();
            if !patterns.is_empty() {
                ig.add(patterns);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file_path: String,
    pub base_dir: String,
    pub source_info: SourceInfo,
    pub disable_model_invocation: bool,
}

#[derive(Clone, Debug, Default)]
pub struct LoadSkillsResult {
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

fn validate_name(name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if name.len() > MAX_NAME_LENGTH {
        errors.push(format!("name exceeds {MAX_NAME_LENGTH} characters ({})", name.len()));
    }
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
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
    let mut errors = Vec::new();
    match description {
        None | Some("") => errors.push("description is required".to_string()),
        Some(description) => {
            if description.len() > MAX_DESCRIPTION_LENGTH {
                errors.push(format!(
                    "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({})",
                    description.len()
                ));
            }
        }
    }
    errors
}

fn create_skill_source_info(file_path: &str, base_dir: &str, source: &str) -> SourceInfo {
    match source {
        "user" => create_synthetic_source_info(
            file_path,
            Some(("local".to_string(), "user".to_string(), Some(base_dir.to_string()))),
        ),
        "project" => create_synthetic_source_info(
            file_path,
            Some(("local".to_string(), "project".to_string(), Some(base_dir.to_string()))),
        ),
        "path" => create_synthetic_source_info(file_path, Some(("local".to_string(), "temporary".to_string(), Some(base_dir.to_string())))),
        other => create_synthetic_source_info(file_path, Some((other.to_string(), "temporary".to_string(), Some(base_dir.to_string())))),
    }
}

pub fn load_skills_from_dir(options: &LoadSkillsFromDirOptions) -> LoadSkillsResult {
    load_skills_from_dir_internal(&options.dir, &options.source, true, None, None)
}

pub struct LoadSkillsFromDirOptions {
    pub dir: String,
    pub source: String,
}

fn load_skills_from_dir_internal(
    dir: &str,
    source: &str,
    include_root_files: bool,
    ignore_matcher: Option<&mut IgnoreMatcher>,
    root_dir: Option<&str>,
) -> LoadSkillsResult {
    let mut skills = Vec::new();
    let mut diagnostics = Vec::new();

    if !Path::new(dir).exists() {
        return LoadSkillsResult { skills, diagnostics };
    }

    let root = root_dir.unwrap_or(dir);
    let mut ig = match ignore_matcher {
        Some(matcher) => matcher,
        None => {
            let mut matcher = IgnoreMatcher::new();
            add_ignore_rules(&mut matcher, dir, root);
            return load_skills_from_dir_internal(dir, source, include_root_files, Some(&mut matcher), Some(root));
        }
    };
    add_ignore_rules(&mut ig, dir, root);

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries.flatten().collect::<Vec<_>>(),
        Err(_) => return LoadSkillsResult { skills, diagnostics },
    };

    // First pass: SKILL.md in this directory means it is a skill root.
    for entry in &entries {
        if entry.file_name().to_string_lossy() != "SKILL.md" {
            continue;
        }
        let full_path = entry.path().to_string_lossy().to_string();
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false)
            || (entry.file_type().map(|t| t.is_symlink()).unwrap_or(false)
                && fs::metadata(&full_path).map(|m| m.is_file()).unwrap_or(false));
        let rel_path = to_posix_path(&relative_path(root, &full_path));
        if !is_file || ig.ignores(&rel_path) {
            continue;
        }
        let result = load_skill_from_file(&full_path, source);
        if let Some(skill) = result.skill {
            skills.push(skill);
        }
        diagnostics.extend(result.diagnostics);
        return LoadSkillsResult { skills, diagnostics };
    }

    // Second pass: recurse into subdirectories, load direct .md files.
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let full_path = entry.path().to_string_lossy().to_string();
        let file_type = entry.file_type().ok();
        let is_directory = file_type.map(|t| t.is_dir()).unwrap_or(false)
            || (file_type.map(|t| t.is_symlink()).unwrap_or(false)
                && fs::metadata(&full_path).map(|m| m.is_dir()).unwrap_or(false));
        let is_file = file_type.map(|t| t.is_file()).unwrap_or(false)
            || (file_type.map(|t| t.is_symlink()).unwrap_or(false)
                && fs::metadata(&full_path).map(|m| m.is_file()).unwrap_or(false));

        let rel_path = to_posix_path(&relative_path(root, &full_path));
        let ignore_path = if is_directory { format!("{rel_path}/") } else { rel_path };
        if ig.ignores(&ignore_path) {
            continue;
        }

        if is_directory {
            let sub_result = load_skills_from_dir_internal(&full_path, source, false, Some(ig), Some(root));
            skills.extend(sub_result.skills);
            diagnostics.extend(sub_result.diagnostics);
            continue;
        }

        if !is_file || !include_root_files || !name.ends_with(".md") {
            continue;
        }

        let result = load_skill_from_file(&full_path, source);
        if let Some(skill) = result.skill {
            skills.push(skill);
        }
        diagnostics.extend(result.diagnostics);
    }

    LoadSkillsResult { skills, diagnostics }
}

fn load_skill_from_file(file_path: &str, source: &str) -> LoadedSkill {
    let mut diagnostics = Vec::new();

    let raw_content = match fs::read_to_string(file_path) {
        Ok(content) => content,
        Err(error) => {
            diagnostics.push(ResourceDiagnostic {
                kind: "warning".into(),
                message: error.to_string(),
                path: Some(file_path.to_string()),
                collision: None,
            });
            return LoadedSkill { skill: None, diagnostics };
        }
    };
    let (frontmatter, _body) = parse_frontmatter(&raw_content);
    let skill_dir = Path::new(file_path)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent_dir_name = Path::new(&skill_dir)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();

    let description = frontmatter.get("description").and_then(|value| match value {
        FrontmatterValue::String(value) => Some(value.clone()),
        _ => None,
    });

    // Validate description.
    for error in validate_description(description.as_deref()) {
        diagnostics.push(ResourceDiagnostic {
            kind: "warning".into(),
            message: error,
            path: Some(file_path.to_string()),
            collision: None,
        });
    }

    // Use name from frontmatter, or fall back to the parent directory name.
    let name = match frontmatter.get("name") {
        Some(FrontmatterValue::String(value)) if !value.is_empty() => value.clone(),
        _ => parent_dir_name,
    };

    // Validate name.
    for error in validate_name(&name) {
        diagnostics.push(ResourceDiagnostic {
            kind: "warning".into(),
            message: error,
            path: Some(file_path.to_string()),
            collision: None,
        });
    }

    // Still load the skill even with warnings (unless description is missing).
    let Some(description) = description else {
        return LoadedSkill { skill: None, diagnostics };
    };
    if description.trim().is_empty() {
        return LoadedSkill { skill: None, diagnostics };
    }

    let disable_model_invocation = matches!(
        frontmatter.get("disable-model-invocation"),
        Some(FrontmatterValue::Bool(true))
    );

    LoadedSkill {
        skill: Some(Skill {
            name,
            description,
            file_path: file_path.to_string(),
            base_dir: skill_dir.clone(),
            source_info: create_skill_source_info(file_path, &skill_dir, source),
            disable_model_invocation,
        }),
        diagnostics,
    }
}

struct LoadedSkill {
    skill: Option<Skill>,
    diagnostics: Vec<ResourceDiagnostic>,
}

/// Format skills for inclusion in a system prompt (Agent Skills XML).
pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    let visible_skills: Vec<&Skill> = skills.iter().filter(|skill| !skill.disable_model_invocation).collect();

    if visible_skills.is_empty() {
        return String::new();
    }

    let mut lines = vec![
        "\n\nThe following skills provide specialized instructions for specific tasks.".to_string(),
        "Use the read tool to load a skill's file when the task matches its description.".to_string(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];

    for skill in visible_skills {
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!("    <description>{}</description>", escape_xml(&skill.description)));
        lines.push(format!("    <location>{}</location>", escape_xml(&skill.file_path)));
        lines.push("  </skill>".to_string());
    }

    lines.push("</available_skills>".to_string());

    lines.join("\n")
}

fn escape_xml(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub struct LoadSkillsOptions {
    pub cwd: String,
    pub agent_dir: Option<String>,
    pub skill_paths: Vec<String>,
    pub include_defaults: bool,
}

/// Load skills from all configured locations.
pub fn load_skills(options: &LoadSkillsOptions) -> LoadSkillsResult {
    let resolved_cwd = resolve_path_public(&options.cwd, None);
    let resolved_agent_dir = resolve_path_public(options.agent_dir.as_deref().unwrap_or(&get_agent_dir()), None);

    let mut skill_map: HashMap<String, Skill> = HashMap::new();
    let mut real_path_set: HashSet<String> = HashSet::new();
    let mut all_diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    let mut collision_diagnostics: Vec<ResourceDiagnostic> = Vec::new();

    fn canonicalize(path: &str) -> String {
        fs::canonicalize(path)
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string())
    }

    {
        fn add_skills(
            result: LoadSkillsResult,
            skill_map: &mut HashMap<String, Skill>,
            real_path_set: &mut HashSet<String>,
            all_diagnostics: &mut Vec<ResourceDiagnostic>,
            collision_diagnostics: &mut Vec<ResourceDiagnostic>,
        ) {
            all_diagnostics.extend(result.diagnostics);
            for skill in result.skills {
                let real_path = canonicalize(&skill.file_path);
                if real_path_set.contains(&real_path) {
                    continue;
                }
                if let Some(existing) = skill_map.get(&skill.name) {
                    collision_diagnostics.push(ResourceDiagnostic {
                        kind: "collision".into(),
                        message: format!("name \"{}\" collision", skill.name),
                        path: Some(skill.file_path.clone()),
                        collision: Some(crate::core::diagnostics::ResourceCollision {
                            resource_type: "skill".into(),
                            name: skill.name.clone(),
                            winner_path: existing.file_path.clone(),
                            loser_path: skill.file_path.clone(),
                            winner_source: None,
                            loser_source: None,
                        }),
                    });
                } else {
                    skill_map.insert(skill.name.clone(), skill);
                    real_path_set.insert(real_path);
                }
            }
        }

        if options.include_defaults {
            let global_dir = Path::new(&resolved_agent_dir).join("skills").to_string_lossy().to_string();
            add_skills(
                load_skills_from_dir(&LoadSkillsFromDirOptions { dir: global_dir, source: "user".into() }),
                &mut skill_map,
                &mut real_path_set,
                &mut all_diagnostics,
                &mut collision_diagnostics,
            );
            let project_dir = Path::new(&resolved_cwd)
                .join(CONFIG_DIR_NAME)
                .join("skills")
                .to_string_lossy()
                .to_string();
            add_skills(
                load_skills_from_dir(&LoadSkillsFromDirOptions { dir: project_dir, source: "project".into() }),
                &mut skill_map,
                &mut real_path_set,
                &mut all_diagnostics,
                &mut collision_diagnostics,
            );
        }
    }

    let user_skills_dir = Path::new(&resolved_agent_dir).join("skills").to_string_lossy().to_string();
    let project_skills_dir = Path::new(&resolved_cwd)
        .join(CONFIG_DIR_NAME)
        .join("skills")
        .to_string_lossy()
        .to_string();

    let is_under_path = |target: &str, root: &str| -> bool {
        if target == root {
            return true;
        }
        let prefix = if root.ends_with('/') { root.to_string() } else { format!("{root}/") };
        target.starts_with(&prefix)
    };

    let get_source = |resolved_path: &str| -> &'static str {
        if !options.include_defaults {
            if is_under_path(resolved_path, &user_skills_dir) {
                return "user";
            }
            if is_under_path(resolved_path, &project_skills_dir) {
                return "project";
            }
        }
        "path"
    };

    for raw_path in &options.skill_paths {
        let resolved_path = resolve_path_public(raw_path, None);
        if !Path::new(&resolved_path).exists() {
            all_diagnostics.push(ResourceDiagnostic {
                kind: "warning".into(),
                message: "skill path does not exist".into(),
                path: Some(resolved_path),
                collision: None,
            });
            continue;
        }

        let metadata = fs::metadata(&resolved_path);
        let Ok(metadata) = metadata else {
            all_diagnostics.push(ResourceDiagnostic {
                kind: "warning".into(),
                message: "failed to read skill path".into(),
                path: Some(resolved_path),
                collision: None,
            });
            continue;
        };
        let source = get_source(&resolved_path);
        if metadata.is_dir() {
            let result = load_skills_from_dir(&LoadSkillsFromDirOptions {
                dir: resolved_path,
                source: source.to_string(),
            });
            for skill in result.skills {
                let real_path = canonicalize(&skill.file_path);
                if real_path_set.contains(&real_path) {
                    continue;
                }
                if let Some(existing) = skill_map.get(&skill.name) {
                    collision_diagnostics.push(ResourceDiagnostic {
                        kind: "collision".into(),
                        message: format!("name \"{}\" collision", skill.name),
                        path: Some(skill.file_path.clone()),
                        collision: Some(crate::core::diagnostics::ResourceCollision {
                            resource_type: "skill".into(),
                            name: skill.name.clone(),
                            winner_path: existing.file_path.clone(),
                            loser_path: skill.file_path.clone(),
                            winner_source: None,
                            loser_source: None,
                        }),
                    });
                } else {
                    skill_map.insert(skill.name.clone(), skill);
                    real_path_set.insert(real_path);
                }
            }
            all_diagnostics.extend(result.diagnostics);
        } else if metadata.is_file() && resolved_path.ends_with(".md") {
            let result = load_skill_from_file(&resolved_path, source);
            if let Some(skill) = result.skill {
                let real_path = canonicalize(&skill.file_path);
                if !real_path_set.contains(&real_path) {
                    if let Some(existing) = skill_map.get(&skill.name) {
                        collision_diagnostics.push(ResourceDiagnostic {
                            kind: "collision".into(),
                            message: format!("name \"{}\" collision", skill.name),
                            path: Some(skill.file_path.clone()),
                            collision: Some(crate::core::diagnostics::ResourceCollision {
                                resource_type: "skill".into(),
                                name: skill.name.clone(),
                                winner_path: existing.file_path.clone(),
                                loser_path: skill.file_path.clone(),
                                winner_source: None,
                                loser_source: None,
                            }),
                        });
                    } else {
                        skill_map.insert(skill.name.clone(), skill);
                        real_path_set.insert(real_path);
                    }
                }
            } else {
                all_diagnostics.extend(result.diagnostics);
            }
        } else {
            all_diagnostics.push(ResourceDiagnostic {
                kind: "warning".into(),
                message: "skill path is not a markdown file".into(),
                path: Some(resolved_path),
                collision: None,
            });
        }
    }

    let mut skills: Vec<Skill> = skill_map.into_values().collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    let mut diagnostics = all_diagnostics;
    diagnostics.extend(collision_diagnostics);
    LoadSkillsResult { skills, diagnostics }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-skills-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().to_string()
    }

    #[test]
    fn loads_skill_from_directory() {
        let dir = temp_dir();
        let skill_dir = Path::new(&dir).join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: A test skill\n---\n\n# Skill body",
        )
        .unwrap();

        let result = load_skills_from_dir(&LoadSkillsFromDirOptions { dir, source: "user".into() });
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].name, "my-skill");
        assert_eq!(result.skills[0].description, "A test skill");
        assert!(result.skills[0].file_path.ends_with("SKILL.md"));
    }

    #[test]
    fn missing_description_rejected() {
        let dir = temp_dir();
        let skill_dir = Path::new(&dir).join("no-desc");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "---\nname: no-desc\n---\n\nbody").unwrap();

        let result = load_skills_from_dir(&LoadSkillsFromDirOptions { dir, source: "user".into() });
        assert!(result.skills.is_empty());
        assert!(result.diagnostics.iter().any(|d| d.message.contains("description is required")));
    }

    #[test]
    fn invalid_name_diagnostic_but_loaded() {
        let dir = temp_dir();
        let skill_dir = Path::new(&dir).join("Bad_Name");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Bad_Name\ndescription: ok\n---\n\nbody",
        )
        .unwrap();

        let result = load_skills_from_dir(&LoadSkillsFromDirOptions { dir, source: "user".into() });
        assert_eq!(result.skills.len(), 1);
        assert!(result.diagnostics.iter().any(|d| d.message.contains("invalid characters")));
    }

    #[test]
    fn format_skills_xml() {
        let skills = vec![Skill {
            name: "a-b".into(),
            description: "desc <x>".into(),
            file_path: "/x/SKILL.md".into(),
            base_dir: "/x".into(),
            source_info: SourceInfo {
                path: "/x/SKILL.md".into(),
                source: "local".into(),
                scope: "user".into(),
                origin: "top-level".into(),
                base_dir: None,
            },
            disable_model_invocation: false,
        }];
        let prompt = format_skills_for_prompt(&skills);
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>a-b</name>"));
        assert!(prompt.contains("desc &lt;x&gt;"));

        let disabled = Skill {
            disable_model_invocation: true,
            ..skills[0].clone()
        };
        assert_eq!(format_skills_for_prompt(&[disabled]), "");
    }

    #[test]
    fn ignore_rules_respected() {
        let dir = temp_dir();
        let skill_dir = Path::new(&dir).join("ignored-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: should be ignored\n---\n\nbody",
        )
        .unwrap();
        std::fs::write(Path::new(&dir).join(".gitignore"), "ignored-skill/").unwrap();

        let result = load_skills_from_dir(&LoadSkillsFromDirOptions { dir, source: "user".into() });
        assert!(result.skills.is_empty());
    }
}
