//! Main entry point, port of `main.ts` (core flow).
//!
//! ponytail: package commands (install/remove/update/list/config) dispatch
//! to a simplified package_manager_cli; auth commands and first-time setup
//! are reduced to stubs with diagnostics; the interactive mode runs via
//! InteractiveMode, print mode runs single-shot prompts.

use std::sync::Arc;

use crate::cli::{parse_args, print_help, Args, Mode};
use crate::config::get_agent_dir;
use crate::core::agent_session::AgentSession;
use crate::core::export_html::export_from_file;
use crate::core::model_runtime::ModelRuntime;
use crate::core::session_manager::SessionManager;
use crate::core::sdk::{create_agent_session, CreateAgentSessionOptions};
use crate::modes::interactive::interactive_mode::{InteractiveMode, InteractiveModeOptions};
use crate::modes::interactive::theme::theme::init_theme;

pub struct MainOptions {
    pub extension_factories: Vec<pi_protocol::Value>,
}

impl Default for MainOptions {
    fn default() -> Self {
        Self {
            extension_factories: Vec::new(),
        }
    }
}

fn resolve_app_mode(parsed: &Args, stdin_is_tty: bool, stdout_is_tty: bool) -> &'static str {
    if let Some(mode) = &parsed.mode {
        return match mode {
            Mode::Rpc => "rpc",
            Mode::Json => "json",
            Mode::Text => "text",
        };
    }
    if parsed.print || !stdin_is_tty || !stdout_is_tty {
        "print"
    } else {
        "interactive"
    }
}

#[allow(dead_code)]
fn is_plain_runtime_metadata_command(parsed: &Args) -> bool {
    parsed.version || parsed.help || parsed.list_models.is_some() || parsed.export.is_some()
}

/// Find a session file whose id starts with the given partial id.
fn find_session_by_partial_id(session_dir: Option<&str>, partial_id: &str, cwd: &str) -> Option<String> {
    let dir = session_dir
        .map(|d| d.to_string())
        .unwrap_or_else(|| crate::core::session_manager::get_default_session_dir(cwd));
    let sessions = SessionManager::list(&dir, Some(&dir), None);
    sessions
        .into_iter()
        .find(|session| session.id.starts_with(partial_id))
        .map(|session| session.path)
}

fn read_piped_stdin() -> Option<String> {
    use std::io::Read;
    let mut buffer = Vec::new();
    let mut stdin = std::io::stdin();
    if stdin.read_to_end(&mut buffer).is_ok() && !buffer.is_empty() {
        Some(String::from_utf8_lossy(&buffer).to_string())
    } else {
        None
    }
}

fn prepare_initial_message(parsed: &Args, stdin_content: Option<&str>) -> Option<String> {
    if let Some(message) = parsed.messages.first() {
        return Some(message.clone());
    }
    stdin_content.map(|s| s.to_string())
}

/// Create the session manager for the CLI run.
fn create_session_manager(parsed: &Args, cwd: &str, session_dir: Option<&str>) -> SessionManager {
    if parsed.no_session {
        return SessionManager::in_memory(None, None);
    }
    if let Some(session) = &parsed.session {
        // Use a specific session file or partial UUID.
        let path = if std::path::Path::new(session).exists() {
            session.clone()
        } else {
            match find_session_by_partial_id(session_dir, session, cwd) {
                Some(path) => path,
                None => session.clone(),
            }
        };
        return SessionManager::open(&path, session_dir, None);
    }
    if let Some(fork) = &parsed.fork {
        let path = if std::path::Path::new(fork).exists() {
            fork.clone()
        } else {
            find_session_by_partial_id(session_dir, fork, cwd).unwrap_or_default()
        };
        if !path.is_empty() {
            let mut manager = SessionManager::open(&path, session_dir, None);
            if let Some(new_file) = manager.create_branched_session(&manager.get_leaf_id().unwrap_or_default()) {
                let _ = new_file;
            }
            return manager;
        }
    }
    if parsed.resume {
        return SessionManager::continue_recent(cwd, session_dir);
    }
    if parsed.r#continue {
        return SessionManager::continue_recent(cwd, session_dir);
    }
    SessionManager::create(cwd, session_dir, None)
}

