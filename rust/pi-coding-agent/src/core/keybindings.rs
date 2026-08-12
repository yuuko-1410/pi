//! App keybindings, port of `core/keybindings.ts`. Extends the TUI default
//! keybindings with app-level actions and manages the user keybindings.json
//! config (including legacy name migration).

use std::collections::HashMap;
use std::fs;

use pi_tui::keybindings::{KeybindingDefinition, KeybindingsConfig, KeybindingsManager as TuiKeybindingsManager, Keys};

pub type Keybinding = String;
pub type KeyId = String;

/// App keybinding names (port of the AppKeybindings interface keys).
pub const APP_KEYBINDINGS: &[&str] = &[
    "app.interrupt",
    "app.clear",
    "app.exit",
    "app.suspend",
    "app.thinking.cycle",
    "app.model.cycleForward",
    "app.model.cycleBackward",
    "app.model.select",
    "app.tools.expand",
    "app.thinking.toggle",
    "app.session.toggleNamedFilter",
    "app.editor.external",
    "app.message.copy",
    "app.message.followUp",
    "app.message.dequeue",
    "app.clipboard.pasteImage",
    "app.session.new",
    "app.session.tree",
    "app.session.fork",
    "app.session.resume",
    "app.tree.foldOrUp",
    "app.tree.unfoldOrDown",
    "app.tree.editLabel",
    "app.tree.toggleLabelTimestamp",
    "app.session.togglePath",
    "app.session.toggleSort",
    "app.session.rename",
    "app.session.delete",
    "app.session.deleteNoninvasive",
    "app.models.save",
    "app.models.enableAll",
    "app.models.clearAll",
    "app.models.toggleProvider",
    "app.models.reorderUp",
    "app.models.reorderDown",
    "app.tree.filter.default",
    "app.tree.filter.noTools",
    "app.tree.filter.userOnly",
    "app.tree.filter.labeledOnly",
    "app.tree.filter.all",
    "app.tree.filter.cycleForward",
    "app.tree.filter.cycleBackward",
];

fn is_windows() -> bool {
    cfg!(windows)
}

fn is_darwin() -> bool {
    cfg!(target_os = "macos")
}

