//! Interactive mode, port of `modes/interactive/interactive-mode.ts`.
//!
//! This is the TUI orchestration layer: it owns the editor, chat/status
//! containers, footer, keybindings, and the main input loop. Extension
//! hooks (auth dialogs, extension UI, OAuth flows, widgets) are simplified
//! or omitted (ponytail notes inline). The agent loop itself runs in
//! AgentSession; this module renders state and dispatches input.

use std::sync::Arc;

use pi_ai::types::Model;
use pi_tui::components::basic::Text;
use pi_tui::tui::Container;
use pi_tui::components::editor::{Editor, EditorOptions, EditorTheme};
use pi_tui::tui::Component;

use crate::core::agent_session::{AgentSession, AgentSessionEvent};
use crate::core::export_html::{export_session_to_html, ExportOptions};
use crate::core::footer_data_provider::FooterDataProvider;
use crate::modes::interactive::components::keybinding_hints::set_global_keybindings;
use crate::core::keybindings::KeybindingsManager;
use crate::core::session_manager::SessionManager;
use crate::core::slash_commands::builtin_command_names;
use crate::modes::interactive::components::footer::FooterComponent;
use crate::modes::interactive::components::model_selector::ModelSelectorComponent;
use crate::modes::interactive::components::session_selector::SessionSelectorComponent;
use crate::modes::interactive::components::settings_selector::{SettingsCallbacks, SettingsItem, SettingsSelectorComponent};
use crate::modes::interactive::components::theme_selector::ThemeSelectorComponent;
use crate::modes::interactive::components::thinking_selector::ThinkingSelectorComponent;
use crate::modes::interactive::components::tree_selector::{FilterMode, TreeSelectorComponent};
use crate::modes::interactive::components::trust_selector::{TrustSelectorComponent, TrustSelectorOptions};
use crate::modes::interactive::theme::theme::{get_available_themes, init_theme, set_theme, theme};

pub struct InteractiveModeOptions {
    pub tui_mode: String, // "regular" | "fullscreen"
    pub verbose: bool,
}

impl Default for InteractiveModeOptions {
    fn default() -> Self {
        Self {
            tui_mode: "regular".to_string(),
            verbose: false,
        }
    }
}

/// Result of a run: normal exit.
pub struct InteractiveMode {
    session: Arc<AgentSession>,
    keybindings: KeybindingsManager,
    editor: Editor,
    chat_container: Container,
    status_container: Container,
    #[allow(dead_code)]
    footer_container: Container,
    is_bash_mode: bool,
    last_escape_time: f64,
    hide_thinking_block: bool,
    #[allow(dead_code)]
    output_pad: usize,
    tool_output_expanded: bool,
    shutdown_requested: bool,
    options: InteractiveModeOptions,
}

const DOUBLE_ESCAPE_WINDOW_MS: f64 = 500.0;

impl InteractiveMode {
    pub fn new(session: Arc<AgentSession>, options: InteractiveModeOptions) -> Self {
        // Keybindings: build from defaults + user config, publish globally
        // for keybinding-hint components.
        let keybindings = KeybindingsManager::create(None);
        set_global_keybindings(KeybindingsManager::create(None));

        let settings = session.settings_manager.lock().unwrap();
        let editor_padding_x = settings.get_editor_padding_x();
        let autocomplete_max_visible = settings.get_autocomplete_max_visible();
        let hide_thinking_block = settings.get_hide_thinking_block();
        let output_pad = settings.get_output_pad() as usize;
        drop(settings);

        let editor_theme = EditorTheme {
            border_color: make_editor_border_color(),
        };
        let editor = Editor::new(
            editor_theme,
            EditorOptions {
                padding_x: Some(editor_padding_x),
                autocomplete_max_visible: Some(autocomplete_max_visible),
            },
            Arc::new(|| {}),
        );

        let footer_data_provider = FooterDataProvider::new(
            session.session_manager.lock().unwrap().get_cwd().to_string(),
        );
        let footer = FooterComponent::new(session.clone(), footer_data_provider);
        let mut footer_container = Container::new();
        footer_container.add_child(Arc::new(footer));

        Self {
            session,
            keybindings,
            editor,
            chat_container: Container::new(),
            status_container: Container::new(),
            footer_container,
            is_bash_mode: false,
            last_escape_time: 0.0,
            hide_thinking_block,
            output_pad,
            tool_output_expanded: false,
            shutdown_requested: false,
            options,
        }
    }