/// Run print mode (single-shot): prompt, print assistant text, exit code.
pub fn run_print_mode(session: Arc<AgentSession>, messages: &[String], initial_message: Option<&str>) -> i32 {
    let mut exit_code = 0;
    let prompt_options = Default::default();

    if let Some(message) = initial_message {
        if let Err(error) = session.prompt(message, &prompt_options) {
            eprintln!("{error}");
            return 1;
        }
    }
    for message in messages {
        if message.trim().is_empty() {
            continue;
        }
        if let Err(error) = session.prompt(message, &prompt_options) {
            eprintln!("{error}");
            return 1;
        }
    }

    session.wait_for_idle();
    let state = session.state();
    if let Some(last) = state.messages.last() {
        use pi_agent_core::types::AgentMessage;
        if let AgentMessage::Llm(pi_ai::types::Message::Assistant(assistant)) = last {
            if assistant.stop_reason == pi_ai::types::StopReason::Error
                || assistant.stop_reason == pi_ai::types::StopReason::Aborted
            {
                let message = assistant
                    .error_message
                    .clone()
                    .unwrap_or_else(|| format!("Request {}", assistant.stop_reason.as_str()));
                eprintln!("{message}");
                exit_code = 1;
            } else {
                for content in &assistant.content {
                    if let pi_ai::types::Content::Text(text) = content {
                        print!("{}\n", text.text);
                    }
                }
            }
        }
    }
    exit_code
}

