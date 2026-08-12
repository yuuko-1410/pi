//! CLI argument parsing and help display, port of `cli/args.ts`.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Text,
    Json,
    Rpc,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Text => "text",
            Mode::Json => "json",
            Mode::Rpc => "rpc",
        }
    }
}

#[derive(Debug, Default)]
pub struct Args {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Vec<String>,
    pub thinking: Option<String>,
    pub r#continue: bool,
    pub resume: bool,
    pub help: bool,
    pub version: bool,
    pub mode: Option<Mode>,
    pub name: Option<String>,
    pub no_session: bool,
    pub session: Option<String>,
    pub session_id: Option<String>,
    pub fork: Option<String>,
    pub session_dir: Option<String>,
    pub models: Vec<String>,
    pub tools: Vec<String>,
    pub exclude_tools: Vec<String>,
    pub no_tools: bool,
    pub no_builtin_tools: bool,
    pub extensions: Vec<String>,
    pub no_extensions: bool,
    pub print: bool,
    pub export: Option<String>,
    pub no_skills: bool,
    pub skills: Vec<String>,
    pub prompt_templates: Vec<String>,
    pub no_prompt_templates: bool,
    pub themes: Vec<String>,
    pub no_themes: bool,
    pub no_context_files: bool,
    pub list_models: Option<Option<String>>,
    pub offline: bool,
    pub tui_mode: Option<String>,
    pub verbose: bool,
    pub project_trust_override: Option<bool>,
    pub messages: Vec<String>,
    pub file_args: Vec<String>,
    /// Unknown flags (potentially extension flags): flag name -> value.
    pub unknown_flags: HashMap<String, Option<String>>,
    pub diagnostics: Vec<(String, String)>, // (type, message)
}

const VALID_THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

pub fn is_valid_thinking_level(level: &str) -> bool {
    VALID_THINKING_LEVELS.contains(&level)
}

