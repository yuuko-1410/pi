//! AgentSession runtime owner, port of `core/agent-session-runtime.ts`.
//! Session replacement flows (switch/new/fork/import/dispose) tear down the
//! current session and apply the next runtime created by the factory.
//! Extension session events flow through the ExtensionRunner emit surface.

use std::path::PathBuf;

use super::agent_session::AgentSession;
use super::model_runtime::ModelRuntime;
use super::resource_loader::DefaultResourceLoader;
use super::sdk::CreateAgentSessionResult;
use super::session_cwd::assert_session_cwd_exists;
use super::session_manager::SessionManager;
use super::settings_manager::SettingsManager;

pub struct AgentSessionRuntimeDiagnostic {
    pub r#type: String, // "info" | "warning" | "error"
    pub message: String,
}

/// Coherent cwd-bound runtime services for one effective session cwd.
pub struct AgentSessionServices {
    pub cwd: String,
    pub agent_dir: String,
    pub model_runtime: ModelRuntime,
    pub settings_manager: SettingsManager,
    pub resource_loader: DefaultResourceLoader,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

pub struct CreateAgentSessionRuntimeResult {
    pub session: std::sync::Arc<AgentSession>,
    pub services: AgentSessionServices,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    pub model_fallback_message: Option<String>,
}

pub struct RuntimeCreateOptions {
    pub cwd: String,
    pub agent_dir: String,
    pub session_manager: SessionManager,
    pub session_start_reason: Option<String>,
    pub previous_session_file: Option<String>,
}

/// Thrown when /import references a JSONL file path that does not exist.
pub struct SessionImportFileNotFoundError {
    pub file_path: String,
}

impl std::fmt::Display for SessionImportFileNotFoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "File not found: {}", self.file_path)
    }
}

fn extract_user_message_text(content: &pi_ai::types::UserMessageContent) -> String {
    match content {
        pi_ai::types::UserMessageContent::Text(text) => text.clone(),
        pi_ai::types::UserMessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                pi_ai::types::Content::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect(),
    }
}

/// Owns the current AgentSession plus its cwd-bound services.
pub struct AgentSessionRuntime {
    session: std::sync::Arc<AgentSession>,
    services: AgentSessionServices,
    create_runtime: Box<dyn Fn(RuntimeCreateOptions) -> Result<CreateAgentSessionRuntimeResult, String> + Send + Sync>,
    diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    model_fallback_message: Option<String>,
    rebind_session: Option<Box<dyn Fn(&AgentSession) -> Result<(), String> + Send + Sync>>,
    before_session_invalidate: Option<Box<dyn Fn() + Send + Sync>>,
}

impl AgentSessionRuntime {
    pub fn new(
        session: std::sync::Arc<AgentSession>,
        services: AgentSessionServices,
        create_runtime: Box<dyn Fn(RuntimeCreateOptions) -> Result<CreateAgentSessionRuntimeResult, String> + Send + Sync>,
        diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
        model_fallback_message: Option<String>,
    ) -> Self {
        Self {
            session,
            services,
            create_runtime,
            diagnostics,
            model_fallback_message,
            rebind_session: None,
            before_session_invalidate: None,
        }
    }

    pub fn session(&self) -> &std::sync::Arc<AgentSession> {
        &self.session
    }

    pub fn cwd(&self) -> &str {
        &self.services.cwd
    }

    pub fn diagnostics(&self) -> &[AgentSessionRuntimeDiagnostic] {
        &self.diagnostics
    }

    pub fn model_fallback_message(&self) -> Option<&str> {
        self.model_fallback_message.as_deref()
    }

    pub fn set_rebind_session(&mut self, rebind: Option<Box<dyn Fn(&AgentSession) -> Result<(), String> + Send + Sync>>) {
        self.rebind_session = rebind;
    }

    pub fn set_before_session_invalidate(&mut self, callback: Option<Box<dyn Fn() + Send + Sync>>) {
        self.before_session_invalidate = callback;
    }

    // ponytail: session_before_switch/session_shutdown extension events are
    // not dispatched (the AgentSession has no ExtensionRunner slot yet);
    // cancellation hooks are inert. Add when extension runner wiring lands.
    fn emit_before_switch(
        &self,
        _reason: &str,
        _target_session_file: Option<&str>,
    ) -> Result<bool, String> {
        Ok(false)
    }

    fn teardown_current(&self, reason: &str, target_session_file: Option<&str>) -> Result<(), String> {
        self.session.abort();
        if let Some(callback) = &self.before_session_invalidate {
            callback();
        }
        self.session.dispose();
        Ok(())
    }