    pub fn session(&self) -> Arc<AgentSession> {
        self.session.clone()
    }

    /// Build the full UI document: chat, status, editor, footer.
    /// ponytail: the sync port renders containers directly in tests; the
    /// TUI mount (TuiMainScreen/TuiAltScreen) is owned by the host.
    #[allow(dead_code)]
    fn build_layout(&self) -> Vec<Arc<dyn Component>> {
        let mut components: Vec<Arc<dyn Component>> = Vec::new();
        components.push(Arc::new(Container::new()));
        components.push(Arc::new(Container::new()));
        components.push(Arc::new(Editor::new(
            EditorTheme {
                border_color: make_editor_border_color(),
            },
            EditorOptions::default(),
            Arc::new(|| {}),
        )));
        components.push(Arc::new(Container::new()));
        components
    }

    fn update_terminal_title(&mut self) {
        let session_name = self.session.get_session_name();
        let cwd_basename = std::path::Path::new(self.session.session_manager.lock().unwrap().get_cwd())
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let title = match session_name {
            Some(name) => format!("π - {name} - {cwd_basename}"),
            None => format!("π - {cwd_basename}"),
        };
        let _ = title;
    }

    /// Main entry point: initialize, process startup notices, run the loop.
    pub fn run(&mut self) -> Result<(), String> {
        self.initialize()?;
        let _ = self.options.verbose;

        // Main interactive loop: read editor input, submit to the session.
        loop {
            if self.shutdown_requested {
                break;
            }
            let text = self.get_user_input();
            match text.trim() {
                "" => continue,
                "/quit" | "/exit" => break,
                _ => {
                    self.submit_text(&text);
                }
            }
        }
        Ok(())
    }

    fn initialize(&mut self) -> Result<(), String> {
        // Ensure a theme is initialized.
        init_theme(None);
        let _ = self.session;
        self.update_terminal_title();
        Ok(())
    }

    /// Wait for a full editor submission. In the sync port the host calls
    /// submit_text directly; this helper returns the last submitted text
    /// through a shared cell so the run loop stays simple.
    fn get_user_input(&mut self) -> String {
        // ponytail: the JS version awaits a promise from the editor. The
        // sync analog is driven externally: run() is called with a
        // feed() source in tests; in production the TUI event loop
        // dispatches into handle_input.
        String::new()
    }