pub fn parse_args(args: &[String]) -> Args {
    let mut result = Args::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        match arg.as_str() {
            "--help" | "-h" => result.help = true,
            "--version" | "-v" => result.version = true,
            "--mode" => {
                if i + 1 < args.len() {
                    i += 1;
                    match args[i].as_str() {
                        "text" => result.mode = Some(Mode::Text),
                        "json" => result.mode = Some(Mode::Json),
                        "rpc" => result.mode = Some(Mode::Rpc),
                        _ => {}
                    }
                }
            }
            "--continue" | "-c" => result.r#continue = true,
            "--resume" | "-r" => result.resume = true,
            "--provider" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.provider = Some(args[i].clone());
                }
            }
            "--model" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.model = Some(args[i].clone());
                }
            }
            "--api-key" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.api_key = Some(args[i].clone());
                }
            }
            "--system-prompt" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.system_prompt = Some(args[i].clone());
                }
            }
            "--append-system-prompt" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.append_system_prompt.push(args[i].clone());
                }
            }
            "--name" | "-n" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.name = Some(args[i].clone());
                } else {
                    result
                        .diagnostics
                        .push(("error".to_string(), "--name requires a value".to_string()));
                }
            }
            "--no-session" => result.no_session = true,
            "--session" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.session = Some(args[i].clone());
                }
            }
            "--session-id" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.session_id = Some(args[i].clone());
                }
            }
            "--fork" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.fork = Some(args[i].clone());
                }
            }
            "--session-dir" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.session_dir = Some(args[i].clone());
                }
            }
            "--models" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.models = args[i].split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            "--no-tools" | "-nt" => result.no_tools = true,
            "--no-builtin-tools" | "-nbt" => result.no_builtin_tools = true,
            "--tools" | "-t" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.tools = args[i].split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            "--exclude-tools" | "-xt" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.exclude_tools = args[i].split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            "--thinking" => {
                if i + 1 < args.len() {
                    i += 1;
                    let level = args[i].clone();
                    if is_valid_thinking_level(&level) {
                        result.thinking = Some(level);
                    } else {
                        result.diagnostics.push((
                            "warning".to_string(),
                            format!("Invalid thinking level: {level} (expected one of: off, minimal, low, medium, high, xhigh, max)"),
                        ));
                    }
                }
            }
            "--print" | "-p" => result.print = true,
            "--export" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.export = Some(args[i].clone());
                }
            }
            "--extension" | "-e" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.extensions.push(args[i].clone());
                }
            }
            "--no-extensions" | "-ne" => result.no_extensions = true,
            "--skill" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.skills.push(args[i].clone());
                }
            }
            "--prompt-template" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.prompt_templates.push(args[i].clone());
                }
            }
            "--theme" => {
                if i + 1 < args.len() {
                    i += 1;
                    result.themes.push(args[i].clone());
                }
            }
            "--no-skills" | "-ns" => result.no_skills = true,
            "--no-prompt-templates" | "-np" => result.no_prompt_templates = true,
            "--no-themes" => result.no_themes = true,
            "--no-context-files" | "-nc" => result.no_context_files = true,
            "--list-models" => {
                if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                    i += 1;
                    result.list_models = Some(Some(args[i].clone()));
                } else {
                    result.list_models = Some(None);
                }
            }
            "--tui-mode" => {
                if i + 1 < args.len() {
                    i += 1;
                    let mode = args[i].clone();
                    if mode == "regular" || mode == "fullscreen" {
                        result.tui_mode = Some(mode);
                    }
                }
            }
            "--verbose" => result.verbose = true,
            "--approve" | "-a" => result.project_trust_override = Some(true),
            "--no-approve" | "-na" => result.project_trust_override = Some(false),
            "--offline" => result.offline = true,
            _ => {
                if let Some(rest) = arg.strip_prefix('@') {
                    result.file_args.push(rest.to_string());
                } else if arg.starts_with("--") {
                    // Unknown long flag: potential extension flag.
                    if let Some(eq_index) = arg.find('=') {
                        let flag_name = arg[2..eq_index].to_string();
                        let value = arg[eq_index + 1..].to_string();
                        result.unknown_flags.insert(flag_name, Some(value));
                    } else if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                        let flag_name = arg[2..].to_string();
                        let value = args[i + 1].clone();
                        i += 1;
                        result.unknown_flags.insert(flag_name, Some(value));
                    } else {
                        let flag_name = arg[2..].to_string();
                        result.unknown_flags.insert(flag_name, None);
                    }
                } else if !arg.starts_with('-') {
                    result.messages.push(arg.clone());
                }
            }
        }
        i += 1;
    }
    result
}