    fn apply(&mut self, result: CreateAgentSessionRuntimeResult) {
        self.session = result.session;
        self.services = result.services;
        self.diagnostics = result.diagnostics;
        self.model_fallback_message = result.model_fallback_message;
    }

    fn finish_session_replacement(
        &self,
        with_session: Option<&dyn Fn(&AgentSession) -> Result<(), String>>,
    ) -> Result<(), String> {
        if let Some(rebind) = &self.rebind_session {
            rebind(&self.session)?;
        }
        if let Some(with_session) = with_session {
            with_session(&self.session)?;
        }
        Ok(())
    }

    fn apply_runtime(
        &mut self,
        cwd: &str,
        agent_dir: &str,
        session_manager: SessionManager,
        reason: &str,
        previous_session_file: Option<String>,
    ) -> Result<(), String> {
        let result = (self.create_runtime)(RuntimeCreateOptions {
            cwd: cwd.to_string(),
            agent_dir: agent_dir.to_string(),
            session_manager,
            session_start_reason: Some(reason.to_string()),
            previous_session_file,
        })?;
        self.apply(result);
        Ok(())
    }

    /// Switch to an existing session file. Mirrors switchSession.
    pub fn switch_session(
        &mut self,
        session_path: &str,
        cwd_override: Option<&str>,
        with_session: Option<&dyn Fn(&AgentSession) -> Result<(), String>>,
    ) -> Result<bool, String> {
        let cancelled = self.emit_before_switch("resume", Some(session_path))?;
        if cancelled {
            return Ok(true);
        }
        let previous_session_file = self.session.get_session_file();
        let session_manager = SessionManager::open(session_path, None, cwd_override);
        assert_session_cwd_exists(&session_manager, self.cwd())?;
        let target = session_manager.get_session_file().map(|s| s.to_string());
        self.teardown_current("resume", target.as_deref())?;
        let new_cwd = session_manager.get_cwd().to_string();
        self.apply_runtime(
            &new_cwd,
            &self.services.agent_dir,
            session_manager,
            "resume",
            previous_session_file,
        )?;
        self.finish_session_replacement(with_session)?;
        Ok(false)
    }

    /// Start a new session. Mirrors newSession.
    pub fn new_session(
        &mut self,
        parent_session: Option<&str>,
        setup: Option<&dyn Fn(&mut SessionManager) -> Result<(), String>>,
        with_session: Option<&dyn Fn(&AgentSession) -> Result<(), String>>,
    ) -> Result<bool, String> {
        let cancelled = self.emit_before_switch("new", None)?;
        if cancelled {
            return Ok(true);
        }
        let previous_session_file = self.session.get_session_file();
        let session_dir = self.session.session_manager().get_session_dir().to_string();
        let mut session_manager = if self.session.session_manager().is_persisted() {
            SessionManager::create(self.cwd(), Some(&session_dir), None)
        } else {
            SessionManager::in_memory(Some(self.cwd().to_string()), None)
        };
        if let Some(parent) = parent_session {
            session_manager.new_session(Some(super::session_manager::NewSessionOptions {
                parent_session: Some(parent.to_string()),
                name: None,
            }));
        }
        let target = session_manager.get_session_file().map(|s| s.to_string());
        self.teardown_current("new", target.as_deref())?;
        self.apply_runtime(
            self.cwd(),
            &self.services.agent_dir,
            session_manager,
            "new",
            previous_session_file,
        )?;
        if let Some(setup) = setup {
            let mut manager = self.session.session_manager();
            setup(&mut manager)?;
            let context = manager.build_session_context();
            let agent = self.session.agent();
            let mut state = agent.state_mut();
            state.messages = context.messages;
        }
        self.finish_session_replacement(with_session)?;
        Ok(false)
    }

