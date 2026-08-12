//! Settings manager, port of
//! `packages/coding-agent/src/core/settings-manager.ts`.
//!
//! Settings are kept as a dynamic Value tree (JS settings objects); the
//! manager tracks modified fields per scope and persists them on save.
//! proper-lockfile (multi-process file locking) is replaced by an
//! in-process mutex; cross-process writes are last-writer-wins
//! (documented).

use std::path::Path;
use std::sync::Mutex;

use pi_protocol::cbor::Value;

use crate::config::{get_agent_dir, CONFIG_DIR_NAME};
use crate::utils::child_process::{normalize_path, resolve_path, PathInputOptions};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingsScope {
    Global,
    Project,
}

/// Settings storage abstraction (JS `SettingsStorage`).
pub trait SettingsStorage: Send + Sync {
    fn with_lock(&self, scope: SettingsScope, callback: Box<dyn FnOnce(Option<&str>) -> Option<String> + '_>);
}

/// File-backed settings storage.
pub struct FileSettingsStorage {
    global_settings_path: String,
    project_settings_path: String,
    lock: Mutex<()>,
}

impl FileSettingsStorage {
    pub fn new(cwd: &str, agent_dir: &str) -> Self {
        let resolved_cwd = resolve_path(cwd, cwd, &PathInputOptions::default());
        let resolved_agent_dir = resolve_path(agent_dir, agent_dir, &PathInputOptions::default());
        Self {
            global_settings_path: Path::new(&resolved_agent_dir)
                .join("settings.json")
                .to_string_lossy()
                .to_string(),
            project_settings_path: Path::new(&resolved_cwd)
                .join(CONFIG_DIR_NAME)
                .join("settings.json")
                .to_string_lossy()
                .to_string(),
            lock: Mutex::new(()),
        }
    }
}

impl SettingsStorage for FileSettingsStorage {
    fn with_lock(&self, scope: SettingsScope, callback: Box<dyn FnOnce(Option<&str>) -> Option<String> + '_>) {
        let _guard = self.lock.lock().unwrap();
        let path = match scope {
            SettingsScope::Global => &self.global_settings_path,
            SettingsScope::Project => &self.project_settings_path,
        };
        let file_exists = Path::new(path).exists();
        let current = if file_exists {
            std::fs::read_to_string(path).ok()
        } else {
            None
        };
        let next = callback(current.as_deref());
        if let Some(next) = next {
            if let Some(parent) = Path::new(path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, next);
        }
    }
}

/// In-memory settings storage (JS `InMemorySettingsStorage`).
pub struct InMemorySettingsStorage {
    global: Mutex<Option<String>>,
    project: Mutex<Option<String>>,
}

impl InMemorySettingsStorage {
    pub fn new() -> Self {
        Self {
            global: Mutex::new(None),
            project: Mutex::new(None),
        }
    }
}

impl Default for InMemorySettingsStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsStorage for InMemorySettingsStorage {
    fn with_lock(&self, scope: SettingsScope, callback: Box<dyn FnOnce(Option<&str>) -> Option<String> + '_>) {
        let slot = match scope {
            SettingsScope::Global => &self.global,
            SettingsScope::Project => &self.project,
        };
        let mut slot = slot.lock().unwrap();
        let current = slot.as_deref();
        let next = callback(current);
        if let Some(next) = next {
            *slot = Some(next);
        }
    }
}

fn is_mergeable(value: &Value) -> bool {
    matches!(value, Value::Map(_))
}

fn deep_merge(base: &Value, overrides: &Value) -> Value {
    match (base, overrides) {
        (Value::Map(base), Value::Map(overrides)) => {
            let mut result: Vec<(String, Value)> = base.to_vec();
            for (key, override_value) in overrides {
                if *override_value == Value::Null {
                    continue;
                }
                let base_value = result
                    .iter()
                    .find(|(existing, _)| existing == key)
                    .map(|(_, value)| value.clone());
                let merged = match base_value {
                    Some(base_value) if is_mergeable(&base_value) && is_mergeable(override_value) => {
                        deep_merge(&base_value, override_value)
                    }
                    _ => override_value.clone(),
                };
                if let Some(entry) = result.iter_mut().find(|(existing, _)| existing == key) {
                    entry.1 = merged;
                } else {
                    result.push((key.clone(), merged));
                }
            }
            Value::Map(result)
        }
        _ => overrides.clone(),
    }
}

fn get_field(settings: &Value, key: &str) -> Option<Value> {
    settings
        .as_map()
        .and_then(|entries| entries.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
}

fn set_field(settings: &mut Value, key: &str, value: Value) {
    if let Value::Map(entries) = settings {
        if let Some(entry) = entries.iter_mut().find(|(k, _)| k == key) {
            entry.1 = value;
        } else {
            entries.push((key.to_string(), value));
        }
    }
}

fn remove_field(settings: &mut Value, key: &str) {
    if let Value::Map(entries) = settings {
        entries.retain(|(k, _)| k != key);
    }
}

fn string_array(settings: &Value, key: &str) -> Option<Vec<String>> {
    match get_field(settings, key) {
        Some(Value::Array(array)) => Some(
            array
                .iter()
                .filter_map(|value| value.as_str().map(|value| value.to_string()))
                .collect(),
        ),
        _ => None,
    }
}

fn nested_value(settings: &Value, key: &str, nested_key: &str) -> Option<Value> {
    let Value::Map(entries) = get_field(settings, key)? else {
        return None;
    };
    entries.iter().find(|(k, _)| k == nested_key).map(|(_, v)| v.clone())
}

fn nested_number(settings: &Value, key: &str, nested_key: &str) -> Option<f64> {
    nested_value(settings, key, nested_key).and_then(|value| value.as_number())
}

fn nested_bool(settings: &Value, key: &str, nested_key: &str) -> Option<bool> {
    let Value::Map(entries) = get_field(settings, key)? else {
        return None;
    };
    let entry = entries.iter().find(|(k, _)| k == nested_key)?;
    entry.1.as_bool()
}

fn get_number(settings: &Value, key: &str) -> Option<f64> {
    get_field(settings, key).and_then(|value| value.as_number())
}

fn get_string(settings: &Value, key: &str) -> Option<String> {
    get_field(settings, key).and_then(|value| value.as_str().map(|value| value.to_string()))
}

/// Migrate old settings formats (JS `migrateSettings`).
pub fn migrate_settings(settings: &mut Value) {
    if !matches!(settings, Value::Map(_)) {
        return;
    }
    // queueMode -> steeringMode
    if get_field(settings, "queueMode").is_some() && get_field(settings, "steeringMode").is_none() {
        if let Some(queue_mode) = get_field(settings, "queueMode") {
            set_field(settings, "steeringMode", queue_mode);
            remove_field(settings, "queueMode");
        }
    }
    // websockets boolean -> transport
    if get_field(settings, "transport").is_none() {
        if let Some(Value::Bool(websockets)) = get_field(settings, "websockets") {
            set_field(
                settings,
                "transport",
                Value::String(if websockets { "websocket" } else { "sse" }.to_string()),
            );
            remove_field(settings, "websockets");
        }
    }
    // skills object -> array format
    if let Some(Value::Map(skills)) = get_field(settings, "skills") {
        let skills = skills.clone();
        let enable_skill_commands = skills.iter().find(|(key, _)| key == "enableSkillCommands").map(|(_, value)| value.clone());
        let custom_directories = skills
            .iter()
            .find(|(key, _)| key == "customDirectories")
            .and_then(|(_, value)| value.as_array())
            .map(|array| array.to_vec());
        if let Some(enable_skill_commands) = enable_skill_commands {
            if get_field(settings, "enableSkillCommands").is_none() {
                set_field(settings, "enableSkillCommands", enable_skill_commands);
            }
        }
        match custom_directories {
            Some(directories) if !directories.is_empty() => {
                set_field(settings, "skills", Value::Array(directories));
            }
            _ => {
                remove_field(settings, "skills");
            }
        }
    }
    // retry.maxDelayMs -> retry.provider.maxRetryDelayMs
    if let Some(Value::Map(mut retry)) = get_field(settings, "retry") {
        let max_delay = retry
            .iter()
            .find(|(key, _)| key == "maxDelayMs")
            .and_then(|(_, value)| value.as_number());
        let provider_exists = retry.iter().any(|(key, _)| key == "provider");
        let mut provider = if provider_exists {
            retry
                .iter()
                .find(|(key, _)| key == "provider")
                .and_then(|(_, value)| value.as_map())
                .map(|map| map.to_vec())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let provider_has_max_delay = provider.iter().any(|(key, _)| key == "maxRetryDelayMs");
        if let Some(max_delay) = max_delay {
            if !provider_has_max_delay {
                if !provider_exists {
                    retry.push(("provider".to_string(), Value::Map(Vec::new())));
                }
                provider = retry
                    .iter()
                    .find(|(key, _)| key == "provider")
                    .and_then(|(_, value)| value.as_map())
                    .map(|map| map.to_vec())
                    .unwrap_or_default();
                if !provider.iter().any(|(key, _)| key == "maxRetryDelayMs") {
                    provider.push(("maxRetryDelayMs".to_string(), Value::Number(max_delay)));
                }
            }
        }
        retry.retain(|(key, _)| key != "maxDelayMs");
        if provider_exists || max_delay.is_some() {
            if let Some(entry) = retry.iter_mut().find(|(key, _)| key == "provider") {
                entry.1 = Value::Map(provider);
            } else if max_delay.is_some() {
                retry.push(("provider".to_string(), Value::Map(provider)));
            }
        }
        set_field(settings, "retry", Value::Map(retry));
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SettingsError {
    pub scope: SettingsScope,
    pub message: String,
}

pub struct SettingsManager {
    storage: Box<dyn SettingsStorage>,
    global_settings: Value,
    project_settings: Value,
    settings: Value,
    project_trusted: bool,
    modified_fields: std::collections::HashSet<String>,
    modified_project_fields: std::collections::HashSet<String>,
    global_load_error: Option<String>,
    project_load_error: Option<String>,
    errors: Vec<SettingsError>,
}

impl SettingsManager {
    fn new(
        storage: Box<dyn SettingsStorage>,
        initial_global: Value,
        initial_project: Value,
        global_load_error: Option<String>,
        project_load_error: Option<String>,
        initial_errors: Vec<SettingsError>,
        project_trusted: bool,
    ) -> Self {
        let settings = deep_merge(&initial_global, &initial_project);
        Self {
            storage,
            global_settings: initial_global,
            project_settings: initial_project,
            settings,
            project_trusted,
            modified_fields: std::collections::HashSet::new(),
            modified_project_fields: std::collections::HashSet::new(),
            global_load_error,
            project_load_error,
            errors: initial_errors,
        }
    }

    fn load_from_storage(storage: &dyn SettingsStorage, scope: SettingsScope, project_trusted: bool) -> Value {
        if scope == SettingsScope::Project && !project_trusted {
            return Value::Map(Vec::new());
        }
        let mut content: Option<String> = None;
        storage.with_lock(scope, Box::new(|current| {
            content = current.map(|value| value.to_string());
            None
        }));
        let Some(content) = content else {
            return Value::Map(Vec::new());
        };
        let mut settings = pi_ai::utils::json::parse_json_with_repair::<Value>(&content).unwrap_or(Value::Map(Vec::new()));
        migrate_settings(&mut settings);
        settings
    }

    fn try_load_from_storage(
        storage: &dyn SettingsStorage,
        scope: SettingsScope,
        project_trusted: bool,
    ) -> (Value, Option<String>) {
        let settings = Self::load_from_storage(storage, scope, project_trusted);
        (settings, None)
    }

    /// Create a manager loading from files (JS `create`).
    pub fn create(cwd: &str, agent_dir: &str, project_trusted: bool) -> Self {
        let storage = FileSettingsStorage::new(cwd, agent_dir);
        Self::from_storage(Box::new(storage), project_trusted)
    }

    /// Create a manager from an arbitrary storage backend (JS
    /// `fromStorage`).
    pub fn from_storage(storage: Box<dyn SettingsStorage>, project_trusted: bool) -> Self {
        let (global_settings, global_error) = Self::try_load_from_storage(storage.as_ref(), SettingsScope::Global, true);
        let (project_settings, project_error) =
            Self::try_load_from_storage(storage.as_ref(), SettingsScope::Project, project_trusted);
        let mut initial_errors: Vec<SettingsError> = Vec::new();
        if let Some(error) = &global_error {
            initial_errors.push(SettingsError {
                scope: SettingsScope::Global,
                message: error.clone(),
            });
        }
        if let Some(error) = &project_error {
            initial_errors.push(SettingsError {
                scope: SettingsScope::Project,
                message: error.clone(),
            });
        }
        Self::new(
            storage,
            global_settings,
            project_settings,
            global_error,
            project_error,
            initial_errors,
            project_trusted,
        )
    }

    /// Create an in-memory manager (JS `inMemory`).
    pub fn in_memory(settings: Value) -> Self {
        let storage = InMemorySettingsStorage::new();
        let mut initial = settings;
        migrate_settings(&mut initial);
        storage.with_lock(SettingsScope::Global, Box::new(|_| Some(pi_ai::utils::json::json_stringify(&initial))));
        Self::from_storage(Box::new(storage), true)
    }

    pub fn get_global_settings(&self) -> Value {
        self.global_settings.clone()
    }

    pub fn get_project_settings(&self) -> Value {
        self.project_settings.clone()
    }

    pub fn is_project_trusted(&self) -> bool {
        self.project_trusted
    }

    pub fn set_project_trusted(&mut self, trusted: bool) {
        if self.project_trusted == trusted {
            return;
        }
        self.project_trusted = trusted;
        self.modified_project_fields.clear();
        if !trusted {
            self.project_settings = Value::Map(Vec::new());
            self.project_load_error = None;
            self.settings = deep_merge(&self.global_settings, &self.project_settings);
            return;
        }
        let (project_settings, project_error) =
            Self::try_load_from_storage(self.storage.as_ref(), SettingsScope::Project, trusted);
        self.project_settings = project_settings;
        self.project_load_error = project_error;
        self.settings = deep_merge(&self.global_settings, &self.project_settings);
    }

    pub fn reload(&mut self) {
        let (global_settings, global_error) = Self::try_load_from_storage(self.storage.as_ref(), SettingsScope::Global, true);
        if global_error.is_none() {
            self.global_settings = global_settings;
            self.global_load_error = None;
        } else {
            self.global_load_error = global_error;
        }
        self.modified_fields.clear();
        self.modified_project_fields.clear();
        let (project_settings, project_error) =
            Self::try_load_from_storage(self.storage.as_ref(), SettingsScope::Project, self.project_trusted);
        if project_error.is_none() {
            self.project_settings = project_settings;
            self.project_load_error = None;
        } else {
            self.project_load_error = project_error;
        }
        self.settings = deep_merge(&self.global_settings, &self.project_settings);
    }

    /// Apply additional overrides on top of current settings (JS
    /// `applyOverrides`).
    pub fn apply_overrides(&mut self, overrides: Value) {
        self.settings = deep_merge(&self.settings, &overrides);
    }

    fn mark_modified(&mut self, field: &str) {
        self.modified_fields.insert(field.to_string());
    }

    fn mark_project_modified(&mut self, field: &str) {
        self.modified_project_fields.insert(field.to_string());
    }

    fn assert_project_trusted_for_write(&self) -> Result<(), String> {
        if !self.project_trusted {
            Err("Project is not trusted; refusing to write project settings".to_string())
        } else {
            Ok(())
        }
    }

    fn persist_scoped_settings(
        storage: &dyn SettingsStorage,
        scope: SettingsScope,
        snapshot: &Value,
        modified_fields: &std::collections::HashSet<String>,
    ) {
        storage.with_lock(scope, Box::new(|current| {
            let mut current_file_settings =
                current
                    .map(|content| pi_ai::utils::json::parse_json_with_repair::<Value>(content).unwrap_or(Value::Map(Vec::new())))
                    .unwrap_or(Value::Map(Vec::new()));
            migrate_settings(&mut current_file_settings);
            for field in modified_fields {
                if let Some(value) = get_field(snapshot, field) {
                    set_field(&mut current_file_settings, field, value);
                }
            }
            Some(pi_ai::utils::json::json_stringify(&current_file_settings))
        }));
    }

    fn save(&mut self) {
        self.settings = deep_merge(&self.global_settings, &self.project_settings);
        if self.global_load_error.is_some() {
            return;
        }
        let snapshot = self.global_settings.clone();
        let modified = self.modified_fields.clone();
        self.modified_fields.clear();
        Self::persist_scoped_settings(self.storage.as_ref(), SettingsScope::Global, &snapshot, &modified);
    }

    fn save_project_settings(&mut self, settings: Value) {
        if let Err(error) = self.assert_project_trusted_for_write() {
            self.errors.push(SettingsError {
                scope: SettingsScope::Project,
                message: error,
            });
            return;
        }
        self.project_settings = settings;
        self.settings = deep_merge(&self.global_settings, &self.project_settings);
        if self.project_load_error.is_some() {
            return;
        }
        let snapshot = self.project_settings.clone();
        let modified = self.modified_project_fields.clone();
        self.modified_project_fields.clear();
        Self::persist_scoped_settings(self.storage.as_ref(), SettingsScope::Project, &snapshot, &modified);
    }

    fn update_project_settings(&mut self, field: &str, update: impl FnOnce(&mut Value)) {
        let mut project_settings = self.project_settings.clone();
        update(&mut project_settings);
        self.mark_project_modified(field);
        self.save_project_settings(project_settings);
    }

    pub fn drain_errors(&mut self) -> Vec<SettingsError> {
        std::mem::take(&mut self.errors)
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        get_field(&self.settings, key)
    }

    /// Set a global field and persist (JS set* methods).
    pub fn set_global(&mut self, key: &str, value: Value) {
        set_field(&mut self.global_settings, key, value);
        self.mark_modified(key);
        self.save();
    }

    /// Set a nested global field and persist.
    pub fn set_global_nested(&mut self, key: &str, nested_key: &str, value: Value) {
        let mut nested = match get_field(&self.global_settings, key) {
            Some(Value::Map(map)) => map,
            _ => Vec::new(),
        };
        if let Some(entry) = nested.iter_mut().find(|(k, _)| k == nested_key) {
            entry.1 = value;
        } else {
            nested.push((nested_key.to_string(), value));
        }
        set_field(&mut self.global_settings, key, Value::Map(nested));
        self.mark_modified(key);
        self.save();
    }

    /// Set a project field and persist.
    pub fn set_project(&mut self, key: &str, value: Value) {
        let key_owned = key.to_string();
        self.update_project_settings(&key_owned, |settings| {
            set_field(settings, &key_owned, value);
        });
    }

    pub fn get_string(&self, key: &str) -> Option<String> {
        get_string(&self.settings, key)
    }

    pub fn get_number(&self, key: &str) -> Option<f64> {
        get_number(&self.settings, key)
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        get_field(&self.settings, key)
            .and_then(|value| value.as_bool())
            .unwrap_or(default)
    }

    // ------------------------------------------------------------------
    // Typed getters (JS parity)
    // ------------------------------------------------------------------

    pub fn get_last_changelog_version(&self) -> Option<String> {
        self.get_string("lastChangelogVersion")
    }

    pub fn set_last_changelog_version(&mut self, version: &str) {
        self.set_global("lastChangelogVersion", Value::String(version.to_string()));
    }

    pub fn get_default_provider(&self) -> Option<String> {
        self.get_string("defaultProvider")
    }

    pub fn get_default_model(&self) -> Option<String> {
        self.get_string("defaultModel")
    }

    pub fn set_default_provider(&mut self, provider: &str) {
        self.set_global("defaultProvider", Value::String(provider.to_string()));
    }

    pub fn set_default_model(&mut self, model_id: &str) {
        self.set_global("defaultModel", Value::String(model_id.to_string()));
    }

    pub fn get_steering_mode(&self) -> String {
        self.get_string("steeringMode").unwrap_or_else(|| "one-at-a-time".to_string())
    }

    pub fn set_steering_mode(&mut self, mode: &str) {
        self.set_global("steeringMode", Value::String(mode.to_string()));
    }

    pub fn get_follow_up_mode(&self) -> String {
        self.get_string("followUpMode").unwrap_or_else(|| "one-at-a-time".to_string())
    }

    pub fn set_follow_up_mode(&mut self, mode: &str) {
        self.set_global("followUpMode", Value::String(mode.to_string()));
    }

    pub fn get_theme(&self) -> Option<String> {
        let theme = self.get_string("theme")?;
        if theme.contains('/') {
            None
        } else {
            Some(theme)
        }
    }

    pub fn set_theme(&mut self, theme: &str) {
        self.set_global("theme", Value::String(theme.to_string()));
    }

    pub fn get_default_thinking_level(&self) -> Option<String> {
        self.get_string("defaultThinkingLevel")
    }

    pub fn set_default_thinking_level(&mut self, level: &str) {
        self.set_global("defaultThinkingLevel", Value::String(level.to_string()));
    }

    pub fn get_transport(&self) -> String {
        self.get_string("transport").unwrap_or_else(|| "auto".to_string())
    }

    pub fn set_transport(&mut self, transport: &str) {
        self.set_global("transport", Value::String(transport.to_string()));
    }

    pub fn get_compaction_enabled(&self) -> bool {
        nested_bool(&self.settings, "compaction", "enabled").unwrap_or(true)
    }

    pub fn set_compaction_enabled(&mut self, enabled: bool) {
        self.set_global_nested("compaction", "enabled", Value::Bool(enabled));
    }

    pub fn get_compaction_reserve_tokens(&self) -> f64 {
        nested_number(&self.settings, "compaction", "reserveTokens")
            .unwrap_or(16384.0)
    }

    pub fn get_compaction_keep_recent_tokens(&self) -> f64 {
        nested_number(&self.settings, "compaction", "keepRecentTokens")
            .unwrap_or(20000.0)
    }

    pub fn get_branch_summary_reserve_tokens(&self) -> f64 {
        nested_number(&self.settings, "branchSummary", "reserveTokens")
            .unwrap_or(16384.0)
    }

    pub fn get_branch_summary_skip_prompt(&self) -> bool {
        nested_bool(&self.settings, "branchSummary", "skipPrompt").unwrap_or(false)
    }

    pub fn get_retry_enabled(&self) -> bool {
        nested_bool(&self.settings, "retry", "enabled").unwrap_or(true)
    }

    pub fn set_retry_enabled(&mut self, enabled: bool) {
        self.set_global_nested("retry", "enabled", Value::Bool(enabled));
    }

    pub fn get_retry_max_retries(&self) -> f64 {
        nested_number(&self.settings, "retry", "maxRetries")
            .unwrap_or(3.0)
    }

    pub fn get_retry_base_delay_ms(&self) -> f64 {
        nested_number(&self.settings, "retry", "baseDelayMs")
            .unwrap_or(2000.0)
    }

    pub fn get_provider_max_retry_delay_ms(&self) -> f64 {
        nested_number(&self.settings, "retry", "provider")
            .and_then(|_| {
                nested_value(&self.settings, "retry", "provider")
                    .and_then(|provider| provider.as_map().and_then(|entries| {
                        entries
                            .iter()
                            .find(|(k, _)| k == "maxRetryDelayMs")
                            .and_then(|(_, v)| v.as_number())
                    }))
            })
            .unwrap_or(60000.0)
    }

    pub fn get_hide_thinking_block(&self) -> bool {
        self.get_bool("hideThinkingBlock", false)
    }

    pub fn set_hide_thinking_block(&mut self, hide: bool) {
        self.set_global("hideThinkingBlock", Value::Bool(hide));
    }

    pub fn get_show_cache_miss_notices(&self) -> bool {
        self.get_bool("showCacheMissNotices", false)
    }

    pub fn set_show_cache_miss_notices(&mut self, show: bool) {
        self.set_global("showCacheMissNotices", Value::Bool(show));
    }

    pub fn get_external_editor_command(&self) -> String {
        self.get_string("externalEditor").unwrap_or_default()
    }

    pub fn set_external_editor(&mut self, command: &str) {
        self.set_global("externalEditor", Value::String(command.to_string()));
    }

    pub fn get_shell_path(&self) -> Option<String> {
        self.get_string("shellPath").map(|path| normalize_path(&path, &PathInputOptions::default()))
    }

    pub fn set_shell_path(&mut self, path: Option<&str>) {
        match path {
            Some(path) => self.set_global("shellPath", Value::String(path.to_string())),
            None => self.set_global("shellPath", Value::Null),
        }
    }

    pub fn set_quiet_startup(&mut self, quiet: bool) {
        self.set_global("quietStartup", Value::Bool(quiet));
    }

    pub fn get_default_project_trust(&self) -> String {
        self.get_string("defaultProjectTrust").unwrap_or_else(|| "ask".to_string())
    }

    pub fn set_default_project_trust(&mut self, trust: &str) {
        self.set_global("defaultProjectTrust", Value::String(trust.to_string()));
    }

    pub fn get_shell_command_prefix(&self) -> Option<String> {
        self.get_string("shellCommandPrefix")
    }

    pub fn set_shell_command_prefix(&mut self, prefix: &str) {
        self.set_global("shellCommandPrefix", Value::String(prefix.to_string()));
    }

    pub fn get_npm_command(&self) -> Option<Vec<String>> {
        string_array(&self.settings, "npmCommand")
    }

    pub fn set_collapse_changelog(&mut self, collapse: bool) {
        self.set_global("collapseChangelog", Value::Bool(collapse));
    }

    pub fn get_enable_install_telemetry(&self) -> bool {
        self.get_bool("enableInstallTelemetry", true)
    }

    pub fn set_enable_install_telemetry(&mut self, enabled: bool) {
        self.set_global("enableInstallTelemetry", Value::Bool(enabled));
    }

    pub fn set_enable_analytics(&mut self, enabled: bool) {
        self.set_global("enableAnalytics", Value::Bool(enabled));
    }

    pub fn set_tracking_id(&mut self, tracking_id: &str) {
        self.set_global("trackingId", Value::String(tracking_id.to_string()));
    }

    pub fn get_enable_skill_commands(&self) -> bool {
        self.get_bool("enableSkillCommands", true)
    }

    pub fn set_enable_skill_commands(&mut self, enabled: bool) {
        self.set_global("enableSkillCommands", Value::Bool(enabled));
    }

    pub fn get_session_dir(&self) -> Option<String> {
        self.get_string("sessionDir").map(|dir| normalize_path(&dir, &PathInputOptions::default()))
    }

    pub fn set_session_dir(&mut self, dir: Option<&str>) {
        match dir {
            Some(dir) => self.set_global("sessionDir", Value::String(dir.to_string())),
            None => self.set_global("sessionDir", Value::Null),
        }
    }

    pub fn get_packages(&self) -> Vec<Value> {
        match get_field(&self.settings, "packages") {
            Some(Value::Array(array)) => array,
            _ => Vec::new(),
        }
    }

    pub fn set_packages(&mut self, packages: Vec<Value>) {
        self.set_global("packages", Value::Array(packages));
    }

    pub fn get_extensions(&self) -> Vec<String> {
        string_array(&self.settings, "extensions")
            .unwrap_or_default()
    }

    pub fn set_extensions(&mut self, extensions: Vec<String>) {
        self.set_global(
            "extensions",
            Value::Array(extensions.into_iter().map(Value::String).collect()),
        );
    }

    pub fn get_skills(&self) -> Vec<String> {
        string_array(&self.settings, "skills")
            .unwrap_or_default()
    }

    pub fn set_skills(&mut self, skills: Vec<String>) {
        self.set_global("skills", Value::Array(skills.into_iter().map(Value::String).collect()));
    }

    pub fn get_prompts(&self) -> Vec<String> {
        string_array(&self.settings, "prompts")
            .unwrap_or_default()
    }

    pub fn set_prompts(&mut self, prompts: Vec<String>) {
        self.set_global("prompts", Value::Array(prompts.into_iter().map(Value::String).collect()));
    }

    pub fn get_themes(&self) -> Vec<String> {
        string_array(&self.settings, "themes")
            .unwrap_or_default()
    }

    pub fn set_themes(&mut self, themes: Vec<String>) {
        self.set_global("themes", Value::Array(themes.into_iter().map(Value::String).collect()));
    }

    pub fn get_double_escape_action(&self) -> String {
        self.get_string("doubleEscapeAction").unwrap_or_else(|| "tree".to_string())
    }

    pub fn set_double_escape_action(&mut self, action: &str) {
        self.set_global("doubleEscapeAction", Value::String(action.to_string()));
    }

    pub fn get_tree_filter_mode(&self) -> String {
        self.get_string("treeFilterMode").unwrap_or_else(|| "default".to_string())
    }

    pub fn set_tree_filter_mode(&mut self, mode: &str) {
        self.set_global("treeFilterMode", Value::String(mode.to_string()));
    }

    pub fn get_tui_mode(&self) -> String {
        self.get_string("tuiMode").unwrap_or_else(|| "regular".to_string())
    }

    pub fn set_tui_mode(&mut self, mode: &str) {
        self.set_global("tuiMode", Value::String(mode.to_string()));
    }

    pub fn get_fullscreen_exit_output(&self) -> String {
        self.get_string("fullscreenExitOutput").unwrap_or_else(|| "transcript".to_string())
    }

    pub fn set_fullscreen_exit_output(&mut self, output: &str) {
        self.set_global("fullscreenExitOutput", Value::String(output.to_string()));
    }

    pub fn get_editor_padding_x(&self) -> f64 {
        self.get_number("editorPaddingX").unwrap_or(0.0)
    }

    pub fn set_editor_padding_x(&mut self, padding: f64) {
        self.set_global("editorPaddingX", Value::Number(padding));
    }

    pub fn get_output_pad(&self) -> f64 {
        self.get_number("outputPad").unwrap_or(1.0)
    }

    pub fn set_output_pad(&mut self, pad: f64) {
        self.set_global("outputPad", Value::Number(pad));
    }

    pub fn get_autocomplete_max_visible(&self) -> f64 {
        self.get_number("autocompleteMaxVisible").unwrap_or(5.0)
    }

    pub fn set_autocomplete_max_visible(&mut self, max: f64) {
        self.set_global("autocompleteMaxVisible", Value::Number(max));
    }

    pub fn get_show_hardware_cursor(&self) -> bool {
        self.get_bool("showHardwareCursor", false)
    }

    pub fn set_show_hardware_cursor(&mut self, show: bool) {
        self.set_global("showHardwareCursor", Value::Bool(show));
    }

    pub fn get_http_proxy(&self) -> Option<String> {
        self.get_string("httpProxy")
    }

    pub fn set_http_proxy(&mut self, proxy: Option<&str>) {
        match proxy {
            Some(proxy) => self.set_global("httpProxy", Value::String(proxy.to_string())),
            None => self.set_global("httpProxy", Value::Null),
        }
    }

    pub fn get_http_idle_timeout_ms(&self) -> f64 {
        self.get_number("httpIdleTimeoutMs").unwrap_or(0.0)
    }

    pub fn set_http_idle_timeout_ms(&mut self, timeout_ms: f64) {
        self.set_global("httpIdleTimeoutMs", Value::Number(timeout_ms));
    }

    pub fn get_websocket_connect_timeout_ms(&self) -> Option<f64> {
        self.get_number("websocketConnectTimeoutMs")
    }

    pub fn set_websocket_connect_timeout_ms(&mut self, timeout_ms: f64) {
        self.set_global("websocketConnectTimeoutMs", Value::Number(timeout_ms));
    }

    pub fn get_warning(&self, key: &str) -> bool {
        nested_bool(&self.settings, "warnings", key).unwrap_or(true)
    }

    pub fn set_warning(&mut self, key: &str, value: bool) {
        self.set_global_nested("warnings", key, Value::Bool(value));
    }

    pub fn get_terminal(&self, key: &str) -> Option<Value> {
        nested_value(&self.settings, "terminal", key)
    }

    pub fn set_terminal(&mut self, key: &str, value: Value) {
        self.set_global_nested("terminal", key, value);
    }

    pub fn get_images(&self, key: &str) -> Option<Value> {
        nested_value(&self.settings, "images", key)
    }

    pub fn set_images(&mut self, key: &str, value: Value) {
        self.set_global_nested("images", key, value);
    }

    pub fn get_thinking_budgets(&self, level: &str) -> Option<f64> {
        nested_number(&self.settings, "thinkingBudgets", level)
    }

    pub fn set_thinking_budgets(&mut self, level: &str, value: f64) {
        self.set_global_nested("thinkingBudgets", level, Value::Number(value));
    }
}

/// Test convenience: agent dir for settings.
pub fn default_agent_dir() -> String {
    get_agent_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value_of(json: &str) -> Value {
        pi_ai::utils::json::parse_json_with_repair::<Value>(json).unwrap()
    }

    #[test]
    fn migrates_legacy_settings() {
        let mut settings = value_of(r#"{"queueMode": "all", "websockets": true, "skills": {"enableSkillCommands": false, "customDirectories": ["/x"]}}"#);
        migrate_settings(&mut settings);
        let entries = settings.as_map().unwrap();
        assert!(entries.iter().any(|(key, _)| key == "steeringMode"));
        assert!(!entries.iter().any(|(key, _)| key == "queueMode"));
        assert!(entries.iter().any(|(key, _)| key == "transport"));
        assert!(entries.iter().any(|(key, _)| key == "enableSkillCommands"));

        // retry.maxDelayMs migration.
        let mut settings = value_of(r#"{"retry": {"maxDelayMs": 5000}}"#);
        migrate_settings(&mut settings);
        let retry = get_field(&settings, "retry").unwrap();
        let retry_entries = retry.as_map().unwrap();
        assert!(!retry_entries.iter().any(|(key, _)| key == "maxDelayMs"));
        let provider = retry_entries.iter().find(|(key, _)| key == "provider").unwrap().1.as_map().unwrap();
        assert!(provider.iter().any(|(key, value)| key == "maxRetryDelayMs" && value.as_number() == Some(5000.0)));
    }

    #[test]
    fn in_memory_manager_roundtrip() {
        let mut manager = SettingsManager::in_memory(value_of(r#"{"defaultProvider": "openai"}"#));
        assert_eq!(manager.get_default_provider().as_deref(), Some("openai"));
        manager.set_default_model("gpt-4o");
        assert_eq!(manager.get_default_model().as_deref(), Some("gpt-4o"));
        manager.set_compaction_enabled(false);
        assert!(!manager.get_compaction_enabled());
        manager.set_theme("dark");
        assert_eq!(manager.get_theme().as_deref(), Some("dark"));
        manager.set_transport("sse");
        assert_eq!(manager.get_transport(), "sse");
    }

    #[test]
    fn deep_merge_nested_objects() {
        let base = value_of(r#"{"retry": {"enabled": true, "maxRetries": 3}}"#);
        let overrides = value_of(r#"{"retry": {"maxRetries": 5}}"#);
        let merged = deep_merge(&base, &overrides);
        let retry_value = get_field(&merged, "retry").unwrap();
        let retry = retry_value.as_map().unwrap();
        assert!(retry.iter().any(|(key, value)| key == "enabled" && value.as_bool() == Some(true)));
        assert!(retry.iter().any(|(key, value)| key == "maxRetries" && value.as_number() == Some(5.0)));
    }

    #[test]
    fn project_trust_gating() {
        let mut manager = SettingsManager::in_memory(value_of("{}"));
        manager.set_project_trusted(false);
        assert!(!manager.is_project_trusted());
        // Writes to project settings are refused.
        manager.set_project("theme", Value::String("x".to_string()));
        assert!(manager.drain_errors().len() > 0);
    }

    #[test]
    fn defaults_match_js() {
        let manager = SettingsManager::in_memory(value_of("{}"));
        assert!(manager.get_compaction_enabled());
        assert_eq!(manager.get_compaction_reserve_tokens(), 16384.0);
        assert_eq!(manager.get_compaction_keep_recent_tokens(), 20000.0);
        assert!(manager.get_retry_enabled());
        assert_eq!(manager.get_retry_max_retries(), 3.0);
        assert_eq!(manager.get_retry_base_delay_ms(), 2000.0);
        assert_eq!(manager.get_provider_max_retry_delay_ms(), 60000.0);
        assert_eq!(manager.get_steering_mode(), "one-at-a-time");
        assert_eq!(manager.get_transport(), "auto");
        assert_eq!(manager.get_default_project_trust(), "ask");
        assert_eq!(manager.get_tui_mode(), "regular");
        assert_eq!(manager.get_editor_padding_x(), 0.0);
        assert_eq!(manager.get_output_pad(), 1.0);
        assert_eq!(manager.get_autocomplete_max_visible(), 5.0);
        assert!(manager.get_enable_skill_commands());
        assert!(manager.get_enable_install_telemetry());
        assert!(manager.get_warning("anthropicExtraUsage"));
    }
}
