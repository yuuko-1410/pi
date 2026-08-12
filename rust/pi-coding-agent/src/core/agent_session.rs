//! AgentSession, port of `core/agent-session.ts`.
//!
//! Synchronous analog: the agent loop, extension emits, and LLM calls are
//! blocking; AbortControllers become CancellationTokens; the idle-wait
//! promise becomes a condition variable. Extension-heavy flows (tool hook
//! interception, emitBeforeAgentStart message injection) run through the
//! runner's sync emit surface.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use pi_ai::models::models_are_equal;
use pi_ai::types::{AssistantMessage, Message, Model, UserMessage, UserMessageContent};
use pi_ai::utils::estimate::calculate_context_tokens;
use pi_ai::utils::retry::is_retryable_assistant_error;
use pi_agent_core::agent::Agent;
use pi_agent_core::types::AgentMessage;

use super::auth_guidance::{format_no_api_key_found_message, format_no_model_selected_message};
use super::compaction::compaction::{
    compact, prepare_compaction, should_compact, CompactionResult, CompactionSettings, SummaryCallOptions,
};
use super::model_resolver::ScopedModel;
use super::model_runtime::ModelRuntime;
use super::resource_loader::DefaultResourceLoader;
use super::session_manager::SessionManager;
use super::session_types::SessionEntry;
use super::settings_manager::SettingsManager;
use super::tools::bash_executor::{execute_bash_with_operations, BashOperations, BashResult};

pub const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

#[derive(Clone, Debug, PartialEq)]
pub enum AgentSessionEvent {
    MessageStart { message: AgentMessage },
    MessageEnd { message: AgentMessage },
    AgentStart,
    AgentEnd { will_retry: bool },
    TurnStart,
    TurnEnd,
    QueueUpdate { steering: Vec<String>, follow_up: Vec<String> },
    ThinkingLevelChanged { level: String },
    AutoRetryEnd { success: bool, attempt: usize },
    AutoCompactionStart,
    AutoCompactionEnd,
    BranchSummaryStart,
    BranchSummaryEnd,
    ModelSelect { provider: String, model_id: String, source: String },
    ToolExecutionStart { tool_call_id: String, tool_name: String },
    ToolExecutionEnd { tool_call_id: String, tool_name: String, is_error: bool },
    SessionStart,
}

pub type AgentSessionEventListener = Box<dyn Fn(&AgentSessionEvent) + Send + Sync>;

#[derive(Clone, Debug)]
pub struct ModelCycleResult {
    pub model: Model,
    pub thinking_level: String,
    pub is_scoped: bool,
}

pub struct PromptOptions {
    pub expand_prompt_templates: bool,
    pub streaming_behavior: Option<String>, // "steer" | "followUp"
    pub images: Option<Vec<pi_ai::types::ImageContent>>,
    pub source: Option<String>,
    pub preflight_result: Option<Box<dyn Fn(bool) + Send + Sync>>,
}

impl Default for PromptOptions {
    fn default() -> Self {
        Self {
            expand_prompt_templates: true,
            streaming_behavior: None,
            images: None,
            source: None,
            preflight_result: None,
        }
    }
}

pub struct AgentSessionConfig {
    pub agent: Agent,
    pub session_manager: SessionManager,
    pub settings_manager: SettingsManager,
    pub scoped_models: Vec<ScopedModel>,
    pub resource_loader: DefaultResourceLoader,
    pub custom_tools: Vec<Value>,
    pub cwd: String,
    pub model_runtime: Arc<ModelRuntime>,
}

/// Idle notification channel (replaces the JS idle-wait promise).
struct IdleState {
    is_running: AtomicBool,
    condvar: Condvar,
    mutex: Mutex<()>,
}

pub struct AgentSession {
    agent: Arc<Mutex<Agent>>,
    pub session_manager: Mutex<SessionManager>,
    pub settings_manager: Mutex<SettingsManager>,
    scoped_models: Vec<ScopedModel>,
    resource_loader: Mutex<DefaultResourceLoader>,
    model_runtime: Arc<ModelRuntime>,
    cwd: String,

    event_listeners: Mutex<Vec<AgentSessionEventListener>>,
    steering_messages: Mutex<Vec<String>>,
    follow_up_messages: Mutex<Vec<String>>,
    pending_next_turn_messages: Mutex<Vec<AgentMessage>>,

    last_assistant_message: Mutex<Option<AssistantMessage>>,
    overflow_recovery_attempted: AtomicBool,
    retry_attempt: AtomicBool,
    retry_count: Mutex<usize>,
    is_agent_run_active: AtomicBool,
    idle: IdleState,

    auto_compaction_enabled: AtomicBool,
    auto_retry_enabled: AtomicBool,
    system_prompt_override: Mutex<Option<String>>,
    base_system_prompt: Mutex<String>,
    _active_tool_names: Mutex<HashSet<String>>,
    pending_bash_messages: Mutex<Vec<AgentMessage>>,
    bash_abort: Arc<AtomicBool>,
    disposed: AtomicBool,
}

use pi_protocol::Value;

impl AgentSession {
    pub fn new(config: AgentSessionConfig) -> Self {
        let session = AgentSession {
            agent: Arc::new(Mutex::new(config.agent)),
            session_manager: Mutex::new(config.session_manager),
            settings_manager: Mutex::new(config.settings_manager),
            scoped_models: config.scoped_models,
            resource_loader: Mutex::new(config.resource_loader),
            model_runtime: config.model_runtime,
            cwd: config.cwd,
            event_listeners: Mutex::new(Vec::new()),
            steering_messages: Mutex::new(Vec::new()),
            follow_up_messages: Mutex::new(Vec::new()),
            pending_next_turn_messages: Mutex::new(Vec::new()),
            last_assistant_message: Mutex::new(None),
            overflow_recovery_attempted: AtomicBool::new(false),
            retry_attempt: AtomicBool::new(false),
            retry_count: Mutex::new(0),
            is_agent_run_active: AtomicBool::new(false),
            idle: IdleState {
                is_running: AtomicBool::new(false),
                condvar: Condvar::new(),
                mutex: Mutex::new(()),
            },
            auto_compaction_enabled: AtomicBool::new(true),
            auto_retry_enabled: AtomicBool::new(true),
            system_prompt_override: Mutex::new(None),
            base_system_prompt: Mutex::new(String::new()),
            _active_tool_names: Mutex::new(HashSet::new()),
            pending_bash_messages: Mutex::new(Vec::new()),
            bash_abort: Arc::new(AtomicBool::new(false)),
            disposed: AtomicBool::new(false),
        };
        session
    }