/// (name, default keys, description) triples for app keybindings, in the JS
/// spread order.
fn app_keybinding_definitions() -> Vec<(&'static str, Keys, &'static str)> {
    vec![
        ("app.interrupt", Keys::Single("escape".into()), "Cancel or abort"),
        ("app.clear", Keys::Single("ctrl+c".into()), "Clear editor"),
        ("app.exit", Keys::Single("ctrl+d".into()), "Exit when editor is empty"),
        (
            "app.suspend",
            if is_windows() {
                Keys::Multiple(vec![])
            } else {
                Keys::Single("ctrl+z".into())
            },
            "Suspend to background",
        ),
        ("app.thinking.cycle", Keys::Single("shift+tab".into()), "Cycle thinking level"),
        ("app.model.cycleForward", Keys::Single("ctrl+p".into()), "Cycle to next model"),
        ("app.model.cycleBackward", Keys::Single("shift+ctrl+p".into()), "Cycle to previous model"),
        ("app.model.select", Keys::Single("ctrl+l".into()), "Open model selector"),
        ("app.tools.expand", Keys::Single("ctrl+o".into()), "Toggle tool output"),
        ("app.thinking.toggle", Keys::Single("ctrl+t".into()), "Toggle thinking blocks"),
        ("app.session.toggleNamedFilter", Keys::Single("ctrl+n".into()), "Toggle named session filter"),
        ("app.editor.external", Keys::Single("ctrl+g".into()), "Open external editor"),
        ("app.message.copy", Keys::Single("ctrl+x".into()), "Copy message to clipboard"),
        ("app.message.followUp", Keys::Single("alt+enter".into()), "Queue follow-up message"),
        ("app.message.dequeue", Keys::Single("alt+up".into()), "Restore queued messages"),
        (
            "app.clipboard.pasteImage",
            if is_windows() {
                Keys::Single("alt+v".into())
            } else {
                Keys::Single("ctrl+v".into())
            },
            "Paste image from clipboard (text fallback)",
        ),
        ("app.session.new", Keys::Multiple(vec![]), "Start a new session"),
        ("app.session.tree", Keys::Multiple(vec![]), "Open session tree"),
        ("app.session.fork", Keys::Multiple(vec![]), "Fork current session"),
        ("app.session.resume", Keys::Multiple(vec![]), "Resume a session"),
        (
            "app.tree.foldOrUp",
            if is_darwin() {
                Keys::Multiple(vec!["alt+left".into(), "ctrl+left".into()])
            } else {
                Keys::Multiple(vec!["ctrl+left".into(), "alt+left".into()])
            },
            "Fold tree branch or move up",
        ),
        (
            "app.tree.unfoldOrDown",
            if is_darwin() {
                Keys::Multiple(vec!["alt+right".into(), "ctrl+right".into()])
            } else {
                Keys::Multiple(vec!["ctrl+right".into(), "alt+right".into()])
            },
            "Unfold tree branch or move down",
        ),
        ("app.tree.editLabel", Keys::Single("shift+l".into()), "Edit tree label"),
        ("app.tree.toggleLabelTimestamp", Keys::Single("shift+t".into()), "Toggle tree label timestamps"),
        ("app.session.togglePath", Keys::Single("ctrl+p".into()), "Toggle session path display"),
        ("app.session.toggleSort", Keys::Single("ctrl+s".into()), "Toggle session sort mode"),
        ("app.session.rename", Keys::Single("ctrl+r".into()), "Rename session"),
        ("app.session.delete", Keys::Single("ctrl+d".into()), "Delete session"),
        ("app.session.deleteNoninvasive", Keys::Single("ctrl+backspace".into()), "Delete session when query is empty"),
        ("app.models.save", Keys::Single("ctrl+s".into()), "Save model selection"),
        ("app.models.enableAll", Keys::Single("ctrl+a".into()), "Enable all models"),
        ("app.models.clearAll", Keys::Single("ctrl+x".into()), "Clear all models"),
        ("app.models.toggleProvider", Keys::Single("ctrl+p".into()), "Toggle all models for provider"),
        ("app.models.reorderUp", Keys::Single("alt+up".into()), "Move model up in order"),
        ("app.models.reorderDown", Keys::Single("alt+down".into()), "Move model down in order"),
        ("app.tree.filter.default", Keys::Single("ctrl+d".into()), "Tree filter: default view"),
        ("app.tree.filter.noTools", Keys::Single("ctrl+t".into()), "Tree filter: hide tool results"),
        ("app.tree.filter.userOnly", Keys::Single("ctrl+u".into()), "Tree filter: user messages only"),
        ("app.tree.filter.labeledOnly", Keys::Single("ctrl+l".into()), "Tree filter: labeled entries only"),
        ("app.tree.filter.all", Keys::Single("ctrl+a".into()), "Tree filter: show all entries"),
        ("app.tree.filter.cycleForward", Keys::Single("ctrl+o".into()), "Tree filter: cycle forward"),
        ("app.tree.filter.cycleBackward", Keys::Single("shift+ctrl+o".into()), "Tree filter: cycle backward"),
    ]
}

/// Legacy keybinding name migrations (pre-namespacing config keys).
const KEYBINDING_NAME_MIGRATIONS: &[(&str, &str)] = &[
    ("cursorUp", "tui.editor.cursorUp"),
    ("cursorDown", "tui.editor.cursorDown"),
    ("cursorLeft", "tui.editor.cursorLeft"),
    ("cursorRight", "tui.editor.cursorRight"),
    ("cursorWordLeft", "tui.editor.cursorWordLeft"),
    ("cursorWordRight", "tui.editor.cursorWordRight"),
    ("cursorLineStart", "tui.editor.cursorLineStart"),
    ("cursorLineEnd", "tui.editor.cursorLineEnd"),
    ("jumpForward", "tui.editor.jumpForward"),
    ("jumpBackward", "tui.editor.jumpBackward"),
    ("pageUp", "tui.editor.pageUp"),
    ("pageDown", "tui.editor.pageDown"),
    ("deleteCharBackward", "tui.editor.deleteCharBackward"),
    ("deleteCharForward", "tui.editor.deleteCharForward"),
    ("deleteWordBackward", "tui.editor.deleteWordBackward"),
    ("deleteWordForward", "tui.editor.deleteWordForward"),
    ("deleteToLineStart", "tui.editor.deleteToLineStart"),
    ("deleteToLineEnd", "tui.editor.deleteToLineEnd"),
    ("yank", "tui.editor.yank"),
    ("yankPop", "tui.editor.yankPop"),
    ("undo", "tui.editor.undo"),
    ("newLine", "tui.input.newLine"),
    ("submit", "tui.input.submit"),
    ("tab", "tui.input.tab"),
    ("copy", "tui.input.copy"),
    ("selectUp", "tui.select.up"),
    ("selectDown", "tui.select.down"),
    ("selectPageUp", "tui.select.pageUp"),
    ("selectPageDown", "tui.select.pageDown"),
    ("selectConfirm", "tui.select.confirm"),
    ("selectCancel", "tui.select.cancel"),
    ("interrupt", "app.interrupt"),
    ("clear", "app.clear"),
    ("exit", "app.exit"),
    ("suspend", "app.suspend"),
    ("cycleThinkingLevel", "app.thinking.cycle"),
    ("cycleModelForward", "app.model.cycleForward"),
    ("cycleModelBackward", "app.model.cycleBackward"),
    ("selectModel", "app.model.select"),
    ("expandTools", "app.tools.expand"),
    ("toggleThinking", "app.thinking.toggle"),
    ("toggleSessionNamedFilter", "app.session.toggleNamedFilter"),
    ("externalEditor", "app.editor.external"),
    ("followUp", "app.message.followUp"),
    ("dequeue", "app.message.dequeue"),
    ("pasteImage", "app.clipboard.pasteImage"),
    ("newSession", "app.session.new"),
    ("tree", "app.session.tree"),
    ("fork", "app.session.fork"),
    ("resume", "app.session.resume"),
    ("treeFoldOrUp", "app.tree.foldOrUp"),
    ("treeUnfoldOrDown", "app.tree.unfoldOrDown"),
    ("treeEditLabel", "app.tree.editLabel"),
    ("treeToggleLabelTimestamp", "app.tree.toggleLabelTimestamp"),
    ("toggleSessionPath", "app.session.togglePath"),
    ("toggleSessionSort", "app.session.toggleSort"),
    ("renameSession", "app.session.rename"),
    ("deleteSession", "app.session.delete"),
    ("deleteSessionNoninvasive", "app.session.deleteNoninvasive"),
];