/// Print the help text (port of printHelp; no extension flags in the
/// simplified port).
pub fn print_help() {
    let app = crate::config::APP_NAME;
    println!(
        r#"{app} - AI coding assistant with read, bash, edit, write tools

Usage:
  {app} [options] [@files...] [messages...]

Commands:
  {app} install <source> [-l]     Install extension source and add to settings
  {app} remove <source> [-l]      Remove extension source from settings
  {app} uninstall <source> [-l]   Alias for remove
  {app} update [source|self|pi]   Update pi, extensions, or model catalogs
  {app} list                      List installed extensions from settings
  {app} config [-l]               Open TUI to enable/disable package resources (Tab switches scope)
  {app} auth <command>            Print credentials or check provider readiness
  {app} <command> --help          Show help for install/remove/uninstall/update/list/config/auth

Options:
  --provider <name>              Provider name (default: google)
  --model <pattern>              Model pattern or ID (supports "provider/id" and optional ":<thinking>")
  --api-key <key>                API key (defaults to env vars)
  --system-prompt <text>         System prompt (default: coding assistant prompt)
  --append-system-prompt <text>  Append text or file contents to the system prompt (can be used multiple times)
  --mode <mode>                  Output mode: text (default), json, or rpc
  --print, -p                    Non-interactive mode: process prompt and exit
  --continue, -c                 Continue previous session
  --resume, -r                   Select a session to resume
  --session <path|id>            Use specific session file or partial UUID
  --session-id <id>              Use exact project session ID, creating it if missing
  --fork <path|id>               Fork specific session file or partial UUID into a new session
  --session-dir <dir>            Directory for session storage and lookup
  --no-session                   Don't save session (ephemeral)
  --name, -n <name>              Set session display name
  --models <patterns>            Comma-separated model patterns for Ctrl+P cycling
  --no-tools, -nt                Disable all tools by default (built-in and extension)
  --no-builtin-tools, -nbt       Disable built-in tools by default but keep extension/custom tools enabled
  --tools, -t <tools>            Comma-separated allowlist of tool names to enable
  --exclude-tools, -xt <tools>   Comma-separated denylist of tool names to disable
  --thinking <level>             Set thinking level: off, minimal, low, medium, high, xhigh, max
  --extension, -e <path>         Load an extension file (can be used multiple times)
  --no-extensions, -ne           Disable extension discovery (explicit -e paths still work)
  --skill <path>                 Load a skill file or directory (can be used multiple times)
  --no-skills, -ns               Disable skills discovery and loading
  --prompt-template <path>       Load a prompt template file or directory (can be used multiple times)
  --no-prompt-templates, -np     Disable prompt template discovery and loading
  --theme <path>                 Load a theme file or directory (can be used multiple times)
  --no-themes                    Disable theme discovery and loading
  --no-context-files, -nc        Disable AGENTS.md and CLAUDE.md discovery and loading
  --export <file>                Export session file to HTML and exit
  --list-models [search]         List available models (with optional fuzzy search)
  --verbose                      Force verbose startup (overrides quietStartup setting)
  --tui-mode <mode>              TUI mode: regular (default) or fullscreen
  --approve, -a                  Trust project-local files for this run
  --no-approve, -na              Ignore project-local files for this run
  --offline                      Disable startup network operations (same as PI_OFFLINE=1)
  --help, -h                     Show this help
  --version, -v                  Show version number
"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn parses_basic_flags() {
        let args = parse_args(&s(&["-p", "hello", "--provider", "acme", "--model", "m1"]));
        assert!(args.print);
        assert_eq!(args.messages, vec!["hello"]);
        assert_eq!(args.provider.as_deref(), Some("acme"));
        assert_eq!(args.model.as_deref(), Some("m1"));
    }

    #[test]
    fn parses_multi_value_flags() {
        let args = parse_args(&s(&["--tools", "read,bash", "--models", "a/*,b"]));
        assert_eq!(args.tools, vec!["read", "bash"]);
        assert_eq!(args.models, vec!["a/*", "b"]);
    }

    #[test]
    fn parses_unknown_flags() {
        let args = parse_args(&s(&["--plan", "yes", "--novalue"]));
        assert_eq!(args.unknown_flags.get("plan"), Some(&Some("yes".to_string())));
        assert_eq!(args.unknown_flags.get("novalue"), Some(&None));
    }

    #[test]
    fn parses_file_args() {
        let args = parse_args(&s(&["@prompt.md", "what is this"]));
        assert_eq!(args.file_args, vec!["prompt.md"]);
        assert_eq!(args.messages, vec!["what is this"]);
    }

    #[test]
    fn validates_thinking_level() {
        let args = parse_args(&s(&["--thinking", "bogus"]));
        assert_eq!(args.thinking, None);
        assert!(args.diagnostics.iter().any(|(t, _)| t == "warning"));
        let args2 = parse_args(&s(&["--thinking", "high"]));
        assert_eq!(args2.thinking.as_deref(), Some("high"));
    }

    #[test]
    fn name_requires_value() {
        let args = parse_args(&s(&["--name"]));
        assert_eq!(args.name, None);
        assert!(args.diagnostics.iter().any(|(t, _)| t == "error"));
    }
}