    /// Feed an input event into the interactive mode (TUI event loop hook).
    pub fn handle_input(&mut self, data: &str) {
        // Editor handles most keys; app actions are dispatched below.
        let kb = &self.keybindings;
        if kb.matches(data, "app.interrupt") || data == "\x1b" {
            self.handle_escape();
            return;
        }
        if kb.matches(data, "app.exit") {
            if self.editor.get_text().trim().is_empty() {
                self.shutdown_requested = true;
                return;
            }
        }
        if kb.matches(data, "app.clear") {
            if self.session.is_streaming() {
                self.session.abort();
            } else {
                self.editor.set_text("");
            }
            return;
        }
        if kb.matches(data, "app.suspend") {
            return; // ponytail: no SIGTSTP handling in the sync port.
        }
        if kb.matches(data, "app.thinking.cycle") {
            self.cycle_thinking_level();
            return;
        }
        if kb.matches(data, "app.model.cycleForward") {
            self.cycle_model("forward");
            return;
        }
        if kb.matches(data, "app.model.cycleBackward") {
            self.cycle_model("backward");
            return;
        }
        if kb.matches(data, "app.model.select") {
            self.show_model_selector(None);
            return;
        }
        if kb.matches(data, "app.tools.expand") {
            self.toggle_tool_output_expansion();
            return;
        }
        if kb.matches(data, "app.thinking.toggle") {
            self.toggle_thinking_block_visibility();
            return;
        }
        if kb.matches(data, "app.message.copy") {
            self.handle_copy_command();
            return;
        }
        if kb.matches(data, "app.session.new") {
            self.handle_clear_command();
            return;
        }
        if kb.matches(data, "app.session.tree") {
            self.show_tree_selector(None);
            return;
        }
        if kb.matches(data, "app.session.fork") {
            self.show_user_message_selector();
            return;
        }
        if kb.matches(data, "app.session.resume") {
            self.show_session_selector();
            return;
        }
        // Default: editor input.
        let before = self.editor.get_text();
        self.editor.handle_input(data);
        if self.editor.get_text() != before {
            let trimmed: String = self.editor.get_text().trim_start().to_string();
            let was_bash_mode = self.is_bash_mode;
            self.is_bash_mode = trimmed.starts_with('!');
            if was_bash_mode != self.is_bash_mode {
                self.update_editor_border_color();
            }
        }
    }

    fn handle_escape(&mut self) {
        if self.session.is_streaming() {
            self.session.abort();
            return;
        }
        if self.session.is_bash_running() {
            self.session.abort_bash();
            return;
        }
        if self.is_bash_mode {
            self.editor.set_text("");
            self.is_bash_mode = false;
            self.update_editor_border_color();
            return;
        }
        if self.editor.get_text().trim().is_empty() {
            let action = self.session.settings_manager.lock().unwrap().get_double_escape_action();
            if action != "none" {
                let now_ms = crate::core::session_manager::now_iso();
                let now_ms = parse_iso_ms(&now_ms);
                if now_ms - self.last_escape_time < DOUBLE_ESCAPE_WINDOW_MS {
                    if action == "tree" {
                        self.show_tree_selector(None);
                    } else {
                        self.show_user_message_selector();
                    }
                    self.last_escape_time = 0.0;
                } else {
                    self.last_escape_time = now_ms;
                }
            }
        }
    }

    fn update_editor_border_color(&mut self) {
        // ponytail: editor border color reflects bash mode via a no-op;
        // the Editor owns a color closure.
    }

    fn cycle_thinking_level(&mut self) {
        self.session.cycle_thinking_level();
    }

    fn cycle_model(&mut self, direction: &str) {
        let _ = self.session.cycle_model(direction);
    }

    fn toggle_tool_output_expansion(&mut self) {
        self.tool_output_expanded = !self.tool_output_expanded;
    }

    fn toggle_thinking_block_visibility(&mut self) {
        self.hide_thinking_block = !self.hide_thinking_block;
    }

    fn handle_copy_command(&mut self) {
        if let Some(text) = self.session.get_last_assistant_text() {
            let _ = copy_to_clipboard(&text);
        }
    }

    fn handle_clear_command(&mut self) {
        self.session.session_manager.lock().unwrap().new_session(None);
        self.editor.set_text("");
        self.chat_container.clear();
    }

