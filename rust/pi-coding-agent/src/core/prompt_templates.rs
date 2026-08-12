//! Prompt templates, port of `core/prompt-templates.ts`.

use std::fs;
use std::path::Path;

use crate::config::CONFIG_DIR_NAME;
use crate::core::session_paths::{join, resolve_path};
use crate::core::source_info::{create_synthetic_source_info, SourceInfo};
use crate::utils::basics::parse_frontmatter;

#[derive(Clone, Debug, PartialEq)]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    pub content: String,
    pub source_info: SourceInfo,
    pub file_path: String,
}

/// Parse command arguments respecting quoted strings (bash-style).
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for char in args_string.chars() {
        if let Some(quote) = in_quote {
            if char == quote {
                in_quote = None;
            } else {
                current.push(char);
            }
        } else if char == '"' || char == '\'' {
            in_quote = Some(char);
        } else if char.is_whitespace() {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(char);
        }
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

/// Substitute argument placeholders in template content:
/// `$1`..`$N`, `$@`/`$ARGUMENTS`, `${N:-default}`, `${@:-default}`,
/// `${@:N}` and `${@:N:L}`. Values are never recursively substituted.
pub fn substitute_args(content: &str, args: &[String]) -> String {
    let all_args = args.join(" ");
    let mut result = String::with_capacity(content.len());
    let chars: Vec<char> = content.chars().collect();
    let mut index = 0;

    while index < chars.len() {
        if chars[index] != '$' {
            result.push(chars[index]);
            index += 1;
            continue;
        }

        // ${...} forms.
        if index + 1 < chars.len() && chars[index + 1] == '{' {
            let Some(close) = chars[index + 2..].iter().position(|c| *c == '}') else {
                result.push('$');
                index += 1;
                continue;
            };
            let close = index + 2 + close;
            let inner: String = chars[index + 2..close].iter().collect();

            if let Some((target, default)) = inner.split_once(":-") {
                let value = if target == "@" || target == "ARGUMENTS" {
                    if all_args.is_empty() {
                        None
                    } else {
                        Some(all_args.as_str())
                    }
                } else {
                    target.parse::<usize>().ok().and_then(|n| args.get(n - 1).map(|v| v.as_str()))
                };
                match value {
                    Some(value) if !value.is_empty() => result.push_str(value),
                    _ => result.push_str(default),
                }
                index = close + 1;
                continue;
            }

            if let Some(rest) = inner.strip_prefix("@:") {
                let parts: Vec<&str> = rest.splitn(2, ':').collect();
                let start_raw: usize = parts[0].parse().unwrap_or(1);
                let mut start = start_raw.saturating_sub(1);
                if start == usize::MAX {
                    start = 0;
                }
                let slice: Vec<String> = match parts.get(1) {
                    Some(length_raw) => {
                        let length: usize = length_raw.parse().unwrap_or(0);
                        args.iter().skip(start).take(length).map(|v| v.clone()).collect()
                    }
                    None => args.iter().skip(start).map(|v| v.clone()).collect(),
                };
                result.push_str(&slice.join(" "));
                index = close + 1;
                continue;
            }

            // Unknown ${...} left as-is.
            result.push('$');
            index += 1;
            continue;
        }

        // $NAME forms: $@, $ARGUMENTS, $digits.
        let rest: String = chars[index + 1..].iter().collect();
        if rest.starts_with('@') {
            result.push_str(&all_args);
            index += 2;
            continue;
        }
        if rest.starts_with("ARGUMENTS") {
            let consumed = "ARGUMENTS".len();
            result.push_str(&all_args);
            index += 1 + consumed;
            continue;
        }
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            let n: usize = digits.parse().unwrap_or(0);
            match args.get(n.wrapping_sub(1)) {
                Some(value) => result.push_str(value),
                None => {}
            }
            index += 1 + digits.len();
            continue;
        }

        result.push('$');
        index += 1;
    }

    result
}