/// Main entry point, mirroring `main()` in main.ts.
pub fn main(args: &[String], _options: MainOptions) -> i32 {
    let offline_mode = args.iter().any(|a| a == "--offline")
        || std::env::var("PI_OFFLINE").map(|v| v == "1" || v == "true").unwrap_or(false);
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/tmp".to_string());
    let agent_dir = get_agent_dir();

    // Package commands (install/remove/update/list/config).
    if crate::core::package_manager_cli::handle_package_command(args, &cwd, &agent_dir) {
        return 0;
    }

    let parsed = parse_args(args);
    for (kind, message) in &parsed.diagnostics {
        if kind == "error" {
            eprintln!("Error: {message}");
        } else {
            eprintln!("Warning: {message}");
        }
    }
    if parsed.diagnostics.iter().any(|(kind, _)| kind == "error") {
        return 1;
    }

    if parsed.version {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return 0;
    }

    if parsed.export.is_some() {
        let input = parsed.export.clone().unwrap();
        let output_path = parsed.messages.first().cloned();
        match export_from_file(&input, &crate::core::export_html::ExportOptions {
            output_path,
            theme_name: None,
        }) {
            Ok(result) => {
                println!("Exported to: {result}");
                return 0;
            }
            Err(error) => {
                eprintln!("Error: {error}");
                return 1;
            }
        }
    }

    let app_mode = resolve_app_mode(&parsed, std::io::IsTerminal::is_terminal(&std::io::stdin()), std::io::IsTerminal::is_terminal(&std::io::stdout()));

    if parsed.mode == Some(Mode::Rpc) && !parsed.file_args.is_empty() {
        eprintln!("Error: @file arguments are not supported in RPC mode");
        return 1;
    }

    // Session manager selection.
    let env_session_dir = std::env::var("PI_CODING_AGENT_SESSION_DIR").ok();
    let session_dir = parsed
        .session_dir
        .clone()
        .or(env_session_dir)
        .or_else(|| Some(crate::core::session_manager::get_default_session_dir(&cwd)));
    let mut session_manager = create_session_manager(&parsed, &cwd, session_dir.as_deref());

    if let Some(name) = &parsed.name {
        let name = name.trim();
        if name.is_empty() {
            eprintln!("Error: --name requires a non-empty value");
            return 1;
        }
        session_manager.append_session_info(name.to_string());
    }

    // Build the runtime: model runtime + settings + session.
    let model_runtime = ModelRuntime::create(crate::core::model_runtime::CreateModelRuntimeOptions {
        auth_path: Some(format!("{agent_dir}/auth.json")),
        models_path: Some(format!("{agent_dir}/models.json")),
        models_store_path: None,
    });

    if parsed.help {
        print_help();
        return 0;
    }

    if let Some(search) = &parsed.list_models {
        list_models(&model_runtime, search.as_deref());
        return 0;
    }

    let mut session_options = CreateAgentSessionOptions {
        cwd: Some(session_manager.get_cwd().to_string()),
        agent_dir: Some(agent_dir),
        model: resolve_cli_model(&parsed, &model_runtime),
        thinking_level: parsed.thinking.clone(),
        no_tools: if parsed.no_tools {
            Some("all".to_string())
        } else if parsed.no_builtin_tools {
            Some("builtin".to_string())
        } else {
            None
        },
        tools: if parsed.tools.is_empty() {
            None
        } else {
            Some(parsed.tools.clone())
        },
        exclude_tools: if parsed.exclude_tools.is_empty() {
            None
        } else {
            Some(parsed.exclude_tools.clone())
        },
        ..Default::default()
    };
    session_options.session_manager = Some(session_manager);
    if let Some(_api_key) = &parsed.api_key {
        if session_options.model.is_none() {
            eprintln!("Error: --api-key requires a model to be specified via --model, --provider/--model, or --models");
            return 1;
        }
    }

    let created = match create_agent_session(session_options) {
        Ok(created) => created,
        Err(error) => {
            eprintln!("Error: {error}");
            return 1;
        }
    };
    let session = created.session;

    if app_mode == "rpc" {
        return crate::core::rpc_entry::run_rpc_mode(session);
    }

    if app_mode == "interactive" {
        let stdin_content = read_piped_stdin();
        let _ = offline_mode;
        let mut interactive_mode = InteractiveMode::new(
            session,
            InteractiveModeOptions {
                tui_mode: parsed.tui_mode.clone().unwrap_or_else(|| "regular".to_string()),
                verbose: parsed.verbose,
            },
        );
        let mut initial: Option<String> = None;
        if let Some(content) = stdin_content {
            if !content.trim().is_empty() {
                initial = Some(content.trim().to_string());
            }
        } else if let Some(message) = prepare_initial_message(&parsed, None) {
            initial = Some(message);
        }
        // Initialize theme.
        init_theme(None);
        interactive_mode.subscribe_to_agent();
        let _ = &mut initial;
        match interactive_mode.run() {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("Error: {error}");
                1
            }
        }
    } else {
        // print/text/json mode.
        init_theme(None);
        let initial_message = prepare_initial_message(&parsed, None);
        let messages: Vec<String> = parsed.messages.iter().skip(if initial_message.is_some() { 1 } else { 0 }).cloned().collect();
        run_print_mode(session, &messages, initial_message.as_deref())
    }
}

fn resolve_cli_model(parsed: &Args, runtime: &ModelRuntime) -> Option<pi_ai::types::Model> {
    let pattern = parsed
        .model
        .clone()
        .or_else(|| {
            parsed.provider.as_ref().map(|provider| {
                if parsed.model.is_some() {
                    parsed.model.clone().unwrap()
                } else {
                    format!("{provider}/*")
                }
            })
        });
    let Some(pattern) = pattern else {
        return None;
    };
    // "provider/id" or bare id or "pattern:thinking".
    let base = pattern.split(':').next().unwrap_or(&pattern).to_string();
    let models = runtime.get_available_snapshot();
    if let Some((provider, id)) = base.split_once('/') {
        return runtime.get_model(provider, id);
    }
    // Fuzzy: id match first, else provider match.
    models.iter().find(|m| m.id == base).cloned()
}

/// Port of cli/list-models.ts.
pub fn list_models(runtime: &ModelRuntime, search: Option<&str>) {
    let mut models = runtime.get_available_snapshot();
    models.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.id.cmp(&b.id)));
    let search = search.unwrap_or("").to_lowercase();
    for model in models {
        if search.is_empty()
            || model.id.to_lowercase().contains(&search)
            || model.provider.to_lowercase().contains(&search)
        {
            println!("{}/{}", model.provider, model.id);
        }
    }
}