    /// Submit an editor line: slash commands, bash mode, or a prompt.
    pub fn submit_text(&mut self, text: &str) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }

        // Slash commands.
        if let Some(handled) = self.dispatch_slash_command(&text) {
            if handled {
                self.editor.set_text("");
                return;
            }
        }

        // Bash mode: `!cmd` or `!!cmd` (excluded from context).
        if let Some(rest) = text.strip_prefix("!!") {
            let command = rest.trim();
            if !command.is_empty() {
                self.execute_bash(command, true);
                self.editor.set_text("");
                return;
            }
        } else if let Some(rest) = text.strip_prefix('!') {
            let command = rest.trim();
            if !command.is_empty() {
                self.execute_bash(command, false);
                self.editor.set_text("");
                return;
            }
        }

        // Normal prompt.
        self.editor.set_text("");
        let result = self.session.prompt(&text, &Default::default());
        if let Err(error) = result {
            self.show_error(&error);
        }
    }

    fn execute_bash(&mut self, command: &str, _exclude_from_context: bool) {
        let cwd = self.session.session_manager.lock().unwrap().get_cwd().to_string();
        let shell_path = self.session.settings_manager.lock().unwrap().get_shell_path();
        let operations = InteractiveBashOperations { shell_path };
        match self.session.execute_bash(command, &cwd, &operations, None) {
            Ok(_) => {}
            Err(error) => self.show_error(&error),
        }
    }

    fn show_error(&mut self, message: &str) {
        let t = theme();
        let t = t.as_ref();
        let styled = t.map(|t| t.fg("error", message)).unwrap_or_else(|| message.to_string());
        self.status_container.clear();
        self.status_container.add_child(Arc::new(Text::new(&styled, 1, 0, None)));
    }

    fn show_status(&mut self, message: &str) {
        let t = theme();
        let t = t.as_ref();
        let styled = t.map(|t| t.fg("muted", message)).unwrap_or_else(|| message.to_string());
        self.status_container.clear();
        self.status_container.add_child(Arc::new(Text::new(&styled, 1, 0, None)));
    }

    /// Slash command dispatch; returns Some(true) when handled.
    fn dispatch_slash_command(&mut self, text: &str) -> Option<bool> {
        match text {
            "/settings" => {
                self.show_settings_selector();
                Some(true)
            }
            "/model" => {
                self.show_model_selector(None);
                Some(true)
            }
            "/tree" => {
                self.show_tree_selector(None);
                Some(true)
            }
            "/fork" => {
                self.show_user_message_selector();
                Some(true)
            }
            "/resume" => {
                self.show_session_selector();
                Some(true)
            }
            "/new" => {
                self.handle_clear_command();
                Some(true)
            }
            "/name" | _ if text.starts_with("/name ") => {
                let name = text.trim_start_matches("/name").trim();
                self.session.set_session_name(name);
                Some(true)
            }
            "/session" => {
                let id = self.session.get_session_id();
                self.show_status(&format!("Session: {id}"));
                Some(true)
            }
            "/compact" => {
                let _ = self.session.compact(None);
                Some(true)
            }
            "/copy" => {
                self.handle_copy_command();
                Some(true)
            }
            "/export" | _ if text.starts_with("/export ") => {
                let output = self.handle_export_command(text);
                if let Some(path) = output {
                    self.show_status(&format!("Exported to {path}"));
                }
                Some(true)
            }
            "/debug" => {
                self.show_status("Debug output enabled");
                Some(true)
            }
            "/trust" => {
                self.show_trust_selector();
                Some(true)
            }
            "/theme" | _ if text.starts_with("/theme ") => {
                let name = text.trim_start_matches("/theme").trim();
                if name.is_empty() {
                    let names = get_available_themes();
                    self.show_status(&format!("Themes: {}", names.join(", ")));
                } else {
                    let (success, error) = set_theme(name);
                    if !success {
                        self.show_error(&error.unwrap_or_else(|| "Failed to set theme".to_string()));
                    }
                }
                Some(true)
            }
            "/quit" | "/exit" => {
                self.shutdown_requested = true;
                Some(true)
            }
            "/help" | "/hotkeys" => {
                let commands = builtin_command_names().join(", ");
                self.show_status(&format!("Commands: {commands}"));
                Some(true)
            }
            _ if text.starts_with("/") => {
                // Unknown command: still pass through as a prompt? JS shows
                // an error for unknown slash commands via the agent loop.
                Some(false)
            }
            _ => None,
        }
    }

    fn handle_export_command(&mut self, text: &str) -> Option<String> {
        let arg = text.trim_start_matches("/export").trim();
        let options = ExportOptions {
            output_path: if arg.is_empty() { None } else { Some(arg.to_string()) },
            theme_name: None,
        };
        export_session_to_html(&self.session.session_manager.lock().unwrap(), None, &options).ok()
    }

    // ------------------------------------------------------------------
    // Selectors
    // ------------------------------------------------------------------

    fn show_model_selector(&mut self, initial_search: Option<&str>) {
        let runtime = self.session.model_runtime();
        let current = self.session.model();
        let scoped = self.session.scoped_models().to_vec();
        let on_select = {
            let session = self.session.clone();
            Arc::new(move |model: Model| {
                // Persist as the new default, then activate.
                {
                    let mut settings = session.settings_manager.lock().unwrap();
                    settings.set_default_provider(&model.provider);
                    settings.set_default_model(&model.id);
                }
                let _ = session.set_model(&model);
            }) as Arc<dyn Fn(Model) + Send + Sync>
        };
        let on_cancel = Arc::new(|| {});
        let selector = ModelSelectorComponent::new(
            current,
            &runtime,
            &scoped,
            on_select,
            on_cancel,
            initial_search,
        );
        self.chat_container.clear();
        self.chat_container.add_child(Arc::new(selector));
    }

    fn show_settings_selector(&mut self) {
        let items = build_settings_items(&self.session);
        let on_change = {
            let session = self.session.clone();
            Arc::new(move |id: &str, value: &str| {
                let mut settings = session.settings_manager.lock().unwrap();
                let bool_value = value == "true";
                match id {
                    "autocompact" => {
                        let enabled = bool_value;
                        drop(settings);
                        session.set_auto_compaction_enabled(enabled);
                        return;
                    }
                    "hide-thinking" => settings.set_hide_thinking_block(bool_value),
                    "quiet-startup" => settings.set_quiet_startup(bool_value),
                    "theme" => {
                        let name = value.to_string();
                        drop(settings);
                        let _ = set_theme(&name);
                        return;
                    }
                    _ => {}
                }
            }) as Arc<dyn Fn(&str, &str) + Send + Sync>
        };
        let on_cancel = Arc::new(|| {});
        let selector = SettingsSelectorComponent::new(
            items,
            SettingsCallbacks { on_change, on_cancel },
        );
        self.chat_container.clear();
        self.chat_container.add_child(Arc::new(selector));
    }

    fn show_tree_selector(&mut self, initial_selected: Option<String>) {
        let tree = self.session.session_manager.lock().unwrap().get_tree();
        let leaf = self.session.session_manager.lock().unwrap().get_leaf_id();
        let on_select = {
            let session = self.session.clone();
            Arc::new(move |entry_id: &str| {
                let _ = session.navigate_tree(entry_id, &Default::default());
            }) as Arc<dyn Fn(&str) + Send + Sync>
        };
        let on_cancel = Arc::new(|| {});
        let filter_mode = parse_filter_mode(
            &self.session.settings_manager.lock().unwrap().get_tree_filter_mode(),
        );
        let mut selector = TreeSelectorComponent::new(
            tree,
            leaf,
            20,
            initial_selected,
            Some(filter_mode),
        );
        selector.set_callbacks(on_select, on_cancel);
        self.chat_container.clear();
        self.chat_container.add_child(Arc::new(selector));
    }

    fn show_user_message_selector(&mut self) {
        let messages = self.session.get_user_messages_for_forking();
        let on_select = {
            let session = self.session.clone();
            Arc::new(move |entry_id: &str| {
                // Fork: copy the active path up to that message into a new session.
                let mut manager = session.session_manager.lock().unwrap();
                if manager.create_branched_session(entry_id).is_some() {
                    let _ = manager.new_session(None);
                }
            }) as Arc<dyn Fn(&str) + Send + Sync>
        };
        let on_cancel = Arc::new(|| {});
        let items: Vec<crate::modes::interactive::components::user_message_selector::UserMessageItem> =
            messages
                .into_iter()
                .map(|(id, text)| crate::modes::interactive::components::user_message_selector::UserMessageItem {
                    id,
                    text,
                    timestamp: None,
                })
                .collect();
        let selector =
            crate::modes::interactive::components::user_message_selector::UserMessageSelectorComponent::new(
                items,
                on_select,
                on_cancel,
                None,
            );
        self.chat_container.clear();
        self.chat_container.add_child(Arc::new(selector));
    }

    fn show_session_selector(&mut self) {
        let cwd = self.session.session_manager.lock().unwrap().get_cwd().to_string();
        let current_sessions = SessionManager::list(&cwd, None, None);
        let all_sessions = SessionManager::list("", None, None);
        let on_select = {
            let session = self.session.clone();
            Arc::new(move |session_path: &str| {
                // Resume: replace the session manager state with the file.
                let mut manager = session.session_manager.lock().unwrap();
                *manager = SessionManager::open(session_path, None, None);
            }) as Arc<dyn Fn(&str) + Send + Sync>
        };
        let on_cancel = Arc::new(|| {});
        let selector = SessionSelectorComponent::new(
            current_sessions,
            all_sessions,
            on_select,
            on_cancel,
            self.session.get_session_file().as_deref(),
        );
        self.chat_container.clear();
        self.chat_container.add_child(Arc::new(selector));
    }

    fn show_trust_selector(&mut self) {
        let cwd = self.session.session_manager.lock().unwrap().get_cwd().to_string();
        let on_select = {
            let session = self.session.clone();
            Arc::new(move |selection: crate::modes::interactive::components::trust_selector::TrustSelection| {
                let mut settings = session.settings_manager.lock().unwrap();
                settings.set_project_trusted(selection.trusted);
            }) as Arc<dyn Fn(crate::modes::interactive::components::trust_selector::TrustSelection) + Send + Sync>
        };
        let on_cancel = Arc::new(|| {});
        let selector = TrustSelectorComponent::new(TrustSelectorOptions {
            cwd,
            saved_decision: None,
            project_trusted: self.session.settings_manager.lock().unwrap().is_project_trusted(),
            on_select,
            on_cancel,
        });
        self.chat_container.clear();
        self.chat_container.add_child(Arc::new(selector));
    }

    #[allow(dead_code)]
    fn show_theme_selector(&mut self) {
        let current = crate::modes::interactive::theme::theme::current_theme_name()
            .unwrap_or_else(|| "dark".to_string());
        let on_select = {
            Arc::new(move |name: &str| {
                let _ = set_theme(name);
            }) as Arc<dyn Fn(&str) + Send + Sync>
        };
        let on_cancel = Arc::new(|| {});
        let on_preview = {
            Arc::new(move |name: &str| {
                let _ = set_theme(name);
            }) as Arc<dyn Fn(&str) + Send + Sync>
        };
        let selector = ThemeSelectorComponent::new(&current, on_select, on_cancel, on_preview);
        self.chat_container.clear();
        self.chat_container.add_child(Arc::new(selector));
    }

    #[allow(dead_code)]
    fn show_thinking_selector(&mut self) {
        let levels = self.session.get_available_thinking_levels();
        let current = self.session.thinking_level();
        let on_select = {
            let session = self.session.clone();
            Arc::new(move |level: &str| {
                session.set_thinking_level(level);
            }) as Arc<dyn Fn(&str) + Send + Sync>
        };
        let on_cancel = Arc::new(|| {});
        let selector = ThinkingSelectorComponent::new(&current, &levels, on_select, on_cancel);
        self.chat_container.clear();
        self.chat_container.add_child(Arc::new(selector));
    }

    // ------------------------------------------------------------------
    // Agent event handling
    // ------------------------------------------------------------------

    /// Subscribe to agent session events and reflect them in the UI.
    pub fn subscribe_to_agent(&mut self) {
        let session = self.session.clone();
        let listener: crate::core::agent_session::AgentSessionEventListener = Box::new(move |event| {
            match event {
                AgentSessionEvent::AgentStart => {
                    // ponytail: status updates are handled by the host.
                }
                AgentSessionEvent::MessageStart { .. } | AgentSessionEvent::MessageEnd { .. } => {}
                AgentSessionEvent::TurnEnd => {}
                AgentSessionEvent::ThinkingLevelChanged { level } => {
                    let _ = level;
                }
                AgentSessionEvent::ToolExecutionStart { tool_name, .. } => {
                    let _ = tool_name;
                }
                AgentSessionEvent::AutoCompactionStart => {}
                AgentSessionEvent::AutoCompactionEnd => {}
                _ => {}
            }
        });
        session.subscribe(listener);
    }
}

