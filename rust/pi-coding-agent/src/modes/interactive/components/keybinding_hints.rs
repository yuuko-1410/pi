//! Keybinding hint formatting, port of `components/keybinding-hints.ts`.

use std::sync::Mutex;


use crate::core::keybindings::KeybindingsManager;

/// Global keybinding manager used for hint text (JS getKeybindings).
/// Defaults to the app defaults; interactive-mode sets the real instance.
static GLOBAL_KEYBINDINGS: Mutex<Option<KeybindingsManager>> = Mutex::new(None);

pub fn set_global_keybindings(manager: KeybindingsManager) {
    *GLOBAL_KEYBINDINGS.lock().unwrap() = Some(manager);
}

fn get_manager() -> std::sync::MutexGuard<'static, Option<KeybindingsManager>> {
    GLOBAL_KEYBINDINGS.lock().unwrap()
}


pub struct KeyTextFormatOptions {
    pub capitalize: bool,
}

impl Default for KeyTextFormatOptions {
    fn default() -> Self {
        Self { capitalize: false }
    }
}

fn format_key_part(part: &str, options: &KeyTextFormatOptions) -> String {
    let display_part = if cfg!(target_os = "macos") && part.to_lowercase() == "alt" {
        "option".to_string()
    } else {
        part.to_string()
    };
    if options.capitalize {
        let mut chars = display_part.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => display_part,
        }
    } else {
        display_part
    }
}

pub fn format_key_text(key: &str, options: KeyTextFormatOptions) -> String {
    key.split('/')
        .map(|k| {
            k.split('+')
                .map(|part| format_key_part(part, &options))
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub fn key_text(keybinding: &str) -> String {
    let manager = get_manager();
    let keys = match &*manager {
        Some(manager) => manager.get_keys(keybinding),
        None => {
            // Fall back to the app defaults so hints work before init.
            let defaults = KeybindingsManager::new(Default::default(), None);
            defaults.get_keys(keybinding)
        }
    };
    if keys.is_empty() {
        return String::new();
    }
    format_key_text(&keys.join("/"), KeyTextFormatOptions::default())
}

/// Dim key + muted description hint line.
pub fn key_hint(keybinding: &str, description: &str) -> String {
    let keys = key_text(keybinding);
    let key_part = if keys.is_empty() {
        String::new()
    } else {
        crate::modes::interactive::theme::theme::theme()
            .as_ref()
            .map(|t| t.fg("dim", &keys))
            .unwrap_or(keys)
    };
    let desc_part = crate::modes::interactive::theme::theme::theme()
        .as_ref()
        .map(|t| t.fg("muted", &format!(" {description}")))
        .unwrap_or_else(|| format!(" {description}"));
    key_part + &desc_part
}

pub fn raw_key_hint(key: &str, description: &str) -> String {
    let formatted = format_key_text(key, KeyTextFormatOptions::default());
    let key_part = crate::modes::interactive::theme::theme::theme()
        .as_ref()
        .map(|t| t.fg("dim", &formatted))
        .unwrap_or(formatted);
    let desc_part = crate::modes::interactive::theme::theme::theme()
        .as_ref()
        .map(|t| t.fg("muted", &format!(" {description}")))
        .unwrap_or_else(|| format!(" {description}"));
    key_part + &desc_part
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_modifier_joins() {
        assert_eq!(format_key_text("ctrl+x", KeyTextFormatOptions::default()), "ctrl+x");
        assert_eq!(
            format_key_text("ctrl+shift+p", KeyTextFormatOptions { capitalize: true }),
            "Ctrl+Shift+P"
        );
    }

    #[test]
    fn splits_slash_alternatives() {
        assert_eq!(
            format_key_text("a/b", KeyTextFormatOptions::default()),
            "a/b"
        );
    }
}
