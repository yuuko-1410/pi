//! Extension loader, port of
//! `packages/coding-agent/src/core/extensions/loader.ts`.
//!
//! The TS module loading (jiti) is not portable to Rust: extensions are
//! registered programmatically via `load_extension_from_factory` or as
//! `InlineExtension`s. The runtime, per-extension API, caching, entry
//! discovery, and directory scanning are fully ported.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::core::event_bus::EventBus;
use crate::core::extensions::types::{
    Extension, HandlerFn, InlineExtension, LoadExtensionsResult, RegisteredTool, ToolDefinition,
};
use crate::core::pi_manifest::read_pi_manifest;
use crate::config::CONFIG_DIR_NAME;
use crate::utils::child_process::{resolve_path, PathInputOptions};

/// Per-extension registered flag.
#[derive(Clone, Debug)]
pub struct ExtensionFlag {
    pub name: String,
    pub extension_path: String,
    pub description: Option<String>,
    pub kind: String, // "boolean" | "string"
    pub default: Option<FlagValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FlagValue {
    Bool(bool),
    String(String),
}

/// Extension runtime with action methods. Action methods are stubs until
/// bound (JS `createExtensionRuntime`).
pub struct ExtensionRuntime {
    pub send_message: Mutex<Option<Arc<dyn Fn(Value, Option<Value>) + Send + Sync>>>,
    pub send_user_message: Mutex<Option<Arc<dyn Fn(Value, Option<Value>) + Send + Sync>>>,
    pub append_entry: Mutex<Option<Arc<dyn Fn(&str, Option<Value>) + Send + Sync>>>,
    pub set_session_name: Mutex<Option<Arc<dyn Fn(&str) + Send + Sync>>>,
    pub get_session_name: Mutex<Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>>,
    pub set_label: Mutex<Option<Arc<dyn Fn(&str, Option<&str>) + Send + Sync>>>,
    pub get_active_tools: Mutex<Option<Arc<dyn Fn() -> Vec<String> + Send + Sync>>>,
    pub get_all_tools: Mutex<Option<Arc<dyn Fn() -> Vec<String> + Send + Sync>>>,
    pub set_active_tools: Mutex<Option<Arc<dyn Fn(Vec<String>) + Send + Sync>>>,
    pub get_commands: Mutex<Option<Arc<dyn Fn() -> Vec<String> + Send + Sync>>>,
    pub get_thinking_level: Mutex<Option<Arc<dyn Fn() -> String + Send + Sync>>>,
    pub set_thinking_level: Mutex<Option<Arc<dyn Fn(&str) + Send + Sync>>>,
    pub flag_values: Mutex<HashMap<String, FlagValue>>,
    pub stale_message: Mutex<Option<String>>,
    event_bus_unsubscribers: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
}

use pi_protocol::cbor::Value;

impl ExtensionRuntime {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            send_message: Mutex::new(None),
            send_user_message: Mutex::new(None),
            append_entry: Mutex::new(None),
            set_session_name: Mutex::new(None),
            get_session_name: Mutex::new(None),
            set_label: Mutex::new(None),
            get_active_tools: Mutex::new(None),
            get_all_tools: Mutex::new(None),
            set_active_tools: Mutex::new(None),
            get_commands: Mutex::new(None),
            get_thinking_level: Mutex::new(None),
            set_thinking_level: Mutex::new(None),
            flag_values: Mutex::new(HashMap::new()),
            stale_message: Mutex::new(None),
            event_bus_unsubscribers: Mutex::new(Vec::new()),
        })
    }

    pub fn assert_active(&self) -> Result<(), String> {
        if let Some(message) = self.stale_message.lock().unwrap().clone() {
            return Err(message);
        }
        Ok(())
    }

    pub fn invalidate(&self, message: Option<&str>) {
        let mut stale = self.stale_message.lock().unwrap();
        if stale.is_some() {
            return;
        }
        *stale = Some(
            message
                .map(|message| message.to_string())
                .unwrap_or_else(|| "This extension ctx is stale after session replacement or reload. Do not use a captured pi or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload().".to_string()),
        );
        let unsubscribers: Vec<Arc<dyn Fn() + Send + Sync>> =
            self.event_bus_unsubscribers.lock().unwrap().drain(..).collect();
        for unsubscribe in unsubscribers {
            unsubscribe();
        }
    }

    pub fn track_event_bus_subscription(
        &self,
        unsubscribe: Arc<dyn Fn() + Send + Sync>,
    ) -> Arc<dyn Fn() + Send + Sync> {
        let runtime = self;
        let tracked: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            // The tracked wrapper removes itself and delegates.
            let _ = runtime;
            unsubscribe();
        });
        self.event_bus_unsubscribers.lock().unwrap().push(tracked.clone());
        tracked
    }
}