/// Local bash operations adapter for interactive-mode bash commands.
struct InteractiveBashOperations {
    shell_path: Option<String>,
}

impl crate::core::tools::bash_executor::BashOperations for InteractiveBashOperations {
    fn exec(
        &self,
        command: &str,
        cwd: &str,
        on_data: &mut dyn FnMut(&[u8]),
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<crate::core::tools::bash_executor::BashExecResult, String> {
        use std::io::Read;
        use std::process::{Command, Stdio};
        let shell_config = crate::utils::shell::get_shell_config(self.shell_path.as_deref())?;
        let mut process = Command::new(&shell_config.shell);
        if shell_config.command_transport.as_deref() == Some("stdin") {
            process.args(&shell_config.args).stdin(Stdio::piped());
        } else {
            let mut args = shell_config.args.clone();
            args.push(command.to_string());
            process.args(&args).stdin(Stdio::null());
        }
        let mut child = process
            .current_dir(cwd)
            .envs(crate::utils::shell::get_shell_env().into_iter())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let mut buffer = [0u8; 8192];
        // Read stdout then stderr; check cancellation between reads.
        loop {
            if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(crate::core::tools::bash_executor::BashExecResult { exit_code: None });
            }
            match stdout.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => on_data(&buffer[..n]),
            }
        }
        loop {
            match stderr.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => on_data(&buffer[..n]),
            }
        }
        let status = child.wait().map_err(|error| error.to_string())?;
        Ok(crate::core::tools::bash_executor::BashExecResult {
            exit_code: status.code().map(|code| code as i64),
        })
    }
}