    /// Fork at a session entry. Mirrors fork.
    pub fn fork(
        &mut self,
        entry_id: &str,
        position: &str,
        with_session: Option<&dyn Fn(&AgentSession) -> Result<(), String>>,
    ) -> Result<(bool, Option<String>), String> {
        let position_at = position == "at";
        let mut target_leaf_id: Option<String>;
        let selected_text: Option<String>;

        let selected_entry = self.session.session_manager().get_entry(entry_id);
        let Some(selected_entry) = selected_entry else {
            return Err("Invalid entry ID for forking".into());
        };

        if position_at {
            target_leaf_id = Some(selected_entry.id().to_string());
            selected_text = None;
        } else {
            if selected_entry.type_name() != "message"
                || !matches!(
                    selected_entry,
                    super::session_types::SessionEntry::Message { message, .. }
                        if matches!(message, super::session_messages::SessionMessage::Llm(pi_ai::types::Message::User(_)))
                )
            {
                return Err("Invalid entry ID for forking".into());
            }
            if let super::session_types::SessionEntry::Message { message, .. } = &selected_entry {
                if let super::session_messages::SessionMessage::Llm(pi_ai::types::Message::User(user)) = message {
                    selected_text = Some(extract_user_message_text(&user.content));
                } else {
                    selected_text = None;
                }
            } else {
                selected_text = None;
            }
            target_leaf_id = selected_entry.parent_id().map(|value| value.to_string());
        }

        let previous_session_file = self.session.get_session_file();
        let persisted = self.session.session_manager().is_persisted();
        let mut session_manager;
        if persisted {
            let current_session_file = self
                .session
                .get_session_file()
                .ok_or_else(|| "Persisted session is missing a session file".to_string())?;
            let session_dir = self.session.session_manager().get_session_dir().to_string();
            let created = match &target_leaf_id {
                None => {
                    let mut manager = SessionManager::create(self.cwd(), Some(&session_dir), None);
                    manager.new_session(Some(super::session_manager::NewSessionOptions {
                        parent_session: Some(current_session_file.clone()),
                        name: None,
                    }));
                    manager
                }
                Some(leaf_id) => {
                    if !std::path::Path::new(&current_session_file).exists() {
                        return Err(
                            "This session has not been saved yet. Wait for the first assistant response before cloning or forking it."
                                .to_string(),
                        );
                    }
                    let mut manager = SessionManager::open(&current_session_file, Some(&session_dir), None);
                    if manager.create_branched_session(leaf_id).is_none() {
                        return Err("Failed to create forked session".to_string());
                    }
                    manager
                }
            };
            session_manager = created;
        } else {
            let mut manager = self.session.session_manager();
            match &target_leaf_id {
                None => {
                    let parent = self.session.get_session_file();
                    manager.new_session(Some(super::session_manager::NewSessionOptions {
                        parent_session: parent,
                        name: None,
                    }));
                }
                Some(leaf_id) => {
                    if manager.create_branched_session(leaf_id).is_none() {
                        return Err("Failed to create forked session".to_string());
                    }
                }
            }
            session_manager = manager;
        }
        let target = session_manager.get_session_file().map(|s| s.to_string());
        self.teardown_current("fork", target.as_deref())?;
        let new_cwd = session_manager.get_cwd().to_string();
        self.apply_runtime(
            &new_cwd,
            &self.services.agent_dir,
            session_manager,
            "fork",
            previous_session_file,
        )?;
        self.finish_session_replacement(with_session)?;
        Ok((false, selected_text))
    }

    /// Import a session JSONL file and switch to it. Mirrors importFromJsonl.
    pub fn import_from_jsonl(
        &mut self,
        input_path: &str,
        cwd_override: Option<&str>,
    ) -> Result<bool, String> {
        let resolved_path = super::session_paths::resolve_path(input_path, None);
        if !std::path::Path::new(&resolved_path).exists() {
            return Err(format!(
                "{}",
                SessionImportFileNotFoundError {
                    file_path: resolved_path.clone()
                }
            ));
        }

        let session_dir = self.session.session_manager().get_session_dir().to_string();
        std::fs::create_dir_all(&session_dir).map_err(|error| error.to_string())?;

        let basename = std::path::Path::new(&resolved_path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "session.jsonl".into());
        let destination_path = PathBuf::from(&session_dir).join(&basename);
        let destination_str = destination_path.to_string_lossy().to_string();

        let cancelled = self.emit_before_switch("resume", Some(&destination_str))?;
        if cancelled {
            return Ok(true);
        }
        let previous_session_file = self.session.get_session_file();
        // Copy unless the resolved path equals the destination.
        let destination_abs = crate::core::session_paths::resolve_path(&destination_str, None);
        let resolved_abs = crate::core::session_paths::resolve_path(&resolved_path, None);
        if destination_abs != resolved_abs {
            std::fs::copy(&resolved_path, &destination_str).map_err(|error| error.to_string())?;
        }

        let session_manager = SessionManager::open(&destination_str, Some(&session_dir), cwd_override);
        assert_session_cwd_exists(&session_manager, self.cwd())?;
        let target = session_manager.get_session_file().map(|s| s.to_string());
        self.teardown_current("resume", target.as_deref())?;
        let new_cwd = session_manager.get_cwd().to_string();
        self.apply_runtime(
            &new_cwd,
            &self.services.agent_dir,
            session_manager,
            "resume",
            previous_session_file,
        )?;
        self.finish_session_replacement(None)?;
        Ok(false)
    }