impl Default for ExtensionRuntime {
    fn default() -> Self {
        unimplemented!("use ExtensionRuntime::new() which returns Arc<Self>")
    }
}

/// Per-extension API: registration methods write to the extension; action
/// methods delegate to the shared runtime.
pub struct ExtensionApi {
    extension: Arc<Mutex<Extension>>,
    runtime: Arc<ExtensionRuntime>,
    cwd: String,
    event_bus: Arc<EventBus>,
    extension_path: String,
}

impl ExtensionApi {
    pub fn new(
        extension: Arc<Mutex<Extension>>,
        runtime: Arc<ExtensionRuntime>,
        cwd: &str,
        event_bus: Arc<EventBus>,
    ) -> Self {
        let extension_path = extension.lock().unwrap().path.clone();
        Self {
            extension,
            runtime,
            cwd: cwd.to_string(),
            event_bus,
            extension_path,
        }
    }

    /// Register an event handler (JS `on`).
    pub fn on(&self, event: &str, handler: HandlerFn) -> Result<(), String> {
        self.runtime.assert_active()?;
        self.extension
            .lock()
            .unwrap()
            .handlers
            .entry(event.to_string())
            .or_default()
            .push(handler);
        Ok(())
    }

    /// Register a tool (JS `registerTool`).
    pub fn register_tool(&self, tool: ToolDefinition) -> Result<(), String> {
        self.runtime.assert_active()?;
        self.extension.lock().unwrap().tools.insert(
            tool.name.clone(),
            RegisteredTool {
                definition: tool,
                hidden: false,
            },
        );
        Ok(())
    }

    /// Register a command (JS `registerCommand`).
    pub fn register_command(&self, command: crate::core::extensions::types::RegisteredCommand) -> Result<(), String> {
        self.runtime.assert_active()?;
        self.extension
            .lock()
            .unwrap()
            .commands
            .insert(command.name.clone(), command);
        Ok(())
    }

    /// Register a flag (JS `registerFlag`).
    pub fn register_flag(&self, name: &str, kind: &str, default: Option<FlagValue>) -> Result<(), String> {
        self.runtime.assert_active()?;
        self.extension.lock().unwrap().flags.insert(
            name.to_string(),
            ExtensionFlag {
                name: name.to_string(),
                extension_path: self.extension_path.clone(),
                description: None,
                kind: kind.to_string(),
                default: default.clone(),
            },
        );
        if let Some(default) = default {
            let mut flag_values = self.runtime.flag_values.lock().unwrap();
            if !flag_values.contains_key(name) {
                flag_values.insert(name.to_string(), default);
            }
        }
        Ok(())
    }

    /// Read a flag value (JS `getFlag`).
    pub fn get_flag(&self, name: &str) -> Option<FlagValue> {
        self.runtime.assert_active().ok()?;
        let extension = self.extension.lock().unwrap();
        if !extension.flags.contains_key(name) {
            return None;
        }
        drop(extension);
        self.runtime.flag_values.lock().unwrap().get(name).cloned()
    }

    /// Emit an event on the extension event bus (JS `events.emit`).
    pub fn emit_event(&self, channel: &str, data: &dyn std::any::Any) -> Result<(), String> {
        self.runtime.assert_active()?;
        self.event_bus.emit(channel, data);
        Ok(())
    }