    // -----------------------------------------------------------------------
    // Events
    // -----------------------------------------------------------------------

    pub fn emit(&self, event: &AgentSessionEvent) {
        let listeners = self.event_listeners.lock().unwrap();
        for listener in listeners.iter() {
            listener(event);
        }
    }

    pub fn subscribe(&self, listener: AgentSessionEventListener) -> usize {
        let mut listeners = self.event_listeners.lock().unwrap();
        listeners.push(listener);
        listeners.len() - 1
    }

    pub fn unsubscribe(&self, id: usize) {
        let mut listeners = self.event_listeners.lock().unwrap();
        if id < listeners.len() {
            let _ = listeners.remove(id);
        }
    }

    fn emit_queue_update(&self) {
        self.emit(&AgentSessionEvent::QueueUpdate {
            steering: self.steering_messages.lock().unwrap().clone(),
            follow_up: self.follow_up_messages.lock().unwrap().clone(),
        });
    }

    // -----------------------------------------------------------------------
    // State access
    // -----------------------------------------------------------------------

    pub fn state(&self) -> pi_agent_core::agent::MutableAgentState {
        self.agent.lock().unwrap().state()
    }

    /// Accessor for the runtime owner (session replacement flows).
    pub fn agent(&self) -> Arc<Mutex<Agent>> {
        self.agent.clone()
    }