fn is_legacy_keybinding_name(key: &str) -> Option<&'static str> {
    KEYBINDING_NAME_MIGRATIONS
        .iter()
        .find(|(legacy, _)| *legacy == key)
        .map(|(_, modern)| *modern)
}

fn to_keybindings_config(value: &HashMap<String, Value>) -> KeybindingsConfig {
    let mut config: KeybindingsConfig = HashMap::new();
    for (key, binding) in value {
        match binding {
            Value::String(key_id) => {
                config.insert(key.clone(), Some(Keys::Single(key_id.clone())));
            }
            Value::Array(keys) if keys.iter().all(|entry| matches!(entry, Value::String(_))) => {
                config.insert(
                    key.clone(),
                    Some(Keys::Multiple(
                        keys.iter()
                            .filter_map(|entry| entry.as_str().map(|value| value.to_string()))
                            .collect(),
                    )),
                );
            }
            _ => {}
        }
    }
    config
}

use pi_protocol::Value;

/// Migrate a raw keybindings config (legacy names -> namespaced names).
pub fn migrate_keybindings_config(
    raw_config: &HashMap<String, Value>,
) -> (HashMap<String, Value>, bool) {
    let mut config: HashMap<String, Value> = HashMap::new();
    let mut migrated = false;

    for (key, value) in raw_config {
        let next_key = is_legacy_keybinding_name(key).unwrap_or(key).to_string();
        if next_key != *key {
            migrated = true;
        }
        if key != &next_key && raw_config.contains_key(&next_key) {
            migrated = true;
            continue;
        }
        config.insert(next_key, value.clone());
    }

    (order_keybindings_config(config), migrated)
}

fn order_keybindings_config(config: HashMap<String, Value>) -> HashMap<String, Value> {
    let mut ordered: Vec<(String, Value)> = Vec::new();
    // Definitions in spread order first (tui + app).
    let mut known: Vec<String> = Vec::new();
    known.extend(pi_tui::keybindings::tui_keybindings().keys().cloned());
    for (name, _, _) in app_keybinding_definitions() {
        known.push(name.to_string());
    }
    for name in &known {
        if let Some(value) = config.get(name) {
            ordered.push((name.clone(), value.clone()));
        }
    }
    let mut extras: Vec<String> = config
        .keys()
        .filter(|key| !known.contains(key))
        .cloned()
        .collect();
    extras.sort();
    for key in extras {
        if let Some(value) = config.get(&key) {
            ordered.push((key, value.clone()));
        }
    }
    ordered.into_iter().collect()
}

fn load_raw_config(path: &str) -> Option<HashMap<String, Value>> {
    let content = fs::read_to_string(path).ok()?;
    let parsed: Value = pi_ai::utils::json::parse_json_with_repair(&content).ok()?;
    match parsed {
        Value::Map(entries) => Some(entries.into_iter().collect()),
        _ => None,
    }
}

/// Combined keybinding manager (tui defaults + app keybindings + user config).
pub struct KeybindingsManager {
    inner: TuiKeybindingsManager,
    config_path: Option<String>,
}