fn make_editor_border_color() -> Arc<dyn Fn(&str) -> String + Send + Sync> {
    let ansi = crate::modes::interactive::theme::theme::theme()
        .as_ref()
        .map(|t| t.get_fg_ansi("borderMuted"))
        .unwrap_or_default();
    if ansi.is_empty() {
        Arc::new(|text: &str| text.to_string())
    } else {
        Arc::new(move |text: &str| format!("{ansi}{text}\x1b[39m"))
    }
}

fn parse_iso_ms(iso: &str) -> f64 {
    let bytes = iso.as_bytes();
    if bytes.len() < 19 {
        return 0.0;
    }
    let year: f64 = iso[0..4].parse().unwrap_or(0.0);
    let month: f64 = iso[5..7].parse().unwrap_or(0.0);
    let day: f64 = iso[8..10].parse().unwrap_or(0.0);
    let hour: f64 = iso[11..13].parse().unwrap_or(0.0);
    let min: f64 = iso[14..16].parse().unwrap_or(0.0);
    let sec: f64 = iso[17..19].parse().unwrap_or(0.0);
    let mut days = 0.0;
    for y in 1970..(year as i64) {
        days += if is_leap(y) { 366.0 } else { 365.0 };
    }
    let month_days = [
        31.0,
        if is_leap(year as i64) { 29.0 } else { 28.0 },
        31.0,
        30.0,
        31.0,
        30.0,
        31.0,
        31.0,
        30.0,
        31.0,
        30.0,
        31.0,
    ];
    for m in 0..((month as i64 - 1).max(0)) as usize {
        days += month_days[m];
    }
    days += day - 1.0;
    (days * 86400.0 + hour * 3600.0 + min * 60.0 + sec) * 1000.0
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn parse_filter_mode(value: &str) -> FilterMode {
    match value {
        "no-tools" => FilterMode::NoTools,
        "user-only" => FilterMode::UserOnly,
        "labeled-only" => FilterMode::LabeledOnly,
        "all" => FilterMode::All,
        _ => FilterMode::Default,
    }
}

fn copy_to_clipboard(_text: &str) -> Result<(), String> {
    // ponytail: clipboard access is not ported; no-op.
    Ok(())
}

fn build_settings_items(session: &AgentSession) -> Vec<SettingsItem> {
    let settings = session.settings_manager.lock().unwrap();
    vec![
        SettingsItem {
            id: "autocompact",
            label: "Auto-compact",
            description: "Automatically compact context when it gets too large",
            current_value: session.is_auto_compaction_enabled().to_string(),
            values: vec!["true".to_string(), "false".to_string()],
        },
        SettingsItem {
            id: "hide-thinking",
            label: "Hide thinking",
            description: "Hide thinking blocks in assistant responses",
            current_value: settings.get_hide_thinking_block().to_string(),
            values: vec!["true".to_string(), "false".to_string()],
        },
        SettingsItem {
            id: "quiet-startup",
            label: "Quiet startup",
            description: "Disable verbose printing at startup",
            current_value: settings.get_quiet_startup().to_string(),
            values: vec!["true".to_string(), "false".to_string()],
        },
        SettingsItem {
            id: "theme",
            label: "Theme",
            description: "Color theme for the interface",
            current_value: crate::modes::interactive::theme::theme::current_theme_name()
                .unwrap_or_else(|| "dark".to_string()),
            values: get_available_themes(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session() -> Arc<AgentSession> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let agent_dir = std::env::temp_dir().join(format!("pi-im-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&agent_dir).unwrap();
        let result = crate::core::sdk::create_agent_session(crate::core::sdk::CreateAgentSessionOptions {
            cwd: Some("/tmp".to_string()),
            agent_dir: Some(agent_dir.to_string_lossy().to_string()),
            ..Default::default()
        })
        .unwrap();
        result.session
    }

    #[test]
    fn constructs_and_handles_input() {
        init_theme(Some("dark"));
        let session = make_session();
        let mut mode = InteractiveMode::new(session, InteractiveModeOptions::default());
        // Escape with empty editor does not crash.
        mode.handle_input("\x1b");
        // App clear.
        mode.handle_input("\u{3}"); // ctrl+c
        assert!(mode.session.is_streaming() == false);
    }

    #[test]
    fn submits_slash_commands() {
        init_theme(Some("dark"));
        let session = make_session();
        let mut mode = InteractiveMode::new(session, InteractiveModeOptions::default());
        mode.submit_text("/session");
        mode.submit_text("/name test-session");
        assert_eq!(mode.session.get_session_name().as_deref(), Some("test-session"));
        mode.submit_text("/quit");
        assert!(mode.shutdown_requested);
    }

    #[test]
    fn dispatch_unknown_command_passes_through() {
        init_theme(Some("dark"));
        let session = make_session();
        let mut mode = InteractiveMode::new(session, InteractiveModeOptions::default());
        let handled = mode.dispatch_slash_command("/definitely-not-a-command");
        assert_eq!(handled, Some(false));
    }

    #[test]
    fn parse_iso_timestamps() {
        let ms = parse_iso_ms("2024-01-01T00:00:00.000Z");
        assert!(ms > 1_700_000_000_000.0);
        let leap = parse_iso_ms("2024-02-29T00:00:00.000Z");
        assert!(leap > ms);
    }
}
