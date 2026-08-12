//! Extension runner, port of
//! `packages/coding-agent/src/core/extensions/runner.ts` (core: runtime
//! binding, context creation, event dispatch with error isolation, tool/
//! flag/command collection, shortcut conflict diagnostics, invalidation).
//!
//! SessionManager/ModelRegistry are represented by optional action
//! closures; UI context and message renderers are light local types.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pi_protocol::cbor::Value;

use crate::core::extensions::loader::{ExtensionFlag, ExtensionRuntime, FlagValue};
use crate::core::extensions::types::{Extension, RegisteredCommand, RegisteredTool, ToolDefinition};
use crate::core::resource_loader::ResourceDiagnostic;

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedCommand {
    pub name: String,
    pub invocation_name: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ExtensionShortcut {
    pub shortcut: String,
    pub extension_path: String,
    pub description: Option<String>,
}

/// Actions bound into the shared runtime by the host (JS ExtensionActions).
#[derive(Clone, Default)]
pub struct ExtensionActions {
    pub send_message: Option<Arc<dyn Fn(Value, Option<Value>) + Send + Sync>>,
    pub append_entry: Option<Arc<dyn Fn(&str, Option<Value>) + Send + Sync>>,
    pub set_session_name: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub get_session_name: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    pub set_label: Option<Arc<dyn Fn(&str, Option<&str>) + Send + Sync>>,
    pub get_active_tools: Option<Arc<dyn Fn() -> Vec<String> + Send + Sync>>,
    pub set_active_tools: Option<Arc<dyn Fn(Vec<String>) + Send + Sync>>,
    pub get_commands: Option<Arc<dyn Fn() -> Vec<String> + Send + Sync>>,
    pub get_thinking_level: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    pub set_thinking_level: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

/// Extension error record (JS `ExtensionError`).
#[derive(Clone, Debug, PartialEq)]
pub struct ExtensionError {
    pub extension_path: String,
    pub event: String,
    pub error: String,
}

pub type ExtensionErrorListener = Arc<dyn Fn(&ExtensionError) + Send + Sync>;

/// Result of a session_before_* event (JS `SessionBeforeEventResult`).
#[derive(Clone, Debug, Default)]
pub struct SessionBeforeEventResult {
    pub cancel: bool,
}

/// Context view exposed to event handlers.
pub struct ExtensionContext {
    pub cwd: String,
    pub mode: String,
    pub has_ui: bool,
    pub is_idle: Arc<dyn Fn() -> bool + Send + Sync>,
    pub is_project_trusted: Arc<dyn Fn() -> bool + Send + Sync>,
    pub abort: Arc<dyn Fn() + Send + Sync>,
    pub has_pending_messages: Arc<dyn Fn() -> bool + Send + Sync>,
    pub get_system_prompt: Arc<dyn Fn() -> String + Send + Sync>,
}

/// The extension runner executes extensions and manages their lifecycle.
pub struct ExtensionRunner {
    pub extensions: Vec<Extension>,
    pub runtime: Arc<ExtensionRuntime>,
    pub cwd: String,
    mode: String,
    has_ui: bool,
    error_listeners: Vec<ExtensionErrorListener>,
    stale_message: Option<String>,
    // Host-bound actions.
    actions: ExtensionActions,
    get_model: Option<Arc<dyn Fn() -> Option<Value> + Send + Sync>>,
    is_idle_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    is_project_trusted_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    #[allow(dead_code)]
    get_signal_fn: Arc<dyn Fn() -> Option<()> + Send + Sync>,
    abort_fn: Arc<dyn Fn() + Send + Sync>,
    has_pending_messages_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    get_system_prompt_fn: Arc<dyn Fn() -> String + Send + Sync>,
    get_thinking_level_fn: Arc<dyn Fn() -> String + Send + Sync>,
    shutdown_handler: Arc<dyn Fn() + Send + Sync>,
    shortcut_diagnostics: Vec<ResourceDiagnostic>,
    #[allow(dead_code)]
    command_diagnostics: Vec<ResourceDiagnostic>,
}

impl ExtensionRunner {
    pub fn new(extensions: Vec<Extension>, runtime: Arc<ExtensionRuntime>, cwd: &str) -> Self {
        Self {
            extensions,
            runtime,
            cwd: cwd.to_string(),
            mode: "print".to_string(),
            has_ui: false,
            error_listeners: Vec::new(),
            stale_message: None,
            actions: ExtensionActions::default(),
            get_model: None,
            is_idle_fn: Arc::new(|| true),
            is_project_trusted_fn: Arc::new(|| true),
            get_signal_fn: Arc::new(|| None),
            abort_fn: Arc::new(|| {}),
            has_pending_messages_fn: Arc::new(|| false),
            get_system_prompt_fn: Arc::new(|| String::new()),
            get_thinking_level_fn: Arc::new(|| "off".to_string()),
            shutdown_handler: Arc::new(|| {}),
            shortcut_diagnostics: Vec::new(),
            command_diagnostics: Vec::new(),
        }
    }

    /// Bind host actions into the shared runtime (JS `bindCore`).
    pub fn bind_core(&mut self, actions: ExtensionActions) {
        if let Some(send_message) = &actions.send_message {
            *self.runtime.send_message.lock().unwrap() = Some(send_message.clone());
        }
        if let Some(append_entry) = &actions.append_entry {
            *self.runtime.append_entry.lock().unwrap() = Some(append_entry.clone());
        }
        if let Some(set_session_name) = &actions.set_session_name {
            *self.runtime.set_session_name.lock().unwrap() = Some(set_session_name.clone());
        }
        if let Some(get_session_name) = &actions.get_session_name {
            *self.runtime.get_session_name.lock().unwrap() = Some(get_session_name.clone());
        }
        if let Some(set_label) = &actions.set_label {
            *self.runtime.set_label.lock().unwrap() = Some(set_label.clone());
        }
        if let Some(get_active_tools) = &actions.get_active_tools {
            *self.runtime.get_active_tools.lock().unwrap() = Some(get_active_tools.clone());
        }
        if let Some(set_active_tools) = &actions.set_active_tools {
            *self.runtime.set_active_tools.lock().unwrap() = Some(set_active_tools.clone());
        }
        if let Some(get_commands) = &actions.get_commands {
            *self.runtime.get_commands.lock().unwrap() = Some(get_commands.clone());
        }
        if let Some(get_thinking_level) = &actions.get_thinking_level {
            *self.runtime.get_thinking_level.lock().unwrap() = Some(get_thinking_level.clone());
        }
        if let Some(set_thinking_level) = &actions.set_thinking_level {
            *self.runtime.set_thinking_level.lock().unwrap() = Some(set_thinking_level.clone());
        }
        self.actions = actions;
    }

    /// Bind context-provider actions (JS bindCore contextActions subset).
    pub fn bind_context_actions(
        &mut self,
        get_model: Option<Arc<dyn Fn() -> Option<Value> + Send + Sync>>,
        is_idle: Arc<dyn Fn() -> bool + Send + Sync>,
        is_project_trusted: Arc<dyn Fn() -> bool + Send + Sync>,
        abort: Arc<dyn Fn() + Send + Sync>,
        has_pending_messages: Arc<dyn Fn() -> bool + Send + Sync>,
        get_system_prompt: Arc<dyn Fn() -> String + Send + Sync>,
        get_thinking_level: Arc<dyn Fn() -> String + Send + Sync>,
        shutdown: Arc<dyn Fn() + Send + Sync>,
    ) {
        self.get_model = get_model;
        self.is_idle_fn = is_idle;
        self.is_project_trusted_fn = is_project_trusted;
        self.abort_fn = abort;
        self.has_pending_messages_fn = has_pending_messages;
        self.get_system_prompt_fn = get_system_prompt;
        self.get_thinking_level_fn = get_thinking_level;
        self.shutdown_handler = shutdown;
    }

    pub fn set_ui_context(&mut self, has_ui: bool, mode: &str) {
        self.has_ui = has_ui;
        self.mode = mode.to_string();
    }

    pub fn has_ui(&self) -> bool {
        self.has_ui
    }

    pub fn get_extension_paths(&self) -> Vec<String> {
        self.extensions.iter().map(|extension| extension.path.clone()).collect()
    }

    /// All registered tools; first registration per name wins (JS
    /// `getAllRegisteredTools`).
    pub fn get_all_registered_tools(&self) -> Vec<RegisteredTool> {
        let mut tools_by_name: HashMap<String, RegisteredTool> = HashMap::new();
        for extension in &self.extensions {
            for (name, tool) in &extension.tools {
                if !tools_by_name.contains_key(name) {
                    tools_by_name.insert(name.clone(), tool.clone());
                }
            }
        }
        tools_by_name.into_values().collect()
    }

    /// Get a tool definition by name (JS `getToolDefinition`).
    pub fn get_tool_definition(&self, tool_name: &str) -> Option<ToolDefinition> {
        for extension in &self.extensions {
            if let Some(tool) = extension.tools.get(tool_name) {
                return Some(tool.definition.clone());
            }
        }
        None
    }

    /// All flags; first registration per name wins (JS `getFlags`).
    pub fn get_flags(&self) -> Vec<ExtensionFlag> {
        let mut all_flags: HashMap<String, ExtensionFlag> = HashMap::new();
        for extension in &self.extensions {
            for (name, flag) in &extension.flags {
                if !all_flags.contains_key(name) {
                    all_flags.insert(name.clone(), flag.clone());
                }
            }
        }
        all_flags.into_values().collect()
    }

    pub fn set_flag_value(&self, name: &str, value: FlagValue) {
        self.runtime.flag_values.lock().unwrap().insert(name.to_string(), value);
    }

    pub fn get_flag_values(&self) -> Vec<(String, FlagValue)> {
        self.runtime.flag_values.lock().unwrap().clone().into_iter().collect()
    }

    pub fn get_shortcut_diagnostics(&self) -> &[ResourceDiagnostic] {
        &self.shortcut_diagnostics
    }

    pub fn invalidate(&mut self, message: Option<&str>) {
        if self.stale_message.is_none() {
            let message = message.unwrap_or(
                "This extension ctx is stale after session replacement or reload. Do not use a captured pi or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload().",
            );
            self.stale_message = Some(message.to_string());
            self.runtime.invalidate(Some(message));
        }
    }

    fn assert_active(&self) -> Result<(), String> {
        match &self.stale_message {
            Some(message) => Err(message.clone()),
            None => Ok(()),
        }
    }

    pub fn on_error(&mut self, listener: ExtensionErrorListener) {
        self.error_listeners.push(listener);
    }

    pub fn emit_error(&self, error: &ExtensionError) {
        for listener in &self.error_listeners {
            listener(error);
        }
    }

    /// Does any extension have handlers for an event type? (JS
    /// `hasHandlers`).
    pub fn has_handlers(&self, event_type: &str) -> bool {
        self.extensions
            .iter()
            .any(|extension| extension.handlers.get(event_type).is_some_and(|handlers| !handlers.is_empty()))
    }

    /// Dispatch an event to all handlers, isolating errors (JS `emit`).
    /// Session-before events support cancel semantics.
    pub fn emit(&self, event_type: &str, event: Value, cancel_on_request: bool) -> Option<SessionBeforeEventResult> {
        let mut result: Option<SessionBeforeEventResult> = None;
        for extension in &self.extensions {
            let Some(handlers) = extension.handlers.get(event_type) else {
                continue;
            };
            for handler in handlers {
                let handler_result = handler(event.clone());
                match handler_result {
                    Ok(Some(_)) if cancel_on_request => {
                        result = Some(SessionBeforeEventResult { cancel: true });
                        return result;
                    }
                    Ok(_) => {}
                    Err(message) => {
                        self.emit_error(&ExtensionError {
                            extension_path: extension.path.clone(),
                            event: event_type.to_string(),
                            error: message,
                        });
                    }
                }
            }
        }
        result
    }

    /// Dispatch an event allowing handler chains to transform a value (JS
    /// `emitContext`-style fold).
    pub fn emit_fold(&self, event_type: &str, mut current: Value) -> Value {
        for extension in &self.extensions {
            let Some(handlers) = extension.handlers.get(event_type) else {
                continue;
            };
            for handler in handlers {
                match handler(Value::Map(vec![
                    ("type".to_string(), Value::String(event_type.to_string())),
                    ("value".to_string(), current.clone()),
                ])) {
                    Ok(Some(transformed)) => current = transformed,
                    Ok(None) => {}
                    Err(message) => {
                        self.emit_error(&ExtensionError {
                            extension_path: extension.path.clone(),
                            event: event_type.to_string(),
                            error: message,
                        });
                    }
                }
            }
        }
        current
    }

    /// Resolve registered commands with invocation-name disambiguation (JS
    /// `resolveRegisteredCommands`).
    pub fn resolve_registered_commands(&self) -> Vec<ResolvedCommand> {
        let mut commands: Vec<RegisteredCommand> = Vec::new();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for extension in &self.extensions {
            for command in extension.commands.values() {
                commands.push(command.clone());
                *counts.entry(command.name.clone()).or_insert(0) += 1;
            }
        }
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut taken: HashSet<String> = HashSet::new();
        commands
            .into_iter()
            .map(|command| {
                let occurrence = *seen.entry(command.name.clone()).or_insert(0) + 1;
                seen.insert(command.name.clone(), occurrence);
                let mut invocation_name = if counts.get(&command.name).copied().unwrap_or(0) > 1 {
                    format!("{}:{occurrence}", command.name)
                } else {
                    command.name.clone()
                };
                if taken.contains(&invocation_name) {
                    let mut suffix = occurrence;
                    loop {
                        suffix += 1;
                        invocation_name = format!("{}:{suffix}", command.name);
                        if !taken.contains(&invocation_name) {
                            break;
                        }
                    }
                }
                taken.insert(invocation_name.clone());
                ResolvedCommand {
                    name: command.name.clone(),
                    invocation_name,
                    description: command.description.clone(),
                }
            })
            .collect()
    }

    pub fn get_command(&self, name: &str) -> Option<ResolvedCommand> {
        self.resolve_registered_commands()
            .into_iter()
            .find(|command| command.invocation_name == name)
    }

    /// Request a graceful shutdown (JS `shutdown`).
    pub fn shutdown(&self) {
        (self.shutdown_handler)();
    }

    /// Create an extension context resolved at call time (JS
    /// `createContext`).
    pub fn create_context(&self) -> Result<ExtensionContext, String> {
        self.assert_active()?;
        Ok(ExtensionContext {
            cwd: self.cwd.clone(),
            mode: self.mode.clone(),
            has_ui: self.has_ui,
            is_idle: self.is_idle_fn.clone(),
            is_project_trusted: self.is_project_trusted_fn.clone(),
            abort: self.abort_fn.clone(),
            has_pending_messages: self.has_pending_messages_fn.clone(),
            get_system_prompt: self.get_system_prompt_fn.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> Arc<ExtensionRuntime> {
        ExtensionRuntime::new()
    }

    fn extension_with_handler(path: &str, event: &str, result: Option<Value>) -> Extension {
        let mut extension = Extension::default();
        extension.path = path.to_string();
        extension.handlers.insert(
            event.to_string(),
            vec![Arc::new(move |_event| Ok(result.clone()))],
        );
        extension
    }

    #[test]
    fn runner_dispatches_events_and_isolates_errors() {
        let mut failing = Extension::default();
        failing.path = "/failing".to_string();
        failing.handlers.insert(
            "session_start".to_string(),
            vec![Arc::new(|_event| Err("boom".to_string()))],
        );
        let ok = extension_with_handler("/ok", "session_start", None);
        let mut runner = ExtensionRunner::new(vec![failing, ok], runtime(), "/tmp");
        assert!(runner.has_handlers("session_start"));
        assert!(!runner.has_handlers("other"));
        let errors = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let errors_clone = errors.clone();
        runner.on_error(Arc::new(move |error| {
            errors_clone.lock().unwrap().push(error.error.clone());
        }));
        let result = runner.emit("session_start", Value::Map(Vec::new()), false);
        assert!(result.is_none());
        assert_eq!(errors.lock().unwrap().clone(), vec!["boom".to_string()]);
    }

    #[test]
    fn session_before_cancel_short_circuits() {
        let cancelling = extension_with_handler(
            "/cancel",
            "session_before_switch",
            Some(Value::Map(vec![("cancel".to_string(), Value::Bool(true))])),
        );
        let runner = ExtensionRunner::new(vec![cancelling], runtime(), "/tmp");
        let result = runner.emit("session_before_switch", Value::Map(Vec::new()), true);
        assert!(result.is_some() && result.unwrap().cancel);
    }

    #[test]
    fn tools_and_flags_collected_first_wins() {
        let mut ext_a = Extension::default();
        ext_a.path = "/a".to_string();
        ext_a.tools.insert(
            "tool-x".to_string(),
            RegisteredTool {
                definition: ToolDefinition::new("tool-x", "x", None, |_id, _params, _state| Ok(Value::Null)),
                hidden: false,
            },
        );
        ext_a.flags.insert(
            "verbose".to_string(),
            ExtensionFlag {
                name: "verbose".to_string(),
                extension_path: "/a".to_string(),
                description: None,
                kind: "boolean".to_string(),
                default: None,
            },
        );
        let mut ext_b = Extension::default();
        ext_b.path = "/b".to_string();
        ext_b.tools.insert(
            "tool-x".to_string(),
            RegisteredTool {
                definition: ToolDefinition::new("tool-x", "other", None, |_id, _params, _state| Ok(Value::Null)),
                hidden: false,
            },
        );
        let runner = ExtensionRunner::new(vec![ext_a, ext_b], runtime(), "/tmp");
        let tools = runner.get_all_registered_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].definition.description, "x");
        assert_eq!(runner.get_flags().len(), 1);
        assert_eq!(runner.get_tool_definition("tool-x").unwrap().description, "x");
    }

    #[test]
    fn commands_resolved_with_invocation_names() {
        let mut ext_a = Extension::default();
        ext_a.path = "/a".to_string();
        ext_a.commands.insert(
            "cmd".to_string(),
            RegisteredCommand {
                name: "cmd".to_string(),
                description: None,
                handler: Arc::new(|_event| Ok(None)),
            },
        );
        let mut ext_b = Extension::default();
        ext_b.path = "/b".to_string();
        ext_b.commands.insert(
            "cmd".to_string(),
            RegisteredCommand {
                name: "cmd".to_string(),
                description: None,
                handler: Arc::new(|_event| Ok(None)),
            },
        );
        let runner = ExtensionRunner::new(vec![ext_a, ext_b], runtime(), "/tmp");
        let commands = runner.resolve_registered_commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].invocation_name, "cmd:1");
        assert_eq!(commands[1].invocation_name, "cmd:2");
        assert!(runner.get_command("cmd:2").is_some());
    }

    #[test]
    fn invalidation_marks_runner_stale() {
        let mut runner = ExtensionRunner::new(Vec::new(), runtime(), "/tmp");
        runner.invalidate(None);
        assert!(runner.create_context().is_err());
    }
}
