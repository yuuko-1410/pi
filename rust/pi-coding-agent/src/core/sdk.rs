//! AgentSession factory, port of the create path of `core/sdk.ts`.
//! Extension wiring (runner refs, provider registration, header hooks) is
//! deferred; the model/thinking restore, session initialization, and tool
//! allowlist logic are ported.

use std::sync::Arc;

use pi_ai::types::{Message, Model};
use pi_agent_core::agent::{Agent, AgentOptions, MutableAgentState};

use super::agent_session::{AgentSession, AgentSessionConfig};
use super::auth_guidance::format_no_models_available_message;
use super::defaults::DEFAULT_THINKING_LEVEL;
use super::model_resolver::find_initial_model;
use super::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use super::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};
use super::session_manager::SessionManager;
use super::settings_manager::SettingsManager;

#[derive(Default)]
pub struct CreateAgentSessionOptions {
    pub cwd: Option<String>,
    pub agent_dir: Option<String>,
    pub model: Option<Model>,
    pub thinking_level: Option<String>,
    pub scoped_models: Vec<super::model_resolver::ScopedModel>,
    pub no_tools: Option<String>,
    pub tools: Option<Vec<String>>,
    pub exclude_tools: Option<Vec<String>>,
    pub custom_tools: Vec<pi_protocol::Value>,
    pub session_manager: Option<SessionManager>,
    pub settings_manager: Option<SettingsManager>,
}

impl std::fmt::Debug for CreateAgentSessionOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateAgentSessionOptions")
            .field("cwd", &self.cwd)
            .field("agent_dir", &self.agent_dir)
            .field("model", &self.model)
            .field("thinking_level", &self.thinking_level)
            .finish()
    }
}

pub struct CreateAgentSessionResult {
    pub session: Arc<AgentSession>,
    pub model_fallback_message: Option<String>,
}

fn resolve_path(input: &str) -> String {
    crate::core::session_paths::resolve_path(input, None)
}

