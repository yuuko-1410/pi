//! Keybinding registry, port of `packages/tui/src/keybindings.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::keys::{matches_key, KeyId};

#[derive(Clone, Debug, PartialEq)]
pub struct KeybindingDefinition {
    pub default_keys: Keys,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Keys {
    Single(String),
    Multiple(Vec<String>),
}

impl Keys {
    pub fn as_vec(&self) -> Vec<String> {
        match self {
            Keys::Single(key) => vec![key.clone()],
            Keys::Multiple(keys) => keys.clone(),
        }
    }
}

pub type KeybindingsConfig = HashMap<String, Option<Keys>>;

#[derive(Clone, Debug, PartialEq)]
pub struct KeybindingConflict {
    pub key: String,
    pub keybindings: Vec<String>,
}

fn normalize_keys(keys: Option<&Keys>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result: Vec<String> = Vec::new();
    if let Some(keys) = keys {
        for key in keys.as_vec() {
            if seen.insert(key.clone()) {
                result.push(key);
            }
        }
    }
    result
}

pub struct KeybindingsManager {
    definitions: HashMap<String, KeybindingDefinition>,
    user_bindings: KeybindingsConfig,
    keys_by_id: HashMap<String, Vec<String>>,
    conflicts: Vec<KeybindingConflict>,
}

impl KeybindingsManager {
    pub fn new(definitions: HashMap<String, KeybindingDefinition>) -> Self {
        let mut manager = Self {
            definitions,
            user_bindings: HashMap::new(),
            keys_by_id: HashMap::new(),
            conflicts: Vec::new(),
        };
        manager.rebuild();
        manager
    }

    fn rebuild(&mut self) {
        self.keys_by_id.clear();
        self.conflicts.clear();

        let mut user_claims: HashMap<String, HashSet<String>> = HashMap::new();
        for (keybinding, keys) in &self.user_bindings {
            if !self.definitions.contains_key(keybinding) {
                continue;
            }
            for key in normalize_keys(keys.as_ref()) {
                user_claims.entry(key).or_default().insert(keybinding.clone());
            }
        }
        for (key, keybindings) in &user_claims {
            if keybindings.len() > 1 {
                self.conflicts.push(KeybindingConflict {
                    key: key.clone(),
                    keybindings: keybindings.iter().cloned().collect(),
                });
            }
        }

        for (id, definition) in &self.definitions {
            let keys = match self.user_bindings.get(id) {
                Some(Some(user_keys)) => normalize_keys(Some(user_keys)),
                Some(None) => Vec::new(),
                None => normalize_keys(Some(&definition.default_keys)),
            };
            self.keys_by_id.insert(id.clone(), keys);
        }
    }

    pub fn matches(&self, data: &str, keybinding: &str) -> bool {
        match self.keys_by_id.get(keybinding) {
            Some(keys) => keys.iter().any(|key| matches_key(data, &KeyId::from(key.clone()))),
            None => false,
        }
    }

    pub fn get_keys(&self, keybinding: &str) -> Vec<String> {
        self.keys_by_id.get(keybinding).cloned().unwrap_or_default()
    }

    pub fn get_definition(&self, keybinding: &str) -> Option<&KeybindingDefinition> {
        self.definitions.get(keybinding)
    }

    pub fn get_conflicts(&self) -> Vec<KeybindingConflict> {
        self.conflicts.clone()
    }

    pub fn set_user_bindings(&mut self, user_bindings: KeybindingsConfig) {
        self.user_bindings = user_bindings;
        self.rebuild();
    }

    pub fn get_user_bindings(&self) -> KeybindingsConfig {
        self.user_bindings.clone()
    }

    pub fn get_resolved_bindings(&self) -> KeybindingsConfig {
        let mut resolved: KeybindingsConfig = HashMap::new();
        for id in self.definitions.keys() {
            let keys = self.keys_by_id.get(id).cloned().unwrap_or_default();
            resolved.insert(
                id.clone(),
                Some(if keys.len() == 1 {
                    Keys::Single(keys[0].clone())
                } else {
                    Keys::Multiple(keys)
                }),
            );
        }
        resolved
    }
}

static GLOBAL_KEYBINDINGS: Mutex<Option<KeybindingsManager>> = Mutex::new(None);

pub fn set_keybindings(keybindings: KeybindingsManager) {
    *GLOBAL_KEYBINDINGS.lock().unwrap() = Some(keybindings);
}

pub fn get_keybindings() -> std::sync::MutexGuard<'static, Option<KeybindingsManager>> {
    GLOBAL_KEYBINDINGS.lock().unwrap()
}