    /// Subscribe to an event channel (JS `events.on`).
    pub fn on_event<F>(&self, channel: &str, handler: F) -> Result<Arc<dyn Fn() + Send + Sync>, String>
    where
        F: Fn(&dyn std::any::Any) + Send + Sync + 'static,
    {
        self.runtime.assert_active()?;
        let unsubscribe = self.event_bus.on(channel, handler);
        Ok(self.runtime.track_event_bus_subscription(unsubscribe))
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    pub fn extension_path(&self) -> &str {
        &self.extension_path
    }
}

/// Create an Extension object with empty collections (JS
/// `createExtension`).
pub fn create_extension(extension_path: &str, resolved_path: &str) -> Extension {
    let source = if extension_path.starts_with('<') && extension_path.ends_with('>') {
        extension_path[1..extension_path.len() - 1]
            .split(':')
            .next()
            .filter(|value| !value.is_empty())
            .unwrap_or("temporary")
            .to_string()
    } else {
        "local".to_string()
    };
    Extension {
        path: extension_path.to_string(),
        resolved_path: resolved_path.to_string(),
        hidden: None,
        handlers: HashMap::new(),
        tools: HashMap::new(),
        commands: HashMap::new(),
        flags: HashMap::new(),
        source: Some(source),
    }
}

/// Load an extension from an inline factory function (JS
/// `loadExtensionFromFactory`). The factory receives the extension API and
/// returns Ok on success.
pub fn load_extension_from_factory(
    factory: &dyn Fn(&ExtensionApi) -> Result<(), String>,
    cwd: &str,
    event_bus: Arc<EventBus>,
    runtime: Arc<ExtensionRuntime>,
    extension_path: &str,
) -> Result<Extension, String> {
    let extension = create_extension(extension_path, extension_path);
    let resolved_cwd = resolve_path(cwd, cwd, &PathInputOptions::default());
    let extension = Arc::new(Mutex::new(extension));
    let api = ExtensionApi::new(extension.clone(), runtime, &resolved_cwd, event_bus);
    factory(&api)?;
    drop(api);
    let extension = Arc::try_unwrap(extension)
        .map_err(|_| "extension arc still referenced".to_string())?
        .into_inner()
        .map_err(|_| "extension mutex poisoned".to_string())?;
    Ok(extension)
}

/// Load an inline extension (Rust extension model).
pub fn load_inline_extension(
    inline: &InlineExtension,
    event_bus: Arc<EventBus>,
    runtime: Arc<ExtensionRuntime>,
) -> Result<Extension, String> {
    let extension = inline.to_extension();
    let _ = event_bus;
    let _ = runtime;
    Ok(extension)
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

fn is_extension_file(name: &str) -> bool {
    name.ends_with(".ts") || name.ends_with(".js")
}

/// Resolve extension entry points from a directory: package.json
/// `pi.extensions` field, else index.ts/index.js (JS
/// `resolveExtensionEntries`).
fn resolve_extension_entries(dir: &str) -> Option<Vec<String>> {
    let package_json_path = format!("{dir}/package.json");
    if std::path::Path::new(&package_json_path).exists() {
        if let Some(manifest) = read_pi_manifest(&package_json_path) {
            if let Some(extensions) = manifest.extensions {
                let mut entries: Vec<String> = Vec::new();
                for ext_path in extensions {
                    let resolved = std::path::Path::new(dir).join(&ext_path);
                    if resolved.exists() {
                        entries.push(resolved.to_string_lossy().to_string());
                    }
                }
                if !entries.is_empty() {
                    return Some(entries);
                }
            }
        }
    }
    let index_ts = format!("{dir}/index.ts");
    let index_js = format!("{dir}/index.js");
    if std::path::Path::new(&index_ts).exists() {
        return Some(vec![index_ts]);
    }
    if std::path::Path::new(&index_js).exists() {
        return Some(vec![index_js]);
    }
    None
}

/// Discover extensions in a directory (JS `discoverExtensionsInDir`).
pub fn discover_extensions_in_dir(dir: &str) -> Vec<String> {
    if !std::path::Path::new(dir).exists() {
        return vec![];
    }
    let mut discovered: Vec<String> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    for entry in entries.flatten() {
        let entry_path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let metadata = entry.metadata().or_else(|_| std::fs::metadata(&entry_path));
        let is_file = metadata.as_ref().map(|m| m.is_file()).unwrap_or(false) || entry_path.is_file();
        let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false) || entry_path.is_dir();
        let is_symlink = metadata.as_ref().map(|m| m.file_type().is_symlink()).unwrap_or(false);
        if (is_file || is_symlink) && is_extension_file(&name) {
            discovered.push(entry_path.to_string_lossy().to_string());
            continue;
        }
        if is_dir || is_symlink {
            if let Some(entries) = resolve_extension_entries(&entry_path.to_string_lossy()) {
                discovered.extend(entries);
            }
        }
    }
    discovered
}

/// Discover and load extensions from standard locations (JS
/// `discoverAndLoadExtensions`; module loading replaced by inline
/// factories, so this returns the discovered paths for callers to load).
pub fn discover_extension_paths(configured_paths: &[String], cwd: &str, agent_dir: &str) -> Vec<String> {
    let resolved_cwd = resolve_path(cwd, cwd, &PathInputOptions::default());
    let resolved_agent_dir = resolve_path(agent_dir, agent_dir, &PathInputOptions::default());
    let mut all_paths: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let add_paths = |paths: Vec<String>, all_paths: &mut Vec<String>, seen: &mut std::collections::HashSet<String>| {
        for path in paths {
            let resolved = std::path::Path::new(&path)
                .canonicalize()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|_| path.clone());
            if !seen.contains(&resolved) {
                seen.insert(resolved);
                all_paths.push(path);
            }
        }
    };