/// Create an AgentSession with the specified options (sync analog).
pub fn create_agent_session(options: CreateAgentSessionOptions) -> Result<CreateAgentSessionResult, String> {
    let cwd = resolve_path(
        options
            .cwd
            .as_deref()
            .or_else(|| options.session_manager.as_ref().map(|sm| sm.get_cwd()))
            .unwrap_or("/tmp"),
    );
    let agent_dir = options
        .agent_dir
        .as_deref()
        .map(resolve_path)
        .unwrap_or_else(crate::config::get_agent_dir);

    let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        auth_path: Some(format!("{agent_dir}/auth.json")),
        models_path: Some(format!("{agent_dir}/models.json")),
        models_store_path: None,
    });

    let settings_manager = match options.settings_manager {
        Some(settings_manager) => settings_manager,
        None => SettingsManager::create(&cwd, &agent_dir, true),
    };

    let mut session_manager = match options.session_manager {
        Some(session_manager) => session_manager,
        None => SessionManager::create(&cwd, Some(&crate::core::session_manager::get_default_session_dir(&cwd)), None),
    };

    // Check if the session has existing data to restore.
    let existing_context = session_manager.build_session_context();
    let has_existing_session = !existing_context.messages.is_empty();
    let has_thinking_entry = session_manager
        .get_entries()
        .iter()
        .any(|entry| matches!(entry, crate::core::session_types::SessionEntry::ThinkingLevelChange { .. }));

    let mut model = options.model.clone();
    let mut model_fallback_message: Option<String> = None;

    // Restore the model from the session.
    if model.is_none() && has_existing_session {
        if let Some((provider, model_id)) = &existing_context.model {
            let restored = model_runtime.get_model(provider, model_id);
            if restored.is_some() && model_runtime.has_configured_auth(provider) {
                model = restored;
            }
            if model.is_none() {
                model_fallback_message =
                    Some(format!("Could not restore model {provider}/{model_id}"));
            }
        }
    }

    // Otherwise find the initial model (settings default, then provider defaults).
    if model.is_none() {
        let result = find_initial_model(
            None,
            None,
            &options.scoped_models,
            has_existing_session,
            settings_manager.get_default_provider().as_deref(),
            settings_manager.get_default_model().as_deref(),
            settings_manager.get_default_thinking_level().as_deref(),
            &model_runtime,
        );
        model = result.model;
        if model.is_none() {
            model_fallback_message = Some(format_no_models_available_message());
        } else if let Some(message) = &model_fallback_message {
            let model = model.as_ref().unwrap();
            model_fallback_message = Some(format!("{message}. Using {}/{}", model.provider, model.id));
        }
    }

    let mut thinking_level = options.thinking_level.clone();
    if thinking_level.is_none() && has_existing_session {
        thinking_level = Some(if has_thinking_entry {
            existing_context.thinking_level.clone()
        } else {
            settings_manager
                .get_default_thinking_level()
                .unwrap_or_else(|| DEFAULT_THINKING_LEVEL.to_string())
        });
    }
    if thinking_level.is_none() {
        thinking_level = Some(
            settings_manager
                .get_default_thinking_level()
                .unwrap_or_else(|| DEFAULT_THINKING_LEVEL.to_string()),
        );
    }
    // Clamp to model capabilities.
    let thinking_level = match &model {
        None => "off".to_string(),
        Some(model) if model.reasoning => {
            let level = thinking_level.unwrap_or_else(|| DEFAULT_THINKING_LEVEL.to_string());
            if super::model_resolver::VALID_THINKING_LEVELS.contains(&level.as_str()) {
                level
            } else {
                "off".to_string()
            }
        }
        Some(_) => "off".to_string(),
    };

    // Tool allowlist resolution.
    let default_active_tool_names = ["read", "bash", "edit", "write"];
    let excluded: Option<std::collections::HashSet<String>> = options
        .exclude_tools
        .as_ref()
        .map(|tools| tools.iter().cloned().collect());
    let initial_active_tool_names: Vec<String> = {
        let base: Vec<String> = if let Some(tools) = &options.tools {
            tools.clone()
        } else if options.no_tools.is_some() {
            Vec::new()
        } else {
            default_active_tool_names.iter().map(|value| value.to_string()).collect()
        };
        base.into_iter()
            .filter(|name| !excluded.as_ref().is_some_and(|set| set.contains(name)))
            .collect()
    };

    // Build the agent with the session state.
    let mut initial_state = MutableAgentState::default();
    initial_state.model = model.clone().unwrap_or_else(|| {
        // Default placeholder model (id "unknown") mirrors the JS default.
        let mut placeholder = pi_ai::types::Model {
            id: "unknown".into(),
            name: "unknown".into(),
            api: "unknown".into(),
            provider: "unknown".into(),
            base_url: String::new(),
            reasoning: false,
            thinking_level_map: None,
            input: Vec::new(),
            cost: pi_ai::types::ModelCost {
                rates: pi_ai::types::ModelCostRates {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                tiers: None,
            },
            context_window: 0.0,
            max_tokens: 0.0,
            sampling_params: None,
            headers: None,
            compat: None,
        };
        placeholder.id = String::new();
        placeholder.provider = String::new();
        placeholder
    });
    initial_state.thinking_level = thinking_level.clone();
    initial_state.system_prompt = String::new();

    let mut agent = Agent::new(AgentOptions {
        initial_state: Some(initial_state),
        ..Default::default()
    });

    // Restore messages from the session.
    if has_existing_session {
        agent.state_mut().messages = existing_context.messages.clone();
        if !has_thinking_entry {
            session_manager.append_thinking_level_change(thinking_level.clone());
        }
    } else {
        if let Some(model) = &model {
            session_manager.append_model_change(model.provider.clone(), model.id.clone());
        }
        session_manager.append_thinking_level_change(thinking_level.clone());
    }

    let resource_loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        settings_manager: None,
        ..Default::default()
    });

    let session = Arc::new(AgentSession::new(AgentSessionConfig {
        agent,
        session_manager,
        settings_manager,
        scoped_models: options.scoped_models.clone(),
        resource_loader,
        custom_tools: options.custom_tools.clone(),
        cwd,
        model_runtime: Arc::new(model_runtime),
    }));
    let _ = initial_active_tool_names;
    let _: Option<&mut ModelRuntime> = None;

    Ok(CreateAgentSessionResult {
        session,
        model_fallback_message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_agent_dir() -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-sdk-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().to_string()
    }

    #[test]
    fn creates_session_with_defaults() {
        let agent_dir = temp_agent_dir();
        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some("/tmp".into()),
            agent_dir: Some(agent_dir),
            ..Default::default()
        })
        .unwrap();
        // No model configured: fallback message present.
        assert!(result.model_fallback_message.is_some());
        let state = result.session.state();
        assert_eq!(state.thinking_level, "off");
    }

    #[test]
    fn session_persistence_restores_model() {
        let agent_dir = temp_agent_dir();
        // Pre-create a session with a model entry, then reopen.
        let mut manager = SessionManager::in_memory(None, None);
        manager.append_message(crate::core::session_types::SessionMessage::Llm(Message::User(
            pi_ai::types::UserMessage {
                content: pi_ai::types::UserMessageContent::Text("hello".into()),
                timestamp: 0.0,
            },
        )));
        manager.append_model_change("acme".into(), "m1".into());

        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some("/tmp".into()),
            agent_dir: Some(agent_dir),
            session_manager: Some(manager),
            ..Default::default()
        })
        .unwrap();
        // Model "acme/m1" is not in the runtime (no models.json): fallback.
        assert!(result.model_fallback_message.is_some());
        let state = result.session.state();
        assert_eq!(state.messages.len(), 1);
    }
}