/// The default TUI keybindings (full table from keybindings.ts).
pub fn tui_keybindings() -> HashMap<String, KeybindingDefinition> {
    let mut definitions = HashMap::new();
    let mut insert = |id: &str, keys: Keys, description: &str| {
        definitions.insert(
            id.to_string(),
            KeybindingDefinition {
                default_keys: keys,
                description: Some(description.to_string()),
            },
        );
    };
    insert("tui.editor.cursorUp", Keys::Single("up".to_string()), "Move cursor up");
    insert("tui.editor.cursorDown", Keys::Single("down".to_string()), "Move cursor down");
    insert("tui.editor.historyPrevious", Keys::Multiple(vec![]), "Select previous prompt history entry");
    insert("tui.editor.historyNext", Keys::Multiple(vec![]), "Select next prompt history entry");
    insert("tui.editor.cursorLeft", Keys::Multiple(vec!["left".to_string(), "ctrl+b".to_string()]), "Move cursor left");
    insert("tui.editor.cursorRight", Keys::Multiple(vec!["right".to_string(), "ctrl+f".to_string()]), "Move cursor right");
    insert("tui.editor.cursorWordLeft", Keys::Multiple(vec!["alt+left".to_string(), "ctrl+left".to_string(), "alt+b".to_string()]), "Move cursor word left");
    insert("tui.editor.cursorWordRight", Keys::Multiple(vec!["alt+right".to_string(), "ctrl+right".to_string(), "alt+f".to_string()]), "Move cursor word right");
    insert("tui.editor.cursorLineStart", Keys::Multiple(vec!["home".to_string(), "ctrl+home".to_string(), "ctrl+a".to_string()]), "Move to line start");
    insert("tui.editor.cursorLineEnd", Keys::Multiple(vec!["end".to_string(), "ctrl+end".to_string(), "ctrl+e".to_string()]), "Move to line end");
    insert("tui.editor.jumpForward", Keys::Single("ctrl+]".to_string()), "Jump forward to character");
    insert("tui.editor.jumpBackward", Keys::Single("ctrl+alt+]".to_string()), "Jump backward to character");
    insert("tui.editor.pageUp", Keys::Multiple(vec!["pageUp".to_string(), "ctrl+pageUp".to_string()]), "Page up");
    insert("tui.editor.pageDown", Keys::Multiple(vec!["pageDown".to_string(), "ctrl+pageDown".to_string()]), "Page down");
    insert("tui.editor.deleteCharBackward", Keys::Single("backspace".to_string()), "Delete character backward");
    insert("tui.editor.deleteCharForward", Keys::Multiple(vec!["delete".to_string(), "ctrl+d".to_string()]), "Delete character forward");
    insert("tui.editor.deleteWordBackward", Keys::Multiple(vec!["ctrl+w".to_string(), "alt+backspace".to_string()]), "Delete word backward");
    insert("tui.editor.deleteWordForward", Keys::Multiple(vec!["alt+d".to_string(), "alt+delete".to_string()]), "Delete word forward");
    insert("tui.editor.deleteToLineStart", Keys::Single("ctrl+u".to_string()), "Delete to line start");
    insert("tui.editor.deleteToLineEnd", Keys::Single("ctrl+k".to_string()), "Delete to line end");
    insert("tui.editor.yank", Keys::Single("ctrl+y".to_string()), "Yank");
    insert("tui.editor.yankPop", Keys::Single("alt+y".to_string()), "Yank pop");
    insert("tui.editor.undo", Keys::Single("ctrl+-".to_string()), "Undo");
    insert("tui.input.newLine", Keys::Multiple(vec!["shift+enter".to_string(), "ctrl+j".to_string()]), "Insert newline");
    insert("tui.input.submit", Keys::Single("enter".to_string()), "Submit input");
    insert("tui.input.tab", Keys::Single("tab".to_string()), "Tab / autocomplete");
    insert("tui.input.copy", Keys::Single("ctrl+c".to_string()), "Copy selection");
    insert("tui.select.up", Keys::Single("up".to_string()), "Move selection up");
    insert("tui.select.down", Keys::Single("down".to_string()), "Move selection down");
    insert("tui.select.pageUp", Keys::Single("pageUp".to_string()), "Selection page up");
    insert("tui.select.pageDown", Keys::Single("pageDown".to_string()), "Selection page down");
    insert("tui.select.confirm", Keys::Single("enter".to_string()), "Confirm selection");
    insert("tui.select.cancel", Keys::Multiple(vec!["escape".to_string(), "ctrl+c".to_string()]), "Cancel selection");
    insert("tui.altScreen.pageUp", Keys::Single("pageUp".to_string()), "Scroll viewport up one page");
    insert("tui.altScreen.pageDown", Keys::Single("pageDown".to_string()), "Scroll viewport down one page");
    insert("tui.altScreen.halfPageUp", Keys::Multiple(vec![]), "Scroll viewport up half a page");
    insert("tui.altScreen.halfPageDown", Keys::Multiple(vec![]), "Scroll viewport down half a page");
    insert("tui.altScreen.previousPrompt", Keys::Single("ctrl+shift+up".to_string()), "Jump to previous semantic prompt");
    insert("tui.altScreen.nextPrompt", Keys::Single("ctrl+shift+down".to_string()), "Jump to next semantic prompt");
    insert("tui.altScreen.top", Keys::Single("home".to_string()), "Scroll viewport to top");
    insert("tui.altScreen.bottom", Keys::Single("end".to_string()), "Scroll viewport to bottom");
    definitions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> KeybindingsManager {
        KeybindingsManager::new(tui_keybindings())
    }

    #[test]
    fn default_bindings_match() {
        let manager = manager();
        assert!(manager.matches("\x1b[A", "tui.editor.cursorUp"));
        assert!(manager.matches("\r", "tui.input.submit"));
        assert!(manager.matches("\t", "tui.input.tab"));
        assert!(manager.matches("\x01", "tui.editor.cursorLineStart")); // ctrl+a
        assert!(!manager.matches("x", "tui.input.submit"));
    }

    #[test]
    fn multi_key_bindings() {
        let manager = manager();
        assert!(manager.matches("\x1b[D", "tui.editor.cursorLeft"));
        assert!(manager.matches("\x02", "tui.editor.cursorLeft")); // ctrl+b
        assert_eq!(
            manager.get_keys("tui.editor.cursorLeft"),
            vec!["left".to_string(), "ctrl+b".to_string()]
        );
    }

    #[test]
    fn user_bindings_override() {
        let mut manager = manager();
        let mut user = HashMap::new();
        user.insert(
            "tui.input.submit".to_string(),
            Some(Keys::Single("ctrl+enter".to_string())),
        );
        manager.set_user_bindings(user);
        assert!(!manager.matches("\r", "tui.input.submit"));
        assert!(manager.matches("\x1b[13;5u", "tui.input.submit"));
    }

    #[test]
    fn conflicts_detected() {
        let mut manager = manager();
        let mut user = HashMap::new();
        user.insert(
            "tui.editor.cursorUp".to_string(),
            Some(Keys::Single("x".to_string())),
        );
        user.insert(
            "tui.editor.cursorDown".to_string(),
            Some(Keys::Single("x".to_string())),
        );
        manager.set_user_bindings(user);
        let conflicts = manager.get_conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].key, "x");
    }

    #[test]
    fn resolved_bindings_shape() {
        let manager = manager();
        let resolved = manager.get_resolved_bindings();
        assert_eq!(resolved.len(), tui_keybindings().len());
        let submit = resolved.get("tui.input.submit").unwrap().clone().unwrap();
        assert_eq!(submit.as_vec(), vec!["enter".to_string()]);
    }

    #[test]
    fn unknown_keybinding_never_matches() {
        let manager = manager();
        assert!(!manager.matches("\r", "tui.doesNotExist"));
        assert!(manager.get_keys("tui.doesNotExist").is_empty());
    }
}