impl KeybindingsManager {
    pub fn new(user_bindings: KeybindingsConfig, config_path: Option<String>) -> Self {
        let mut definitions = pi_tui::keybindings::tui_keybindings();
        for (name, keys, description) in app_keybinding_definitions() {
            definitions.insert(
                name.to_string(),
                KeybindingDefinition {
                    default_keys: keys,
                    description: Some(description.to_string()),
                },
            );
        }
        let mut inner = TuiKeybindingsManager::new(definitions);
        inner.set_user_bindings(user_bindings);
        Self { inner, config_path }
    }

    pub fn create(agent_dir: Option<String>) -> KeybindingsManager {
        let config_path = agent_dir.unwrap_or_else(crate::config::get_agent_dir) + "/keybindings.json";
        let user_bindings = Self::load_from_file(&config_path);
        Self::new(user_bindings, Some(config_path))
    }

    pub fn reload(&mut self) {
        if let Some(config_path) = &self.config_path {
            let user_bindings = Self::load_from_file(config_path);
            self.inner.set_user_bindings(user_bindings);
        }
    }

    pub fn get_effective_config(&self) -> KeybindingsConfig {
        self.inner.get_resolved_bindings()
    }

    pub fn get_keys(&self, keybinding: &str) -> Vec<String> {
        self.inner.get_keys(keybinding)
    }

    pub fn matches(&self, data: &str, keybinding: &str) -> bool {
        self.inner.matches(data, keybinding)
    }

    fn load_from_file(path: &str) -> KeybindingsConfig {
        match load_raw_config(path) {
            Some(raw_config) => {
                let (migrated, _) = migrate_keybindings_config(&raw_config);
                to_keybindings_config(&migrated)
            }
            None => KeybindingsConfig::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_definitions_cover_all_ids() {
        let names: Vec<&str> = app_keybinding_definitions().iter().map(|(name, _, _)| *name).collect();
        assert_eq!(names, APP_KEYBINDINGS.to_vec());
    }

    #[test]
    fn default_keys_match_js() {
        let definitions = app_keybinding_definitions();
        let interrupt = definitions.iter().find(|(name, _, _)| *name == "app.interrupt").unwrap();
        assert_eq!(interrupt.1, Keys::Single("escape".into()));
        let suspend = definitions.iter().find(|(name, _, _)| *name == "app.suspend").unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(suspend.1, Keys::Single("ctrl+z".into()));
        }
    }

    #[test]
    fn legacy_migration_renames() {
        let raw: HashMap<String, Value> = [
            ("interrupt".to_string(), Value::String("escape".into())),
            ("cursorUp".to_string(), Value::String("ctrl+p".into())),
        ]
        .into_iter()
        .collect();
        let (config, migrated) = migrate_keybindings_config(&raw);
        assert!(migrated);
        assert!(config.contains_key("app.interrupt"));
        assert!(config.contains_key("tui.editor.cursorUp"));
        assert!(!config.contains_key("interrupt"));
    }

    #[test]
    fn legacy_migration_skips_when_target_exists() {
        let raw: HashMap<String, Value> = [
            ("interrupt".to_string(), Value::String("escape".into())),
            ("app.interrupt".to_string(), Value::String("ctrl+x".into())),
        ]
        .into_iter()
        .collect();
        let (config, migrated) = migrate_keybindings_config(&raw);
        assert!(migrated);
        assert_eq!(config.get("app.interrupt"), Some(&Value::String("ctrl+x".into())));
    }

    #[test]
    fn manager_resolves_combined_defaults() {
        let manager = KeybindingsManager::new(KeybindingsConfig::new(), None);
        assert_eq!(manager.get_keys("app.interrupt"), vec!["escape".to_string()]);
        // TUI defaults are included.
        assert!(!manager.get_keys("tui.input.submit").is_empty());
        assert!(manager.matches("\u{1b}", "app.interrupt"));
    }

    #[test]
    fn user_bindings_override() {
        let user: KeybindingsConfig = [(
            "app.interrupt".to_string(),
            Some(Keys::Single("ctrl+k".into())),
        )]
        .into_iter()
        .collect();
        let manager = KeybindingsManager::new(user, None);
        assert_eq!(manager.get_keys("app.interrupt"), vec!["ctrl+k".to_string()]);
    }

    #[test]
    fn config_round_trip_from_file() {
        let counter = std::sync::atomic::AtomicU64::new(0);
        let n = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("pi-kb-{}-{n}.json", std::process::id()));
        std::fs::write(&path, r#"{"app.interrupt": "ctrl+k"}"#).unwrap();
        let config = KeybindingsManager::load_from_file(&path.to_string_lossy());
        assert_eq!(
            config.get("app.interrupt"),
            Some(&Some(Keys::Single("ctrl+k".into())))
        );
        let _ = std::fs::remove_file(&path);
    }
}
