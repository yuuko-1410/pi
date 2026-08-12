//! System prompt construction, port of `core/system-prompt.ts`.

use super::skills::{format_skills_for_prompt, Skill};

#[derive(Clone, Debug, Default)]
pub struct BuildSystemPromptOptions {
    /// Custom system prompt (replaces default).
    pub custom_prompt: Option<String>,
    /// Tools to include in prompt. Default: [read, bash, edit, write].
    pub selected_tools: Option<Vec<String>>,
    /// Optional one-line tool snippets keyed by tool name.
    pub tool_snippets: Option<Vec<(String, String)>>,
    /// Additional guideline bullets appended to the default guidelines.
    pub prompt_guidelines: Option<Vec<String>>,
    /// Text to append to system prompt.
    pub append_system_prompt: Option<String>,
    /// Working directory.
    pub cwd: String,
    /// Pre-loaded context files.
    pub context_files: Vec<(String, String)>, // (path, content)
    /// Pre-loaded skills.
    pub skills: Vec<Skill>,
}

/// Build the system prompt with tools, guidelines, and context.
pub fn build_system_prompt(options: &BuildSystemPromptOptions) -> String {
    let prompt_cwd = options.cwd.replace('\\', "/");

    let append_section = options
        .append_system_prompt
        .as_deref()
        .map(|value| format!("\n\n{value}"))
        .unwrap_or_default();

    let context_files = &options.context_files;
    let skills = &options.skills;

    if let Some(custom_prompt) = &options.custom_prompt {
        let mut prompt = custom_prompt.clone();

        if !append_section.is_empty() {
            prompt.push_str(&append_section);
        }

        if !context_files.is_empty() {
            prompt.push_str("\n\n<project_context>\n\n");
            prompt.push_str("Project-specific instructions and guidelines:\n\n");
            for (file_path, content) in context_files {
                prompt.push_str(&format!(
                    "<project_instructions path=\"{file_path}\">\n{content}\n</project_instructions>\n\n"
                ));
            }
            prompt.push_str("</project_context>\n");
        }

        // Append skills section (only if the read tool is available).
        let custom_prompt_has_read = options
            .selected_tools
            .as_ref()
            .is_none_or(|tools| tools.iter().any(|tool| tool == "read"));
        if custom_prompt_has_read && !skills.is_empty() {
            prompt.push_str(&format_skills_for_prompt(skills));
        }

        prompt.push_str(&format!("\nCurrent working directory: {prompt_cwd}\n"));

        return prompt;
    }

    // Get absolute paths to documentation and examples.
    let readme_path = crate::config::get_package_dir() + "/README.md";
    let docs_path = crate::config::get_package_dir() + "/docs";
    let examples_path = crate::config::get_package_dir() + "/examples";

    // A tool appears in "Available tools" only when the caller provides a
    // one-line snippet.
    let tools: Vec<String> = options
        .selected_tools
        .clone()
        .unwrap_or_else(|| ["read", "bash", "edit", "write"].iter().map(|value| value.to_string()).collect());
    let visible_tools: Vec<String> = tools
        .iter()
        .filter(|name| {
            options
                .tool_snippets
                .as_ref()
                .is_some_and(|snippets| snippets.iter().any(|(tool, _)| tool == *name))
        })
        .cloned()
        .collect();
    let tools_list = if !visible_tools.is_empty() {
        visible_tools
            .iter()
            .map(|name| {
                let snippet = options
                    .tool_snippets
                    .as_ref()
                    .and_then(|snippets| snippets.iter().find(|(tool, _)| tool == name))
                    .map(|(_, snippet)| snippet.as_str())
                    .unwrap_or("");
                format!("- {name}: {snippet}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        "(none)".to_string()
    };

    let mut guidelines_list: Vec<String> = Vec::new();
    let mut guidelines_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut add_guideline = |guideline: &str| {
        if guidelines_set.contains(guideline) {
            return;
        }
        guidelines_set.insert(guideline.to_string());
        guidelines_list.push(guideline.to_string());
    };

    let has_bash = tools.iter().any(|tool| tool == "bash");
    let has_grep = tools.iter().any(|tool| tool == "grep");
    let has_find = tools.iter().any(|tool| tool == "find");
    let has_ls = tools.iter().any(|tool| tool == "ls");
    let has_read = tools.iter().any(|tool| tool == "read");

    // File exploration guidelines.
    if has_bash && !has_grep && !has_find && !has_ls {
        add_guideline("Use bash for file operations like ls, rg, find");
    }

    if let Some(prompt_guidelines) = &options.prompt_guidelines {
        for guideline in prompt_guidelines {
            let normalized = guideline.trim();
            if !normalized.is_empty() {
                add_guideline(normalized);
            }
        }
    }

    // Always include these.
    add_guideline("Be concise in your responses");
    add_guideline("Show file paths clearly when working with files");

    let guidelines = guidelines_list.iter().map(|guideline| format!("- {guideline}")).collect::<Vec<_>>().join("\n");

    let mut prompt = format!(
        "You are an expert coding assistant operating inside pi, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.

Available tools:
{tools_list}

In addition to the tools above, you may have access to other custom tools depending on the project.

Guidelines:
{guidelines}

Pi documentation (read only when the user asks about pi itself, its SDK, extensions, themes, skills, or TUI):
- Main documentation: {readme_path}
- Additional docs: {docs_path}
- Examples: {examples_path} (extensions, custom tools, SDK)
- When reading pi docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory
- When asked about: extensions (docs/extensions.md, examples/extensions/), themes (docs/themes.md), skills (docs/skills.md), prompt templates (docs/prompt-templates.md), TUI components (docs/tui.md), keybindings (docs/keybindings.md), SDK integrations (docs/sdk.md), custom providers (docs/custom-provider.md), adding models (docs/models.md), pi packages (docs/packages.md), environment variables (docs/environment-variables.md)
- When working on pi topics, read the docs and examples, and follow .md cross-references before implementing
- Always read pi .md files completely and follow links to related docs (e.g., tui.md for TUI API details)"
    );

    if !append_section.is_empty() {
        prompt.push_str(&append_section);
    }

    if !context_files.is_empty() {
        prompt.push_str("\n\n<project_context>\n\n");
        prompt.push_str("Project-specific instructions and guidelines:\n\n");
        for (file_path, content) in context_files {
            prompt.push_str(&format!(
                "<project_instructions path=\"{file_path}\">\n{content}\n</project_instructions>\n\n"
            ));
        }
        prompt.push_str("</project_context>\n");
    }

    // Append skills section (only if read tool is available).
    if has_read && !skills.is_empty() {
        prompt.push_str(&format_skills_for_prompt(skills));
    }

    prompt.push_str(&format!("\nCurrent working directory: {prompt_cwd}"));

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_options() -> BuildSystemPromptOptions {
        BuildSystemPromptOptions {
            cwd: "/tmp".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn default_prompt_mentions_docs() {
        let prompt = build_system_prompt(&default_options());
        assert!(prompt.contains("Available tools:"));
        assert!(prompt.contains("(none)"));
        assert!(prompt.contains("Be concise in your responses"));
        assert!(prompt.contains("Current working directory: /tmp"));
        assert!(prompt.contains("Pi documentation"));
    }

    #[test]
    fn tool_snippets_show_tools() {
        let mut options = default_options();
        options.selected_tools = Some(vec!["read".into(), "bash".into()]);
        options.tool_snippets = Some(vec![("read".to_string(), "Read files".to_string())]);
        let prompt = build_system_prompt(&options);
        assert!(prompt.contains("- read: Read files"));
        assert!(!prompt.contains("- bash:"));
    }

    #[test]
    fn bash_only_exploration_guideline() {
        let mut options = default_options();
        options.selected_tools = Some(vec!["bash".into()]);
        let prompt = build_system_prompt(&options);
        assert!(prompt.contains("Use bash for file operations like ls, rg, find"));
    }

    #[test]
    fn custom_prompt_path() {
        let mut options = default_options();
        options.custom_prompt = Some("You are a test agent.".to_string());
        options.selected_tools = Some(vec!["bash".into()]);
        let prompt = build_system_prompt(&options);
        assert!(prompt.starts_with("You are a test agent."));
        // No tools section in custom prompt mode.
        assert!(!prompt.contains("Available tools:"));
        assert!(prompt.ends_with("Current working directory: /tmp\n"));
    }

    #[test]
    fn context_files_and_appends() {
        let mut options = default_options();
        options.append_system_prompt = Some("Extra instructions".to_string());
        options.context_files = vec![("AGENTS.md".to_string(), "content here".to_string())];
        let prompt = build_system_prompt(&options);
        assert!(prompt.contains("Extra instructions"));
        assert!(prompt.contains("<project_instructions path=\"AGENTS.md\">"));
        assert!(prompt.contains("content here"));
    }

    #[test]
    fn skills_appended_when_read_available() {
        let mut options = default_options();
        options.selected_tools = Some(vec!["read".into()]);
        options.skills = vec![Skill {
            name: "test-skill".into(),
            description: "A skill".into(),
            file_path: "/x/SKILL.md".into(),
            base_dir: "/x".into(),
            source_info: super::super::source_info::SourceInfo {
                path: "/x/SKILL.md".into(),
                source: "local".into(),
                scope: "user".into(),
                origin: "top-level".into(),
                base_dir: None,
            },
            disable_model_invocation: false,
        }];
        let prompt = build_system_prompt(&options);
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("test-skill"));
    }
}
