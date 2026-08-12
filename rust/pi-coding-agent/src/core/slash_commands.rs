//! Slash commands, port of `core/slash-commands.ts`.

use crate::config::APP_NAME;
use crate::core::source_info::SourceInfo;

#[derive(Clone, Debug, PartialEq)]
pub enum SlashCommandSource {
    Extension,
    Prompt,
    Skill,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SlashCommandInfo {
    pub name: String,
    pub description: Option<String>,
    pub source: SlashCommandSource,
    pub source_info: SourceInfo,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BuiltinSlashCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub argument_hint: Option<&'static str>,
}

pub const BUILTIN_SLASH_COMMANDS: &[BuiltinSlashCommand] = &[
    BuiltinSlashCommand { name: "settings", description: "Open settings menu", argument_hint: None },
    BuiltinSlashCommand { name: "model", description: "Select model (opens selector UI)", argument_hint: Some("<provider/model>") },
    BuiltinSlashCommand { name: "scoped-models", description: "Enable/disable models for Ctrl+P cycling", argument_hint: None },
    BuiltinSlashCommand { name: "export", description: "Export session (HTML default, or specify path: .html/.jsonl)", argument_hint: None },
    BuiltinSlashCommand { name: "import", description: "Import and resume a session from a JSONL file", argument_hint: None },
    BuiltinSlashCommand { name: "share", description: "Share session as a secret GitHub gist", argument_hint: None },
    BuiltinSlashCommand { name: "copy", description: "Copy last agent message to clipboard", argument_hint: None },
    BuiltinSlashCommand { name: "name", description: "Set session display name", argument_hint: None },
    BuiltinSlashCommand { name: "session", description: "Show session info and stats", argument_hint: None },
    BuiltinSlashCommand { name: "changelog", description: "Show changelog entries", argument_hint: None },
    BuiltinSlashCommand { name: "hotkeys", description: "Show all keyboard shortcuts", argument_hint: None },
    BuiltinSlashCommand { name: "fork", description: "Create a new fork from a previous user message", argument_hint: None },
    BuiltinSlashCommand { name: "clone", description: "Duplicate the current session at the current position", argument_hint: None },
    BuiltinSlashCommand { name: "tree", description: "Navigate session tree (switch branches)", argument_hint: None },
    BuiltinSlashCommand { name: "trust", description: "Save project trust decision for future sessions", argument_hint: None },
    BuiltinSlashCommand { name: "login", description: "Configure provider authentication", argument_hint: Some("<provider>") },
    BuiltinSlashCommand { name: "logout", description: "Remove provider authentication", argument_hint: None },
    BuiltinSlashCommand { name: "new", description: "Start a new session", argument_hint: None },
    BuiltinSlashCommand { name: "compact", description: "Manually compact the session context", argument_hint: None },
    BuiltinSlashCommand { name: "resume", description: "Resume a different session", argument_hint: None },
    BuiltinSlashCommand { name: "reload", description: "Reload keybindings, extensions, skills, prompts, themes, and context files", argument_hint: None },
    BuiltinSlashCommand { name: "quit", description: "Quit π", argument_hint: None },
];

/// Builtin command names for lookup (the JS array's name field).
pub fn builtin_command_names() -> Vec<&'static str> {
    BUILTIN_SLASH_COMMANDS.iter().map(|command| command.name).collect()
}

/// APP_NAME re-export for callers that need the display name.
pub fn app_name() -> &'static str {
    APP_NAME
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_commands_cover_core_actions() {
        let names = builtin_command_names();
        assert!(names.contains(&"settings"));
        assert!(names.contains(&"model"));
        assert!(names.contains(&"quit"));
        assert!(names.contains(&"compact"));
        assert_eq!(names.len(), 22);
        assert_eq!(app_name(), "pi");
        // argument hints on model/login only.
        let model = BUILTIN_SLASH_COMMANDS.iter().find(|c| c.name == "model").unwrap();
        assert_eq!(model.argument_hint, Some("<provider/model>"));
    }
}