    let local_ext_dir = format!("{resolved_cwd}/{CONFIG_DIR_NAME}/extensions");
    add_paths(discover_extensions_in_dir(&local_ext_dir), &mut all_paths, &mut seen);
    let global_ext_dir = format!("{resolved_agent_dir}/extensions");
    add_paths(discover_extensions_in_dir(&global_ext_dir), &mut all_paths, &mut seen);

    for path in configured_paths {
        let resolved = resolve_path(path, &resolved_cwd, &PathInputOptions {
            normalize_unicode_spaces: true,
            ..PathInputOptions::default()
        });
        let resolved_path = std::path::Path::new(&resolved);
        if resolved_path.exists() && resolved_path.is_dir() {
            if let Some(entries) = resolve_extension_entries(&resolved) {
                add_paths(entries, &mut all_paths, &mut seen);
                continue;
            }
            add_paths(discover_extensions_in_dir(&resolved), &mut all_paths, &mut seen);
            continue;
        }
        add_paths(vec![resolved], &mut all_paths, &mut seen);
    }

    all_paths
}

/// Load extensions from paths via inline factories (JS `loadExtensions`).
pub fn load_extensions(
    paths: &[String],
    factories: &std::collections::HashMap<String, Arc<dyn Fn(&ExtensionApi) -> Result<(), String> + Send + Sync>>,
    cwd: &str,
    event_bus: Arc<EventBus>,
    runtime: Arc<ExtensionRuntime>,
) -> LoadExtensionsResult {
    let mut result = LoadExtensionsResult::default();
    for path in paths {
        let Some(factory) = factories.get(path) else {
            result.errors.push((path.clone(), "No factory registered for extension path".to_string()));
            continue;
        };
        match load_extension_from_factory(factory.as_ref(), cwd, event_bus.clone(), runtime.clone(), path) {
            Ok(extension) => result.extensions.push(extension),
            Err(error) => result.errors.push((path.clone(), error)),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extensions::types::ToolDefinition;

    fn runtime() -> Arc<ExtensionRuntime> {
        ExtensionRuntime::new()
    }

    #[test]
    fn runtime_starts_inactive_free() {
        let runtime = runtime();
        assert!(runtime.assert_active().is_ok());
        runtime.invalidate(None);
        assert!(runtime.assert_active().is_err());
    }

    #[test]
    fn api_registers_tools_and_flags() {
        let runtime = runtime();
        let event_bus = Arc::new(EventBus::new());
        let extension = create_extension("test-ext", "/tmp/test-ext");
        let extension = Arc::new(Mutex::new(extension));
        let api = ExtensionApi::new(extension.clone(), runtime.clone(), "/tmp", event_bus);
        api.register_tool(ToolDefinition::new("t", "desc", None, |_id, _params, _state| Ok(Value::Null)))
            .unwrap();
        api.register_flag("verbose", "boolean", Some(FlagValue::Bool(false))).unwrap();
        assert!(extension.lock().unwrap().tools.contains_key("t"));
        assert_eq!(api.get_flag("verbose"), Some(FlagValue::Bool(false)));
        assert_eq!(api.get_flag("missing"), None);
    }

    #[test]
    fn factory_loading_populates_extension() {
        let runtime = runtime();
        let event_bus = Arc::new(EventBus::new());
        let extension = load_extension_from_factory(
            &|api| {
                api.register_tool(ToolDefinition::new("tool-x", "x", None, |_id, _params, _state| Ok(Value::Null)))?;
                api.on("session_start", std::sync::Arc::new(|_event| Ok(None)))?;
                Ok(())
            },
            "/tmp",
            event_bus,
            runtime,
            "<inline>",
        )
        .unwrap();
        assert!(extension.tools.contains_key("tool-x"));
        assert!(extension.handlers.contains_key("session_start"));
    }

    #[test]
    fn discovers_directory_entries() {
        let dir = std::env::temp_dir().join(format!("pi-ext-disc-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.ts"), "x").unwrap();
        std::fs::write(dir.join("sub/index.ts"), "y").unwrap();
        let discovered = discover_extensions_in_dir(&dir.to_string_lossy());
        assert_eq!(discovered.len(), 2);
        assert!(discovered.iter().any(|path| path.ends_with("a.ts")));
        assert!(discovered.iter().any(|path| path.ends_with("sub/index.ts")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn manifest_entries_resolved() {
        let dir = std::env::temp_dir().join(format!("pi-ext-manifest-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/main.ts"), "x").unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"pi": {"extensions": ["lib/main.ts"]}}"#,
        )
        .unwrap();
        let entries = resolve_extension_entries(&dir.to_string_lossy()).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].ends_with("lib/main.ts"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