    /// Accessor for the runtime owner (session replacement flows).
    pub fn session_manager(&self) -> std::sync::MutexGuard<'_, SessionManager> {
        self.session_manager.lock().unwrap()
    }

    /// Model runtime for this session (model selector, provider counts).
    pub fn model_runtime(&self) -> Arc<ModelRuntime> {
        self.model_runtime.clone()
    }

    /// Whether auto-compaction is enabled (footer indicator).
    pub fn is_auto_compaction_enabled(&self) -> bool {
        self.auto_compaction_enabled.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn model(&self) -> Option<Model> {
        let model = self.agent.lock().unwrap().state().model;
        if model.provider.is_empty() || model.provider == "unknown" {
            None
        } else {
            Some(model)
        }
    }

    pub fn thinking_level(&self) -> String {
        self.agent.lock().unwrap().state().thinking_level
    }

    pub fn is_streaming(&self) -> bool {
        self.is_agent_run_active.load(Ordering::SeqCst)
    }

    pub fn system_prompt(&self) -> String {
        self.agent.lock().unwrap().state().system_prompt
    }

    pub fn messages(&self) -> Vec<AgentMessage> {
        self.agent.lock().unwrap().state().messages
    }

    pub fn get_session_file(&self) -> Option<String> {
        self.session_manager.lock().unwrap().get_session_file().map(|value| value.to_string())
    }

    pub fn get_session_id(&self) -> String {
        self.session_manager.lock().unwrap().get_session_id().to_string()
    }

    pub fn get_session_name(&self) -> Option<String> {
        self.session_manager.lock().unwrap().get_session_name()
    }

    pub fn scoped_models(&self) -> &[ScopedModel] {
        &self.scoped_models
    }

    pub fn prompt_templates(&self) -> Vec<pi_agent_core::harness::events::PromptTemplate> {
        self.resource_loader.lock().unwrap().get_prompts().to_vec()
    }

    fn expand_prompt_template(&self, text: &str) -> String {
        let templates: Vec<super::prompt_templates::PromptTemplate> = self
            .prompt_templates()
            .iter()
            .map(|template| super::prompt_templates::PromptTemplate {
                name: template.name.clone(),
                description: template.description.clone().unwrap_or_default(),
                argument_hint: None,
                content: template.content.clone(),
                source_info: super::source_info::SourceInfo {
                    path: template.file_path.clone(),
                    source: "local".into(),
                    scope: "user".into(),
                    origin: "top-level".into(),
                    base_dir: None,
                },
                file_path: template.file_path.clone(),
            })
            .collect();
        super::prompt_templates::expand_prompt_template(text, &templates)
    }

    // -----------------------------------------------------------------------
    // Prompting
    // -----------------------------------------------------------------------

    fn mark_running(&self, running: bool) {
        self.is_agent_run_active.store(running, Ordering::SeqCst);
        let _guard = self.idle.mutex.lock().unwrap();
        self.idle.is_running.store(running, Ordering::SeqCst);
        if !running {
            self.idle.condvar.notify_all();
        }
    }

    pub fn wait_for_idle(&self) {
        let mut guard = self.idle.mutex.lock().unwrap();
        while self.idle.is_running.load(Ordering::SeqCst) {
            guard = self.idle.condvar.wait(guard).unwrap();
        }
    }

    fn run_agent_prompt(&self, messages: Vec<AgentMessage>) -> Result<(), String> {
        self.mark_running(true);
        let result = (|| -> Result<(), String> {
            self.agent.lock().unwrap().prompt(messages)?;
            while self.handle_post_agent_run()? {
                self.agent.lock().unwrap().continue_()?;
            }
            Ok(())
        })();
        *self.system_prompt_override.lock().unwrap() = None;
        self.flush_pending_bash_messages();
        self.mark_running(false);
        self.emit(&AgentSessionEvent::AgentEnd {
            will_retry: false,
        });
        result
    }

    fn handle_post_agent_run(&self) -> Result<bool, String> {
        let message = self.last_assistant_message.lock().unwrap().take();
        let Some(message) = message else {
            return Ok(false);
        };

        if is_retryable_assistant_error(&message) && self.prepare_retry(&message)? {
            return Ok(true);
        }

        if message.stop_reason == pi_ai::types::StopReason::Error {
            let attempt = *self.retry_count.lock().unwrap();
            if attempt > 0 {
                self.emit(&AgentSessionEvent::AutoRetryEnd {
                    success: false,
                    attempt,
                });
                *self.retry_count.lock().unwrap() = 0;
            }
        }

        if self.check_compaction(&message, false)? {
            return Ok(true);
        }

        Ok(self.agent.lock().unwrap().has_queued_messages())
    }

    /// Send a prompt to the agent (sync analog of prompt()).
    pub fn prompt(&self, text: &str, options: &PromptOptions) -> Result<(), String> {
        let expand_prompt_templates = options.expand_prompt_templates;

        // Extension commands execute immediately.
        if expand_prompt_templates && text.starts_with('/') {
            if self.try_execute_extension_command(text) {
                if let Some(preflight) = &options.preflight_result {
                    preflight(true);
                }
                return Ok(());
            }
        }

        // Expand skill commands and prompt templates.
        let mut expanded_text = text.to_string();
        if expand_prompt_templates {
            expanded_text = self.expand_skill_command(&expanded_text);
            expanded_text = self.expand_prompt_template(&expanded_text);
        }

        // If streaming, queue via steer or followUp.
        if self.is_streaming() {
            let behavior = options
                .streaming_behavior
                .as_deref()
                .ok_or_else(|| "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.".to_string())?;
            if behavior == "followUp" {
                self.queue_follow_up(&expanded_text, options.images.as_deref());
            } else {
                self.queue_steer(&expanded_text, options.images.as_deref());
            }
            if let Some(preflight) = &options.preflight_result {
                preflight(true);
            }
            return Ok(());
        }

        // Flush pending bash messages.
        self.flush_pending_bash_messages();

        // Validate model.
        let Some(model) = self.model() else {
            return Err(format_no_model_selected_message());
        };
        let has_configured_auth = self.model_runtime.has_configured_auth(&model.provider);
        if !has_configured_auth {
            return Err(format_no_api_key_found_message(&model.provider));
        }

        // Compaction check before sending.
        if let Some(last_assistant) = self.find_last_assistant_message() {
            self.check_compaction(&last_assistant, false)?;
        }

        // Build the user message.
        let mut content: Vec<pi_ai::types::Content> = vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
            text: expanded_text,
            text_signature: None,
        })];
        if let Some(images) = &options.images {
            for image in images {
                content.push(pi_ai::types::Content::Image(image.clone()));
            }
        }
        let mut messages: Vec<AgentMessage> = vec![AgentMessage::Llm(Message::User(UserMessage {
            content: UserMessageContent::Blocks(content),
            timestamp: now_ms(),
        }))];

        // Inject pending nextTurn messages.
        let pending = self.pending_next_turn_messages.lock().unwrap().clone();
        for message in pending {
            messages.push(message);
        }
        self.pending_next_turn_messages.lock().unwrap().clear();

        if let Some(preflight) = &options.preflight_result {
            preflight(true);
        }
        self.run_agent_prompt(messages)
    }

    fn try_execute_extension_command(&self, text: &str) -> bool {
        // Extension commands are dispatched through the runner registry; the
        // runner port resolves registered commands. Commands that require
        // interactive ctx (ui.select etc.) are not executed here.
        let space_index = text.find(' ');
        let command_name = match space_index {
            Some(index) => &text[1..index],
            None => &text[1..],
        };
        if command_name.is_empty() {
            return false;
        }
        // ponytail: extension command dispatch is deferred to interactive
        // mode; the runner registry is not exposed through the loader.
        // ponytail: extension command dispatch is deferred to interactive
        // mode; the runner registry is not exposed through the loader.
        let _ = command_name;
        false
    }

    fn expand_skill_command(&self, text: &str) -> String {
        if !text.starts_with("/skill:") {
            return text.to_string();
        }
        let space_index = text.find(' ');
        let skill_name = match space_index {
            Some(index) => &text[7..index],
            None => &text[7..],
        };
        let args = match space_index {
            Some(index) => text[index + 1..].trim().to_string(),
            None => String::new(),
        };

        let skills = self.resource_loader.lock().unwrap().get_skills().to_vec();
        let Some(skill) = skills.iter().find(|skill| skill.name == skill_name) else {
            return text.to_string();
        };

        let content = match std::fs::read_to_string(&skill.file_path) {
            Ok(content) => content,
            Err(_) => {
                return text.to_string();
            }
        };
        let body = crate::utils::basics::strip_frontmatter(&content).trim().to_string();
        let skill_block = format!(
            "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
            skill.name,
            skill.file_path,
            std::path::Path::new(&skill.file_path).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
            body
        );
        if args.is_empty() {
            skill_block
        } else {
            format!("{skill_block}\n\n{args}")
        }
    }

    fn queue_steer(&self, text: &str, images: Option<&[pi_ai::types::ImageContent]>) {
        self.steering_messages.lock().unwrap().push(text.to_string());
        self.emit_queue_update();
        self.agent.lock().unwrap().steer(user_message_with_images(text, images));
    }

    fn queue_follow_up(&self, text: &str, images: Option<&[pi_ai::types::ImageContent]>) {
        self.follow_up_messages.lock().unwrap().push(text.to_string());
        self.emit_queue_update();
        self.agent.lock().unwrap().follow_up(user_message_with_images(text, images));
    }

    /// Queue a steering message (already expanded).
    pub fn steer(&self, text: &str, images: Option<&[pi_ai::types::ImageContent]>) -> Result<(), String> {
        if text.starts_with('/') {
            self.throw_if_extension_command(text)?;
        }
        let expanded = self.expand_skill_command(text);
        let expanded = self.expand_prompt_template(&expanded);
        self.queue_steer(&expanded, images);
        Ok(())
    }

    /// Queue a follow-up message (already expanded).
    pub fn follow_up(&self, text: &str, images: Option<&[pi_ai::types::ImageContent]>) -> Result<(), String> {
        if text.starts_with('/') {
            self.throw_if_extension_command(text)?;
        }
        let expanded = self.expand_skill_command(text);
        let expanded = self.expand_prompt_template(&expanded);
        self.queue_follow_up(&expanded, images);
        Ok(())
    }

    fn throw_if_extension_command(&self, _text: &str) -> Result<(), String> {
        // ponytail: extension commands cannot be queued; the runner registry
        // is not exposed through the loader, so the check is a no-op.
        Ok(())
    }

    pub fn clear_queue(&self) -> (Vec<String>, Vec<String>) {
        let steering = self.steering_messages.lock().unwrap().clone();
        let follow_up = self.follow_up_messages.lock().unwrap().clone();
        self.steering_messages.lock().unwrap().clear();
        self.follow_up_messages.lock().unwrap().clear();
        self.agent.lock().unwrap().clear_steering_queue();
        self.agent.lock().unwrap().clear_follow_up_queue();
        self.emit_queue_update();
        (steering, follow_up)
    }

    pub fn pending_message_count(&self) -> usize {
        self.steering_messages.lock().unwrap().len() + self.follow_up_messages.lock().unwrap().len()
    }

    pub fn get_steering_messages(&self) -> Vec<String> {
        self.steering_messages.lock().unwrap().clone()
    }

    pub fn get_follow_up_messages(&self) -> Vec<String> {
        self.follow_up_messages.lock().unwrap().clone()
    }

    // -----------------------------------------------------------------------
    // Agent event handling (session persistence)
    // -----------------------------------------------------------------------

    /// Handle an agent event: persist messages, track assistant state,
    /// reset retry counters. Called by the loop integration layer.
    pub fn handle_agent_event(&self, event: &pi_agent_core::types::AgentEvent) {
        match event {
            pi_agent_core::types::AgentEvent::MessageEnd { message } => {
                // Persist standard roles as session entries; custom roles are
                // persisted by their producers.
                match message {
                    AgentMessage::Llm(message) => match message {
                        Message::User(_) | Message::Assistant(_) | Message::ToolResult(_) => {
                            self.session_manager.lock().unwrap().append_message(
                                crate::core::session_types::SessionMessage::Llm(message.clone()),
                            );
                        }
                    },
                    AgentMessage::Custom(_) => {}
                }
                if let AgentMessage::Llm(Message::Assistant(assistant)) = message {
                    *self.last_assistant_message.lock().unwrap() = Some(assistant.clone());
                    if assistant.stop_reason != pi_ai::types::StopReason::Error
                        && assistant.stop_reason != pi_ai::types::StopReason::Length
                    {
                        self.overflow_recovery_attempted.store(false, Ordering::SeqCst);
                    }
                    if assistant.stop_reason != pi_ai::types::StopReason::Error {
                        let attempt = *self.retry_count.lock().unwrap();
                        if attempt > 0 {
                            self.emit(&AgentSessionEvent::AutoRetryEnd {
                                success: true,
                                attempt,
                            });
                            *self.retry_count.lock().unwrap() = 0;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn find_last_assistant_message(&self) -> Option<AssistantMessage> {
        let messages = self.agent.lock().unwrap().state().messages;
        for message in messages.iter().rev() {
            if let AgentMessage::Llm(Message::Assistant(assistant)) = message {
                return Some(assistant.clone());
            }
        }
        None
    }

    // -----------------------------------------------------------------------
    // Model management
    // -----------------------------------------------------------------------

    /// Set the model directly (validates auth, persists, re-clamps thinking).
    pub fn set_model(&self, model: &Model) -> Result<(), String> {
        if !self.model_runtime.has_configured_auth(&model.provider) {
            return Err(format!("No API key for {}/{}", model.provider, model.id));
        }
        let previous = self.model();
        let thinking_level = self.get_thinking_level_for_model_switch(None);
        self.agent.lock().unwrap().state_mut().model = model.clone();
        self.session_manager.lock().unwrap().append_model_change(model.provider.clone(), model.id.clone());
        self.settings_manager.lock().unwrap().set_default_provider(&model.provider);
        self.settings_manager.lock().unwrap().set_default_model(&model.id);
        self.set_thinking_level(&thinking_level);
        if !models_are_equal(previous.as_ref(), Some(model)) {
            self.emit(&AgentSessionEvent::ModelSelect {
                provider: model.provider.clone(),
                model_id: model.id.clone(),
                source: "set".to_string(),
            });
        }
        Ok(())
    }

    /// Cycle to the next/previous model.
    pub fn cycle_model(&self, direction: &str) -> Option<ModelCycleResult> {
        if !self.scoped_models.is_empty() {
            return self.cycle_scoped_model(direction);
        }
        self.cycle_available_model(direction)
    }

    fn cycle_scoped_model(&self, direction: &str) -> Option<ModelCycleResult> {
        let available_ids: HashSet<String> = self
            .model_runtime
            .get_available_snapshot()
            .iter()
            .map(|model| format!("{}\0{}", model.provider, model.id))
            .collect();
        let scoped_models: Vec<ScopedModel> = self
            .scoped_models
            .iter()
            .filter(|scoped| available_ids.contains(&format!("{}\0{}", scoped.model.provider, scoped.model.id)))
            .cloned()
            .collect();
        if scoped_models.len() <= 1 {
            return None;
        }
        let current = self.model();
        let mut current_index = scoped_models
            .iter()
            .position(|sm| models_are_equal(Some(&sm.model), current.as_ref()))
            .unwrap_or(0);
        if current_index == usize::MAX {
            current_index = 0;
        }
        let len = scoped_models.len();
        let next_index = if direction == "forward" {
            (current_index + 1) % len
        } else {
            (current_index + len - 1) % len
        };
        let next = &scoped_models[next_index];
        let thinking_level = self.get_thinking_level_for_model_switch(next.thinking_level.clone());

        self.agent.lock().unwrap().state_mut().model = next.model.clone();
        self.session_manager
            .lock()
            .unwrap()
            .append_model_change(next.model.provider.clone(), next.model.id.clone());
        self.settings_manager.lock().unwrap().set_default_provider(&next.model.provider);
        self.settings_manager.lock().unwrap().set_default_model(&next.model.id);
        self.set_thinking_level(&thinking_level);

        self.emit(&AgentSessionEvent::ModelSelect {
            provider: next.model.provider.clone(),
            model_id: next.model.id.clone(),
            source: "cycle".to_string(),
        });

        Some(ModelCycleResult {
            model: next.model.clone(),
            thinking_level: self.thinking_level(),
            is_scoped: true,
        })
    }

    fn cycle_available_model(&self, direction: &str) -> Option<ModelCycleResult> {
        let available_models = self.model_runtime.get_available_snapshot();
        if available_models.len() <= 1 {
            return None;
        }
        let current = self.model();
        let mut current_index = available_models
            .iter()
            .position(|model| models_are_equal(Some(model), current.as_ref()))
            .unwrap_or(0);
        if current_index == usize::MAX {
            current_index = 0;
        }
        let len = available_models.len();
        let next_index = if direction == "forward" {
            (current_index + 1) % len
        } else {
            (current_index + len - 1) % len
        };
        let next_model = &available_models[next_index];

        let thinking_level = self.get_thinking_level_for_model_switch(None);
        self.agent.lock().unwrap().state_mut().model = next_model.clone();
        self.session_manager
            .lock()
            .unwrap()
            .append_model_change(next_model.provider.clone(), next_model.id.clone());
        self.settings_manager.lock().unwrap().set_default_provider(&next_model.provider);
        self.settings_manager.lock().unwrap().set_default_model(&next_model.id);
        self.set_thinking_level(&thinking_level);

        self.emit(&AgentSessionEvent::ModelSelect {
            provider: next_model.provider.clone(),
            model_id: next_model.id.clone(),
            source: "cycle".to_string(),
        });

        Some(ModelCycleResult {
            model: next_model.clone(),
            thinking_level: self.thinking_level(),
            is_scoped: false,
        })
    }

    // -----------------------------------------------------------------------
    // Thinking level management
    // -----------------------------------------------------------------------

    pub fn get_available_thinking_levels(&self) -> Vec<String> {
        match self.model() {
            Some(model) if model.reasoning => THINKING_LEVELS.iter().map(|value| value.to_string()).collect(),
            _ => THINKING_LEVELS.iter().map(|value| value.to_string()).collect(),
        }
    }

    pub fn supports_thinking(&self) -> bool {
        self.model().is_some_and(|model| model.reasoning)
    }

    fn clamp_thinking_level(&self, level: &str) -> String {
        match self.model() {
            Some(model) if model.reasoning => {
                if THINKING_LEVELS.contains(&level) {
                    level.to_string()
                } else {
                    "off".to_string()
                }
            }
            _ => "off".to_string(),
        }
    }

    pub fn set_thinking_level(&self, level: &str) {
        let available_levels = self.get_available_thinking_levels();
        let effective_level = if available_levels.iter().any(|value| value == level) {
            level.to_string()
        } else {
            self.clamp_thinking_level(level)
        };

        let previous_level = self.agent.lock().unwrap().state().thinking_level;
        let is_changing = effective_level != previous_level;

        self.agent.lock().unwrap().state_mut().thinking_level = effective_level.clone();

        if is_changing {
            self.session_manager.lock().unwrap().append_thinking_level_change(effective_level.clone());
            if self.supports_thinking() || effective_level != "off" {
                self.settings_manager.lock().unwrap().set_default_thinking_level(&effective_level);
            }
            self.emit(&AgentSessionEvent::ThinkingLevelChanged {
                level: effective_level,
            });
        }
    }

    pub fn cycle_thinking_level(&self) -> Option<String> {
        if !self.supports_thinking() {
            return None;
        }
        let levels = self.get_available_thinking_levels();
        let current_index = levels.iter().position(|level| *level == self.thinking_level()).unwrap_or(0);
        let next_index = (current_index + 1) % levels.len();
        let next_level = levels[next_index].clone();
        self.set_thinking_level(&next_level);
        Some(next_level)
    }

    fn get_thinking_level_for_model_switch(&self, explicit_level: Option<String>) -> String {
        match explicit_level {
            Some(level) => level,
            None if !self.supports_thinking() => self
                .settings_manager
                .lock()
                .unwrap()
                .get_default_thinking_level()
                .unwrap_or_else(|| "medium".to_string()),
            None => self.thinking_level(),
        }
    }

    // -----------------------------------------------------------------------
    // Compaction
    // -----------------------------------------------------------------------

    fn compaction_settings(&self) -> CompactionSettings {
        let settings = self.settings_manager.lock().unwrap();
        CompactionSettings {
            enabled: settings.get_compaction_enabled(),
            reserve_tokens: settings.get_compaction_reserve_tokens(),
            keep_recent_tokens: settings.get_compaction_keep_recent_tokens(),
        }
    }

    /// Manually compact the session context.
    pub fn compact(&self, custom_instructions: Option<&str>) -> Result<CompactionResult, String> {
        let settings = self.compaction_settings();
        let entries = self.session_manager.lock().unwrap().get_entries();
        let by_id = crate::core::session_types::build_entry_index(&entries);
        let context_entries =
            crate::core::session_types::build_context_entries(&entries, self.session_manager.lock().unwrap().get_leaf_id().as_deref(), &by_id);
        let path_entries: Vec<SessionEntry> = context_entries.into_iter().cloned().collect();

        let Some(preparation) = prepare_compaction(&path_entries, &settings) else {
            return Err("Nothing to compact".to_string());
        };

        let Some(model) = self.model() else {
            return Err("No model selected".to_string());
        };

        self.emit(&AgentSessionEvent::AutoCompactionStart);
        let result = compact(
            &preparation,
            &model,
            &SummaryCallOptions {
                api_key: None,
                headers: None,
                signal: None,
                custom_instructions,
                previous_summary: None,
                thinking_level: None,
                stream_fn: None,
                env: None,
                retry: None,
                callbacks: None,
            },
        );
        self.emit(&AgentSessionEvent::AutoCompactionEnd);

        let result = result?;
        self.session_manager.lock().unwrap().append_compaction(
            result.summary.clone(),
            result.first_kept_entry_id.clone(),
            result.tokens_before,
            result.details.clone(),
            Some(false),
            result.usage.clone(),
        );
        Ok(result)
    }

    fn check_compaction(&self, assistant_message: &AssistantMessage, skip_aborted_check: bool) -> Result<bool, String> {
        let _ = skip_aborted_check;
        // Auto-compaction on context overflow.
        if !self.auto_compaction_enabled.load(Ordering::SeqCst) {
            return Ok(false);
        }
        if assistant_message.stop_reason == pi_ai::types::StopReason::Length
            && !self.overflow_recovery_attempted.load(Ordering::SeqCst)
        {
            self.overflow_recovery_attempted.store(true, Ordering::SeqCst);
            let settings = self.compaction_settings();
            if settings.enabled {
                return self.run_auto_compaction("overflow", false);
            }
        }
        // Threshold-based compaction.
        let model = match self.model() {
            Some(model) => model,
            None => return Ok(false),
        };
        let context_window = model.context_window;
        if context_window > 0.0 {
            let context_tokens = self.estimate_context_tokens();
            if should_compact(context_tokens, context_window, &self.compaction_settings()) {
                return self.run_auto_compaction("threshold", false);
            }
        }
        Ok(false)
    }

    fn estimate_context_tokens(&self) -> f64 {
        let entries = self.session_manager.lock().unwrap().get_entries();
        let by_id = crate::core::session_types::build_entry_index(&entries);
        let context = crate::core::session_types::build_session_context(
            &entries,
            self.session_manager.lock().unwrap().get_leaf_id().as_deref(),
            &by_id,
        );
        let mut tokens = 0.0;
        for message in &context.messages {
            if let AgentMessage::Llm(message) = message {
                tokens += calculate_context_tokens(&message_usage(message));
            }
        }
        if tokens > 0.0 {
            tokens
        } else {
            // Estimate fallback: chars/4 over serialized messages.
            let serialized = crate::core::compaction::utils::serialize_conversation(
                &crate::core::messages::convert_to_llm(&context.messages),
            );
            (serialized.len() as f64 / 4.0).ceil()
        }
    }

    fn run_auto_compaction(&self, reason: &str, will_retry: bool) -> Result<bool, String> {
        let _ = will_retry;
        let settings = self.compaction_settings();
        let entries = self.session_manager.lock().unwrap().get_entries();
        let by_id = crate::core::session_types::build_entry_index(&entries);
        let context_entries =
            crate::core::session_types::build_context_entries(&entries, self.session_manager.lock().unwrap().get_leaf_id().as_deref(), &by_id);
        let path_entries: Vec<SessionEntry> = context_entries.into_iter().cloned().collect();
        let Some(preparation) = prepare_compaction(&path_entries, &settings) else {
            return Ok(false);
        };
        let Some(model) = self.model() else {
            return Ok(false);
        };

        self.emit(&AgentSessionEvent::AutoCompactionStart);
        let result = compact(
            &preparation,
            &model,
            &SummaryCallOptions {
                api_key: None,
                headers: None,
                signal: None,
                custom_instructions: None,
                previous_summary: None,
                thinking_level: None,
                stream_fn: None,
                env: None,
                retry: None,
                callbacks: None,
            },
        );
        self.emit(&AgentSessionEvent::AutoCompactionEnd);
        let result = match result {
            Ok(result) => result,
            Err(_) => return Ok(false),
        };
        let _ = reason;
        self.session_manager.lock().unwrap().append_compaction(
            result.summary.clone(),
            result.first_kept_entry_id.clone(),
            result.tokens_before,
            result.details.clone(),
            Some(false),
            result.usage.clone(),
        );
        Ok(true)
    }

    pub fn set_auto_compaction_enabled(&self, enabled: bool) {
        self.auto_compaction_enabled.store(enabled, Ordering::SeqCst);
    }

    // -----------------------------------------------------------------------
    // Retry
    // -----------------------------------------------------------------------

    fn is_retryable_error(&self, message: &AssistantMessage) -> bool {
        if !self.auto_retry_enabled.load(Ordering::SeqCst) {
            return false;
        }
        let settings = self.settings_manager.lock().unwrap();
        if !settings.get_retry_enabled() || *self.retry_count.lock().unwrap() >= settings.get_retry_max_retries() as usize {
            return false;
        }
        is_retryable_assistant_error(message)
    }

    fn prepare_retry(&self, message: &AssistantMessage) -> Result<bool, String> {
        if !self.is_retryable_error(message) {
            return Ok(false);
        }
        let attempt = *self.retry_count.lock().unwrap() + 1;
        *self.retry_count.lock().unwrap() = attempt;
        self.retry_attempt.store(true, Ordering::SeqCst);
        // ponytail: the JS retry waits for backoff before re-prompting; the
        // sync loop retries immediately with the same messages.
        let messages = self.agent.lock().unwrap().state().messages;
        self.agent.lock().unwrap().prompt(messages)?;
        self.retry_attempt.store(false, Ordering::SeqCst);
        Ok(true)
    }

    pub fn abort_retry(&self) {
        self.retry_attempt.store(false, Ordering::SeqCst);
    }

    pub fn retry_attempt(&self) -> usize {
        *self.retry_count.lock().unwrap()
    }

    pub fn set_auto_retry_enabled(&self, enabled: bool) {
        self.auto_retry_enabled.store(enabled, Ordering::SeqCst);
    }

    // -----------------------------------------------------------------------
    // Bash
    // -----------------------------------------------------------------------

    /// Execute a bash command with the given operations, appending a
    /// BashExecutionMessage to the pending queue.
    pub fn execute_bash(
        &self,
        command: &str,
        cwd: &str,
        operations: &dyn BashOperations,
        on_chunk: Option<&mut dyn FnMut(&str)>,
    ) -> Result<BashResult, String> {
        self.bash_abort.store(false, Ordering::SeqCst);
        let result = execute_bash_with_operations(command, cwd, operations, on_chunk, &self.bash_abort)?;
        self.record_bash_result(command, &result, false);
        Ok(result)
    }

    pub fn record_bash_result(&self, command: &str, result: &BashResult, exclude_from_context: bool) {
        let message = AgentMessage::Custom(Arc::new(super::messages::BashExecutionMessage {
            command: command.to_string(),
            output: result.output.clone(),
            exit_code: result.exit_code,
            cancelled: result.cancelled,
            truncated: result.truncated,
            full_output_path: result.full_output_path.clone(),
            timestamp: now_ms(),
            exclude_from_context: if exclude_from_context { Some(true) } else { None },
        }));
        self.pending_bash_messages.lock().unwrap().push(message);
    }

    pub fn abort_bash(&self) {
        self.bash_abort.store(true, Ordering::SeqCst);
    }

    pub fn is_bash_running(&self) -> bool {
        self.bash_abort.load(Ordering::SeqCst)
    }

    pub fn has_pending_bash_messages(&self) -> bool {
        !self.pending_bash_messages.lock().unwrap().is_empty()
    }

    fn flush_pending_bash_messages(&self) {
        let messages = std::mem::take(&mut *self.pending_bash_messages.lock().unwrap());
        for message in messages {
            if let AgentMessage::Custom(custom) = &message {
                if let Some(bash) = custom.as_any().downcast_ref::<super::messages::BashExecutionMessage>() {
                    self.session_manager.lock().unwrap().append_message(
                        crate::core::session_types::SessionMessage::Bash(bash.clone()),
                    );
                    continue;
                }
            }
            self.session_manager.lock().unwrap().append_message(crate::core::session_types::SessionMessage::Unknown(
                pi_protocol::Value::Map(vec![("role".to_string(), pi_protocol::Value::String("custom".to_string()))]),
            ));
        }
    }

    // -----------------------------------------------------------------------
    // Session management
    // -----------------------------------------------------------------------

    pub fn set_session_name(&self, name: &str) {
        let sanitized = name.replace(['\r', '\n'], " ").trim().to_string();
        self.session_manager.lock().unwrap().append_session_info(sanitized);
    }

    /// Navigate the session tree (branch or summary-branch).
    pub fn navigate_tree(&self, entry_id: &str, options: &TreeNavigationOptions) -> Result<(), String> {
        if options.append_branch_summary {
            if let Some(summary) = &options.summary {
                self.session_manager.lock().unwrap().branch_with_summary(
                    Some(entry_id.to_string()),
                    summary.clone(),
                    None,
                    Some(false),
                    None,
                );
                return Ok(());
            }
            return Err("Summary required for branchWithSummary".to_string());
        }
        self.session_manager.lock().unwrap().branch(entry_id);
        Ok(())
    }

    /// User messages visible for forking.
    pub fn get_user_messages_for_forking(&self) -> Vec<(String, String)> {
        let entries = self.session_manager.lock().unwrap().get_entries();
        let mut result = Vec::new();
        for entry in &entries {
            if let SessionEntry::Message { message, .. } = entry {
                if let crate::core::session_types::SessionMessage::Llm(Message::User(user)) = message {
                    let text = match &user.content {
                        UserMessageContent::Text(text) => text.clone(),
                        UserMessageContent::Blocks(blocks) => blocks
                            .iter()
                            .filter_map(|block| match block {
                                pi_ai::types::Content::Text(text) => Some(text.text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(" "),
                    };
                    if !text.is_empty() {
                        result.push((entry.id().to_string(), text));
                    }
                }
            }
        }
        result
    }

    pub fn get_last_assistant_text(&self) -> Option<String> {
        let state = self.agent.lock().unwrap().state();
        for message in state.messages.iter().rev() {
            if let AgentMessage::Llm(Message::Assistant(assistant)) = message {
                let text = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        pi_ai::types::Content::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
        None
    }

    /// Abort the current operation.
    pub fn abort(&self) {
        self.abort_retry();
        self.agent.lock().unwrap().abort();
    }

    pub fn dispose(&self) {
        if self.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.abort_retry();
        self.agent.lock().unwrap().abort();
        self.steering_messages.lock().unwrap().clear();
        self.follow_up_messages.lock().unwrap().clear();
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}

fn user_message_with_images(text: &str, images: Option<&[pi_ai::types::ImageContent]>) -> AgentMessage {
    let mut content: Vec<pi_ai::types::Content> = vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
        text: text.to_string(),
        text_signature: None,
    })];
    if let Some(images) = images {
        for image in images {
            content.push(pi_ai::types::Content::Image(image.clone()));
        }
    }
    AgentMessage::Llm(Message::User(UserMessage {
        content: UserMessageContent::Blocks(content),
        timestamp: now_ms(),
    }))
}

fn message_usage(message: &Message) -> pi_ai::types::Usage {
    match message {
        Message::Assistant(assistant) => assistant.usage.clone(),
        _ => pi_ai::types::Usage {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0.0,
            cost: pi_ai::types::UsageCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
        },
    }
}

pub struct TreeNavigationOptions {
    pub append_branch_summary: bool,
    pub summary: Option<String>,
}

impl Default for TreeNavigationOptions {
    fn default() -> Self {
        Self {
            append_branch_summary: false,
            summary: None,
        }
    }
}

impl AgentSession {
    /// Rebuild the base system prompt from the resource loader (used at
    /// construction and after extension reloads).
    pub fn rebuild_system_prompt(&self, tool_names: &[String]) -> String {
        let loader = self.resource_loader.lock().unwrap();
        let loader_system_prompt = loader.get_system_prompt().unwrap_or("").to_string();
        let loader_append = loader.get_append_system_prompt().to_vec();
        let append_system_prompt = if loader_append.is_empty() {
            None
        } else {
            Some(loader_append.join("\n\n"))
        };
        let skills: Vec<crate::core::skills::Skill> = loader
            .get_skills()
            .iter()
            .map(|skill| crate::core::skills::Skill {
                name: skill.name.clone(),
                description: skill.description.clone(),
                file_path: skill.file_path.clone(),
                base_dir: std::path::Path::new(&skill.file_path)
                    .parent()
                    .map(|parent| parent.to_string_lossy().to_string())
                    .unwrap_or_default(),
                source_info: crate::core::source_info::SourceInfo {
                    path: skill.file_path.clone(),
                    source: "local".into(),
                    scope: "user".into(),
                    origin: "top-level".into(),
                    base_dir: None,
                },
                disable_model_invocation: skill.disable_model_invocation.unwrap_or(false),
            })
            .collect();
        let context_files: Vec<(String, String)> = loader.get_agents_files().to_vec();

        let prompt = crate::core::system_prompt::build_system_prompt(&crate::core::system_prompt::BuildSystemPromptOptions {
            custom_prompt: if loader_system_prompt.is_empty() {
                None
            } else {
                Some(loader_system_prompt)
            },
            selected_tools: Some(tool_names.to_vec()),
            tool_snippets: None,
            prompt_guidelines: None,
            append_system_prompt,
            cwd: self.cwd.clone(),
            context_files,
            skills,
        });
        *self.base_system_prompt.lock().unwrap() = prompt.clone();
        self.agent.lock().unwrap().state_mut().system_prompt = prompt.clone();
        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::resource_loader::DefaultResourceLoaderOptions;
    use pi_ai::types::{ModelCost, ModelCostRates};

    fn model() -> Model {
        Model {
            id: "m1".into(),
            name: "M1".into(),
            api: "openai".into(),
            provider: "acme".into(),
            base_url: "https://a.example".into(),
            reasoning: true,
            thinking_level_map: None,
            input: vec!["text".into()],
            cost: ModelCost {
                rates: ModelCostRates {
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
        }
    }

    #[test]
    fn thinking_level_clamping() {
        let mut agent = Agent::new(pi_agent_core::agent::AgentOptions::default());
        agent.state_mut().model = model();
        agent.state_mut().thinking_level = "off".to_string();
        let session = AgentSession::new(AgentSessionConfig {
            agent,
            session_manager: SessionManager::in_memory(None, None),
            settings_manager: SettingsManager::in_memory(pi_protocol::Value::Map(Vec::new())),
            scoped_models: Vec::new(),
            resource_loader: DefaultResourceLoader::new(DefaultResourceLoaderOptions {
                cwd: "/tmp".into(),
                agent_dir: "/tmp".into(),
                ..Default::default()
            }),
            custom_tools: Vec::new(),
            cwd: "/tmp".into(),
            model_runtime: Arc::new(ModelRuntime::create(crate::core::model_runtime::CreateModelRuntimeOptions::default())),
        });
        session.set_thinking_level("high");
        assert_eq!(session.thinking_level(), "high");
        session.set_thinking_level("bogus");
        assert_eq!(session.thinking_level(), "off");
        assert!(session.supports_thinking());
        let levels = session.get_available_thinking_levels();
        assert!(levels.contains(&"high".to_string()));
        let next = session.cycle_thinking_level().unwrap();
        assert_eq!(next, "minimal");
    }

    #[test]
    fn pending_queues() {
        let agent = Agent::new(pi_agent_core::agent::AgentOptions::default());
        let session = AgentSession::new(AgentSessionConfig {
            agent,
            session_manager: SessionManager::in_memory(None, None),
            settings_manager: SettingsManager::in_memory(pi_protocol::Value::Map(Vec::new())),
            scoped_models: Vec::new(),
            resource_loader: DefaultResourceLoader::new(DefaultResourceLoaderOptions {
                cwd: "/tmp".into(),
                agent_dir: "/tmp".into(),
                ..Default::default()
            }),
            custom_tools: Vec::new(),
            cwd: "/tmp".into(),
            model_runtime: Arc::new(ModelRuntime::create(crate::core::model_runtime::CreateModelRuntimeOptions::default())),
        });
        assert_eq!(session.pending_message_count(), 0);
        session.steer("hello", None).unwrap();
        assert_eq!(session.pending_message_count(), 1);
        assert_eq!(session.get_steering_messages(), vec!["hello".to_string()]);
        let (steering, follow_up) = session.clear_queue();
        assert_eq!(steering, vec!["hello".to_string()]);
        assert!(follow_up.is_empty());
        assert_eq!(session.pending_message_count(), 0);
    }

    #[test]
    fn session_name_sanitization() {
        let agent = Agent::new(pi_agent_core::agent::AgentOptions::default());
        let session = AgentSession::new(AgentSessionConfig {
            agent,
            session_manager: SessionManager::in_memory(None, None),
            settings_manager: SettingsManager::in_memory(pi_protocol::Value::Map(Vec::new())),
            scoped_models: Vec::new(),
            resource_loader: DefaultResourceLoader::new(DefaultResourceLoaderOptions {
                cwd: "/tmp".into(),
                agent_dir: "/tmp".into(),
                ..Default::default()
            }),
            custom_tools: Vec::new(),
            cwd: "/tmp".into(),
            model_runtime: Arc::new(ModelRuntime::create(crate::core::model_runtime::CreateModelRuntimeOptions::default())),
        });
        session.set_session_name("my\nsession");
        assert_eq!(session.get_session_name().as_deref(), Some("my session"));
    }

    #[test]
    fn prompt_requires_model() {
        let agent = Agent::new(pi_agent_core::agent::AgentOptions::default());
        let session = AgentSession::new(AgentSessionConfig {
            agent,
            session_manager: SessionManager::in_memory(None, None),
            settings_manager: SettingsManager::in_memory(pi_protocol::Value::Map(Vec::new())),
            scoped_models: Vec::new(),
            resource_loader: DefaultResourceLoader::new(DefaultResourceLoaderOptions {
                cwd: "/tmp".into(),
                agent_dir: "/tmp".into(),
                ..Default::default()
            }),
            custom_tools: Vec::new(),
            cwd: "/tmp".into(),
            model_runtime: Arc::new(ModelRuntime::create(crate::core::model_runtime::CreateModelRuntimeOptions::default())),
        });
        let error = session.prompt("hi", &PromptOptions::default()).unwrap_err();
        assert!(error.contains("No model selected"), "got: {error}");
    }
}