fn load_template_from_file(file_path: &str, source_info: SourceInfo) -> Option<PromptTemplate> {
    let raw_content = fs::read_to_string(file_path).ok()?;
    let (frontmatter, body) = parse_frontmatter(&raw_content);

    let name = Path::new(file_path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
        .strip_suffix(".md")
        .unwrap_or_default()
        .to_string();

    // Get description from frontmatter or the first non-empty line.
    let mut description = frontmatter
        .get("description")
        .and_then(|value| match value {
            crate::utils::basics::FrontmatterValue::String(value) => Some(value.clone()),
            _ => None,
        })
        .unwrap_or_default();
    if description.is_empty() {
        if let Some(first_line) = body.split('\n').find(|line| !line.trim().is_empty()) {
            description = first_line.chars().take(60).collect();
            if first_line.chars().count() > 60 {
                description.push_str("...");
            }
        }
    }

    let argument_hint = frontmatter
        .get("argument-hint")
        .and_then(|value| match value {
            crate::utils::basics::FrontmatterValue::String(value) => Some(value.clone()),
            _ => None,
        });

    Some(PromptTemplate {
        name,
        description,
        argument_hint,
        content: body,
        source_info,
        file_path: file_path.to_string(),
    })
}

fn load_templates_from_dir(dir: &str, get_source_info: &dyn Fn(&str) -> SourceInfo) -> Vec<PromptTemplate> {
    let mut templates = Vec::new();
    if !Path::new(dir).exists() {
        return templates;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries.flatten().collect::<Vec<_>>(),
        Err(_) => return templates,
    };
    for entry in entries {
        let full_path = entry.path().to_string_lossy().to_string();
        let file_type = entry.file_type().ok();
        let mut is_file = file_type.map(|t| t.is_file()).unwrap_or(false);
        if file_type.map(|t| t.is_symlink()).unwrap_or(false) {
            is_file = fs::metadata(&full_path).map(|m| m.is_file()).unwrap_or(false);
        }
        if is_file && full_path.ends_with(".md") {
            if let Some(template) = load_template_from_file(&full_path, get_source_info(&full_path)) {
                templates.push(template);
            }
        }
    }
    templates
}

pub struct LoadPromptTemplatesOptions {
    pub cwd: String,
    pub agent_dir: String,
    pub prompt_paths: Vec<String>,
    pub include_defaults: bool,
}

fn is_under_path(target: &str, root: &str) -> bool {
    if target == root {
        return true;
    }
    let prefix = if root.ends_with('/') {
        root.to_string()
    } else {
        format!("{root}/")
    };
    target.starts_with(&prefix)
}

/// Load all prompt templates from global, project, and explicit paths.
pub fn load_prompt_templates(options: &LoadPromptTemplatesOptions) -> Vec<PromptTemplate> {
    let resolved_cwd = resolve_path(&options.cwd, None);
    let resolved_agent_dir = resolve_path(&options.agent_dir, None);

    let mut templates = Vec::new();

    let global_prompts_dir = join(&resolved_agent_dir, "prompts");
    let project_prompts_dir = join(&join(&resolved_cwd, CONFIG_DIR_NAME), "prompts");

    let get_source_info = |resolved_path: &str| -> SourceInfo {
        if is_under_path(resolved_path, &global_prompts_dir) {
            return create_synthetic_source_info(
                resolved_path,
                Some(("local".to_string(), "user".to_string(), Some(global_prompts_dir.clone()))),
            );
        }
        if is_under_path(resolved_path, &project_prompts_dir) {
            return create_synthetic_source_info(
                resolved_path,
                Some(("local".to_string(), "project".to_string(), Some(project_prompts_dir.clone()))),
            );
        }
        let is_dir = fs::metadata(resolved_path).map(|m| m.is_dir()).unwrap_or(false);
        let base_dir = if is_dir {
            resolved_path.to_string()
        } else {
            Path::new(resolved_path)
                .parent()
                .map(|parent| parent.to_string_lossy().to_string())
                .unwrap_or_default()
        };
        create_synthetic_source_info(resolved_path, Some(("local".to_string(), "temporary".to_string(), Some(base_dir))))
    };

    if options.include_defaults {
        templates.extend(load_templates_from_dir(&global_prompts_dir, &get_source_info));
        templates.extend(load_templates_from_dir(&project_prompts_dir, &get_source_info));
    }

    // Load explicit prompt paths.
    for raw_path in &options.prompt_paths {
        let resolved_path = resolve_path(raw_path, None);
        if !Path::new(&resolved_path).exists() {
            continue;
        }
        let metadata = match fs::metadata(&resolved_path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.is_dir() {
            templates.extend(load_templates_from_dir(&resolved_path, &get_source_info));
        } else if metadata.is_file() && resolved_path.ends_with(".md") {
            if let Some(template) = load_template_from_file(&resolved_path, get_source_info(&resolved_path)) {
                templates.push(template);
            }
        }
    }

    templates
}

/// Expand a prompt template if the text matches a template name; otherwise
/// return the original text.
pub fn expand_prompt_template(text: &str, templates: &[PromptTemplate]) -> String {
    if !text.starts_with('/') {
        return text.to_string();
    }
    let Some(rest) = text.strip_prefix('/') else {
        return text.to_string();
    };
    let (template_name, args_string) = match rest.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.to_string()),
        None => (rest, String::new()),
    };
    if template_name.is_empty() {
        return text.to_string();
    }

    if let Some(template) = templates.iter().find(|template| template.name == template_name) {
        let args = parse_command_args(&args_string);
        return substitute_args(&template.content, &args);
    }

    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_args() {
        assert_eq!(parse_command_args("a b c"), vec!["a", "b", "c"]);
        assert_eq!(parse_command_args("a \"b c\" d"), vec!["a", "b c", "d"]);
        assert_eq!(parse_command_args("'x y' z"), vec!["x y", "z"]);
        assert_eq!(parse_command_args(""), Vec::<String>::new());
        // JS behavior: leading/trailing whitespace is ignored, inner words kept.
        assert_eq!(parse_command_args("  spaced  "), vec!["spaced"]);
    }

    #[test]
    fn substitutes_positional_args() {
        let args = vec!["one".to_string(), "two".to_string()];
        assert_eq!(substitute_args("$1 $2", &args), "one two");
        assert_eq!(substitute_args("$1-$2", &args), "one-two");
        assert_eq!(substitute_args("$3", &args), "");
        assert_eq!(substitute_args("$@", &args), "one two");
        assert_eq!(substitute_args("$ARGUMENTS", &args), "one two");
        assert_eq!(substitute_args("literal $1", &[]), "literal ");
    }

    #[test]
    fn substitutes_defaults_and_slices() {
        let args = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(substitute_args("${1:-default}", &args), "a");
        assert_eq!(substitute_args("${5:-default}", &args), "default");
        assert_eq!(substitute_args("${2:-d}", &[]), "d");
        assert_eq!(substitute_args("${@:-nothing}", &[]), "nothing");
        assert_eq!(substitute_args("${@:-nothing}", &args), "a b c");
        assert_eq!(substitute_args("${@:2}", &args), "b c");
        assert_eq!(substitute_args("${@:2:1}", &args), "b");
        assert_eq!(substitute_args("${@:0}", &args), "a b c"); // 0 treated as 1
        assert_eq!(substitute_args("${ARGUMENTS:-d}", &[]), "d");
    }

    #[test]
    fn no_recursive_substitution() {
        let args = vec!["$1".to_string()];
        assert_eq!(substitute_args("$1", &args), "$1");
    }

    #[test]
    fn loads_templates_from_dir_with_frontmatter() {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-tpl-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("review.md"),
            "---\ndescription: Do a review\nargument-hint: <file>\n---\n\nReview $1 carefully.",
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "not a template").unwrap();

        let templates = load_templates_from_dir(
            &dir.to_string_lossy(),
            &|path| create_synthetic_source_info(path, None),
        );
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].name, "review");
        assert_eq!(templates[0].description, "Do a review");
        assert_eq!(templates[0].argument_hint.as_deref(), Some("<file>"));
        assert_eq!(templates[0].content, "Review $1 carefully.");
    }

    #[test]
    fn expand_template_or_original() {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-tpl2-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("review.md"), "---\ndescription: d\n---\n\nReview $1").unwrap();

        let templates = load_templates_from_dir(&dir.to_string_lossy(), &|path| create_synthetic_source_info(path, None));
        assert_eq!(expand_prompt_template("/review myfile", &templates), "Review myfile");
        assert_eq!(expand_prompt_template("/missing x", &templates), "/missing x");
        assert_eq!(expand_prompt_template("plain text", &templates), "plain text");
        assert_eq!(expand_prompt_template("/", &templates), "/");
    }

    #[test]
    fn description_falls_back_to_first_line() {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-tpl3-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let long_line = "x".repeat(80);
        std::fs::write(dir.join("plain.md"), format!("{long_line}\n\nbody")).unwrap();

        let templates = load_templates_from_dir(&dir.to_string_lossy(), &|path| create_synthetic_source_info(path, None));
        assert_eq!(templates[0].description.len(), 63); // 60 chars + "..."
        assert!(templates[0].description.ends_with("..."));
    }
}