    pub fn dispose(&mut self) {
        if let Some(callback) = &self.before_session_invalidate {
            callback();
        }
        self.session.dispose();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-rt-{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().to_string()
    }

    fn make_factory() -> Box<dyn Fn(RuntimeCreateOptions) -> Result<CreateAgentSessionRuntimeResult, String> + Send + Sync> {
        Box::new(|options: RuntimeCreateOptions| {
            let sm = options.session_manager;
            let cwd = sm.get_cwd().to_string();
            let agent_dir = options.agent_dir;
            let result = super::super::sdk::create_agent_session(super::super::sdk::CreateAgentSessionOptions {
                cwd: Some(cwd.clone()),
                agent_dir: Some(agent_dir.clone()),
                session_manager: Some(sm),
                ..Default::default()
            })?;
            let services = AgentSessionServices {
                cwd,
                agent_dir,
                model_runtime: super::super::model_runtime::ModelRuntime::create(
                    super::super::model_runtime::CreateModelRuntimeOptions::default(),
                ),
                settings_manager: super::super::settings_manager::SettingsManager::create("/tmp", &agent_dir, true),
                resource_loader: super::super::resource_loader::DefaultResourceLoader::new(
                    super::super::resource_loader::DefaultResourceLoaderOptions::default(),
                ),
                diagnostics: Vec::new(),
            };
            Ok(CreateAgentSessionRuntimeResult {
                session: result.session,
                services,
                diagnostics: Vec::new(),
                model_fallback_message: result.model_fallback_message,
            })
        })
    }

    #[test]
    fn new_session_switches_sessions() {
        let agent_dir = temp_dir("agent");
        let session_dir = temp_dir("sessions");
        let sm = SessionManager::create("/tmp", Some(&session_dir), None);
        let runtime = AgentSessionRuntime::new(
            {
                let result = super::super::sdk::create_agent_session(super::super::sdk::CreateAgentSessionOptions {
                    cwd: Some("/tmp".into()),
                    agent_dir: Some(agent_dir.clone()),
                    session_manager: Some(sm),
                    ..Default::default()
                })
                .unwrap();
                result.session
            },
            AgentSessionServices {
                cwd: "/tmp".into(),
                agent_dir: agent_dir.clone(),
                model_runtime: ModelRuntime::create(super::super::model_runtime::CreateModelRuntimeOptions::default()),
                settings_manager: SettingsManager::create("/tmp", &agent_dir, true),
                resource_loader: DefaultResourceLoader::new(
                    super::super::resource_loader::DefaultResourceLoaderOptions::default(),
                ),
                diagnostics: Vec::new(),
            },
            make_factory(),
            Vec::new(),
            None,
        );
        let mut runtime = runtime;
        let session_id_before = runtime.session().get_session_id();
        let cancelled = runtime.new_session(None, None, None).unwrap();
        assert!(!cancelled);
        assert_ne!(runtime.session().get_session_id(), session_id_before);
    }

    #[test]
    fn import_missing_file_errors() {
        let agent_dir = temp_dir("agent2");
        let session_dir = temp_dir("sessions2");
        let sm = SessionManager::create("/tmp", Some(&session_dir), None);
        let mut runtime = AgentSessionRuntime::new(
            {
                let result = super::super::sdk::create_agent_session(super::super::sdk::CreateAgentSessionOptions {
                    cwd: Some("/tmp".into()),
                    agent_dir: Some(agent_dir.clone()),
                    session_manager: Some(sm),
                    ..Default::default()
                })
                .unwrap();
                result.session
            },
            AgentSessionServices {
                cwd: "/tmp".into(),
                agent_dir,
                model_runtime: ModelRuntime::create(super::super::model_runtime::CreateModelRuntimeOptions::default()),
                settings_manager: SettingsManager::create("/tmp", &agent_dir, true),
                resource_loader: DefaultResourceLoader::new(
                    super::super::resource_loader::DefaultResourceLoaderOptions::default(),
                ),
                diagnostics: Vec::new(),
            },
            make_factory(),
            Vec::new(),
            None,
        );
        let error = runtime.import_from_jsonl("/nonexistent/never-existed.jsonl", None).unwrap_err();
        assert!(error.contains("File not found"));
    }
}
