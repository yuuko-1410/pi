//! Keyboard input parsing and matching, port of `packages/tui/src/keys.ts`.
//!
//! Differences: `process.env.WT_SESSION` detection (Windows Terminal) is
//! approximated by the `PI_WINDOWS_TERMINAL` env var; Kitty protocol
//! activity is process-global (AtomicBool), matching the JS module state.

use std::sync::atomic::{AtomicBool, Ordering};

pub type KeyId = String;

pub const MOD_SHIFT: i64 = 1;
pub const MOD_ALT: i64 = 2;
pub const MOD_CTRL: i64 = 4;
pub const MOD_SUPER: i64 = 8;
const LOCK_MASK: i64 = 64 + 128;

pub const CODEPOINT_ESCAPE: i64 = 27;
pub const CODEPOINT_TAB: i64 = 9;
pub const CODEPOINT_ENTER: i64 = 13;
pub const CODEPOINT_SPACE: i64 = 32;
pub const CODEPOINT_BACKSPACE: i64 = 127;
pub const CODEPOINT_KP_ENTER: i64 = 57414;

pub const ARROW_UP: i64 = -1;
pub const ARROW_DOWN: i64 = -2;
pub const ARROW_RIGHT: i64 = -3;
pub const ARROW_LEFT: i64 = -4;

pub const FUNC_DELETE: i64 = -10;
pub const FUNC_INSERT: i64 = -11;
pub const FUNC_PAGE_UP: i64 = -12;
pub const FUNC_PAGE_DOWN: i64 = -13;
pub const FUNC_HOME: i64 = -14;
pub const FUNC_END: i64 = -15;

static KITTY_PROTOCOL_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn set_kitty_protocol_active(active: bool) {
    KITTY_PROTOCOL_ACTIVE.store(active, Ordering::SeqCst);
}

pub fn is_kitty_protocol_active() -> bool {
    KITTY_PROTOCOL_ACTIVE.load(Ordering::SeqCst)
}

const SYMBOL_KEYS: &[&str] = &[
    "`", "-", "=", "[", "]", "\\", ";", "'", ",", ".", "/", "!", "@", "#", "$", "%", "^", "&", "*",
    "(", ")", "_", "+", "|", "~", "{", "}", ":", "<", ">", "?",
];

fn is_symbol_key(key: &str) -> bool {
    SYMBOL_KEYS.contains(&key)
}

fn kitty_functional_equivalent(codepoint: i64) -> i64 {
    match codepoint {
        57399 => 48,
        57400 => 49,
        57401 => 50,
        57402 => 51,
        57403 => 52,
        57404 => 53,
        57405 => 54,
        57406 => 55,
        57407 => 56,
        57408 => 57,
        57409 => 46,
        57410 => 47,
        57411 => 42,
        57412 => 45,
        57413 => 43,
        57415 => 61,
        57416 => 44,
        57417 => ARROW_LEFT,
        57418 => ARROW_RIGHT,
        57419 => ARROW_UP,
        57420 => ARROW_DOWN,
        57421 => FUNC_PAGE_UP,
        57422 => FUNC_PAGE_DOWN,
        57423 => FUNC_HOME,
        57424 => FUNC_END,
        57425 => FUNC_INSERT,
        57426 => FUNC_DELETE,
        other => other,
    }
}

fn normalize_shifted_letter_identity_codepoint(codepoint: i64, modifier: i64) -> i64 {
    let effective_modifier = modifier & !LOCK_MASK;
    if effective_modifier & MOD_SHIFT != 0 && (65..=90).contains(&codepoint) {
        return codepoint + 32;
    }
    codepoint
}

// ---------------------------------------------------------------------------
// Legacy sequences
// ---------------------------------------------------------------------------

fn legacy_sequences(key: &str) -> &'static [&'static str] {
    match key {
        "up" => &["\x1b[A", "\x1bOA"],
        "down" => &["\x1b[B", "\x1bOB"],
        "right" => &["\x1b[C", "\x1bOC"],
        "left" => &["\x1b[D", "\x1bOD"],
        "home" => &["\x1b[H", "\x1bOH", "\x1b[1~", "\x1b[7~"],
        "end" => &["\x1b[F", "\x1bOF", "\x1b[4~", "\x1b[8~"],
        "insert" => &["\x1b[2~"],
        "delete" => &["\x1b[3~"],
        "pageUp" => &["\x1b[5~", "\x1b[[5~"],
        "pageDown" => &["\x1b[6~", "\x1b[[6~"],
        "clear" => &["\x1b[E", "\x1bOE"],
        "f1" => &["\x1bOP", "\x1b[11~", "\x1b[[A"],
        "f2" => &["\x1bOQ", "\x1b[12~", "\x1b[[B"],
        "f3" => &["\x1bOR", "\x1b[13~", "\x1b[[C"],
        "f4" => &["\x1bOS", "\x1b[14~", "\x1b[[D"],
        "f5" => &["\x1b[15~", "\x1b[[E"],
        "f6" => &["\x1b[17~"],
        "f7" => &["\x1b[18~"],
        "f8" => &["\x1b[19~"],
        "f9" => &["\x1b[20~"],
        "f10" => &["\x1b[21~"],
        "f11" => &["\x1b[23~"],
        "f12" => &["\x1b[24~"],
        _ => &[],
    }
}

fn legacy_shift_sequences(key: &str) -> &'static [&'static str] {
    match key {
        "up" => &["\x1b[a"],
        "down" => &["\x1b[b"],
        "right" => &["\x1b[c"],
        "left" => &["\x1b[d"],
        "clear" => &["\x1b[e"],
        "insert" => &["\x1b[2$"],
        "delete" => &["\x1b[3$"],
        "pageUp" => &["\x1b[5$"],
        "pageDown" => &["\x1b[6$"],
        "home" => &["\x1b[7$"],
        "end" => &["\x1b[8$"],
        _ => &[],
    }
}

fn legacy_ctrl_sequences(key: &str) -> &'static [&'static str] {
    match key {
        "up" => &["\x1bOa"],
        "down" => &["\x1bOb"],
        "right" => &["\x1bOc"],
        "left" => &["\x1bOd"],
        "clear" => &["\x1bOe"],
        "insert" => &["\x1b[2^"],
        "delete" => &["\x1b[3^"],
        "pageUp" => &["\x1b[5^"],
        "pageDown" => &["\x1b[6^"],
        "home" => &["\x1b[7^"],
        "end" => &["\x1b[8^"],
        _ => &[],
    }
}

fn matches_legacy_sequence(data: &str, sequences: &[&str]) -> bool {
    sequences.contains(&data)
}

fn matches_legacy_modifier_sequence(data: &str, key: &str, modifier: i64) -> bool {
    if modifier == MOD_SHIFT {
        matches_legacy_sequence(data, legacy_shift_sequences(key))
    } else if modifier == MOD_CTRL {
        matches_legacy_sequence(data, legacy_ctrl_sequences(key))
    } else {
        false
    }
}

/// Legacy sequences mapped directly to key ids.
fn legacy_sequence_key_id(data: &str) -> Option<&'static str> {
    Some(match data {
        "\x1bOA" => "up",
        "\x1bOB" => "down",
        "\x1bOC" => "right",
        "\x1bOD" => "left",
        "\x1bOH" => "home",
        "\x1bOF" => "end",
        "\x1b[E" => "clear",
        "\x1bOE" => "clear",
        "\x1bOe" => "ctrl+clear",
        "\x1b[e" => "shift+clear",
        "\x1b[2~" => "insert",
        "\x1b[2$" => "shift+insert",
        "\x1b[2^" => "ctrl+insert",
        "\x1b[3$" => "shift+delete",
        "\x1b[3^" => "ctrl+delete",
        "\x1b[[5~" => "pageUp",
        "\x1b[[6~" => "pageDown",
        "\x1b[a" => "shift+up",
        "\x1b[b" => "shift+down",
        "\x1b[c" => "shift+right",
        "\x1b[d" => "shift+left",
        "\x1bOa" => "ctrl+up",
        "\x1bOb" => "ctrl+down",
        "\x1bOc" => "ctrl+right",
        "\x1bOd" => "ctrl+left",
        "\x1b[5$" => "shift+pageUp",
        "\x1b[6$" => "shift+pageDown",
        "\x1b[7$" => "shift+home",
        "\x1b[8$" => "shift+end",
        "\x1b[5^" => "ctrl+pageUp",
        "\x1b[6^" => "ctrl+pageDown",
        "\x1b[7^" => "ctrl+home",
        "\x1b[8^" => "ctrl+end",
        "\x1bOP" => "f1",
        "\x1bOQ" => "f2",
        "\x1bOR" => "f3",
        "\x1bOS" => "f4",
        "\x1b[11~" => "f1",
        "\x1b[12~" => "f2",
        "\x1b[13~" => "f3",
        "\x1b[14~" => "f4",
        "\x1b[[A" => "f1",
        "\x1b[[B" => "f2",
        "\x1b[[C" => "f3",
        "\x1b[[D" => "f4",
        "\x1b[[E" => "f5",
        "\x1b[15~" => "f5",
        "\x1b[17~" => "f6",
        "\x1b[18~" => "f7",
        "\x1b[19~" => "f8",
        "\x1b[20~" => "f9",
        "\x1b[21~" => "f10",
        "\x1b[23~" => "f11",
        "\x1b[24~" => "f12",
        "\x1bb" => "alt+left",
        "\x1bf" => "alt+right",
        "\x1bp" => "alt+up",
        "\x1bn" => "alt+down",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Kitty protocol parsing
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum KeyEventType {
    Press,
    Repeat,
    Release,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedKittySequence {
    pub codepoint: i64,
    pub shifted_key: Option<i64>,
    pub base_layout_key: Option<i64>,
    pub modifier: i64,
    pub event_type: KeyEventType,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedModifyOtherKeysSequence {
    pub codepoint: i64,
    pub modifier: i64,
}

fn parse_event_type(event_type_str: Option<&str>) -> KeyEventType {
    match event_type_str {
        None => KeyEventType::Press,
        Some(value) => match value.parse::<i64>() {
            Ok(2) => KeyEventType::Repeat,
            Ok(3) => KeyEventType::Release,
            _ => KeyEventType::Press,
        },
    }
}

/// Parse a Kitty CSI-u sequence or modified arrows/functional keys.
pub fn parse_kitty_sequence(data: &str) -> Option<ParsedKittySequence> {
    // CSI u: ESC [ codepoint [:shifted[:base]] [;mod] [:event] u
    if let Some(rest) = data.strip_prefix("\x1b[") {
        if let Some(rest) = rest.strip_suffix('u') {
            let mut parts = rest.split([';', ':']);
            let codepoint = parts.next()?.parse::<i64>().ok()?;
            let colons: Vec<&str> = rest.split(';').next().unwrap_or("").split(':').skip(1).collect();
            // Reconstruct: rest = "cp[:shifted[:base]][;mod][:event]"
            let mut segments = rest.split(';');
            let first = segments.next().unwrap_or("");
            let mut first_parts = first.split(':');
            let shifted_key = first_parts.nth(1).filter(|s| !s.is_empty()).map(|s| s.parse::<i64>().ok()).flatten();
            let base_layout_key = first_parts.next().map(|s| s.parse::<i64>().ok()).flatten();
            let mod_value = segments.next().map(|s| s.parse::<i64>().ok()).flatten().unwrap_or(1);
            let event_type = segments.next().map(|s| s.parse::<i64>().ok()).flatten();
            let _ = parts;
            let _ = colons;
            return Some(ParsedKittySequence {
                codepoint,
                shifted_key,
                base_layout_key,
                modifier: mod_value - 1,
                event_type: parse_event_type(event_type.map(|v| v.to_string()).as_deref()),
            });
        }
    }

    // Arrow with modifier: ESC [ 1 ; mod [:event] A/B/C/D
    if let Some(rest) = data.strip_prefix("\x1b[1;") {
        if let Some(tail) = rest.strip_suffix(['A', 'B', 'C', 'D']) {
            if let Some(suffix) = rest.strip_suffix('A').or_else(|| rest.strip_suffix('B')).or_else(|| rest.strip_suffix('C')).or_else(|| rest.strip_suffix('D')) {
                let _ = tail;
                let mut parts = suffix.split(':');
                let mod_value = parts.next()?.parse::<i64>().ok()?;
                let event_type = parts.next();
                let arrow_code = match rest.chars().last()? {
                    'A' => ARROW_UP,
                    'B' => ARROW_DOWN,
                    'C' => ARROW_RIGHT,
                    'D' => ARROW_LEFT,
                    _ => return None,
                };
                return Some(ParsedKittySequence {
                    codepoint: arrow_code,
                    shifted_key: None,
                    base_layout_key: None,
                    modifier: mod_value - 1,
                    event_type: parse_event_type(event_type),
                });
            }
        }
    }

    // Functional: ESC [ num [;mod] [:event] ~
    if let Some(rest) = data.strip_prefix("\x1b[").and_then(|r| r.strip_suffix('~')) {
        let mut parts = rest.split([';', ':']);
        let key_num = parts.next()?.parse::<i64>().ok()?;
        let mod_value = parts.next().map(|s| s.parse::<i64>().ok()).flatten().unwrap_or(1);
        let event_type = parts.next();
        let func_code = match key_num {
            2 => FUNC_INSERT,
            3 => FUNC_DELETE,
            5 => FUNC_PAGE_UP,
            6 => FUNC_PAGE_DOWN,
            7 => FUNC_HOME,
            8 => FUNC_END,
            _ => return None,
        };
        return Some(ParsedKittySequence {
            codepoint: func_code,
            shifted_key: None,
            base_layout_key: None,
            modifier: mod_value - 1,
            event_type: parse_event_type(event_type),
        });
    }

    // Home/End with modifier: ESC [ 1 ; mod [:event] H/F
    if let Some(rest) = data.strip_prefix("\x1b[1;") {
        if let Some(suffix) = rest.strip_suffix('H').or_else(|| rest.strip_suffix('F')) {
            let mut parts = suffix.split(':');
            let mod_value = parts.next()?.parse::<i64>().ok()?;
            let event_type = parts.next();
            let codepoint = if rest.ends_with('H') { FUNC_HOME } else { FUNC_END };
            return Some(ParsedKittySequence {
                codepoint,
                shifted_key: None,
                base_layout_key: None,
                modifier: mod_value - 1,
                event_type: parse_event_type(event_type),
            });
        }
    }

    None
}

fn matches_kitty_sequence(data: &str, expected_codepoint: i64, expected_modifier: i64) -> bool {
    let Some(parsed) = parse_kitty_sequence(data) else {
        return false;
    };
    let actual_mod = parsed.modifier & !LOCK_MASK;
    let expected_mod = expected_modifier & !LOCK_MASK;
    if actual_mod != expected_mod {
        return false;
    }

    let normalized_codepoint = normalize_shifted_letter_identity_codepoint(
        kitty_functional_equivalent(parsed.codepoint),
        parsed.modifier,
    );
    let normalized_expected = normalize_shifted_letter_identity_codepoint(
        kitty_functional_equivalent(expected_codepoint),
        expected_modifier,
    );

    if normalized_codepoint == normalized_expected {
        return true;
    }

    // Alternate match via base layout key for non-Latin layouts.
    if let Some(base_layout_key) = parsed.base_layout_key {
        if base_layout_key == expected_codepoint {
            let cp = normalized_codepoint;
            let is_latin_letter = (97..=122).contains(&cp);
            let is_known_symbol = (32..=126).contains(&cp) && {
                let char = char::from_u32(cp as u32).unwrap_or(' ');
                is_symbol_key(&char.to_string())
            };
            if !is_latin_letter && !is_known_symbol {
                return true;
            }
        }
    }

    false
}

fn parse_modify_other_keys_sequence(data: &str) -> Option<ParsedModifyOtherKeysSequence> {
    // ESC [ 27 ; mod ; codepoint ~
    let rest = data.strip_prefix("\x1b[27;")?.strip_suffix('~')?;
    let mut parts = rest.split(';');
    let mod_value = parts.next()?.parse::<i64>().ok()?;
    let codepoint = parts.next()?.parse::<i64>().ok()?;
    Some(ParsedModifyOtherKeysSequence {
        codepoint,
        modifier: mod_value - 1,
    })
}

fn matches_modify_other_keys(data: &str, expected_keycode: i64, expected_modifier: i64) -> bool {
    match parse_modify_other_keys_sequence(data) {
        Some(parsed) => parsed.codepoint == expected_keycode && parsed.modifier == expected_modifier,
        None => false,
    }
}

fn is_windows_terminal_session() -> bool {
    std::env::var("PI_WINDOWS_TERMINAL").is_ok()
        && std::env::var("SSH_CONNECTION").is_err()
        && std::env::var("SSH_CLIENT").is_err()
        && std::env::var("SSH_TTY").is_err()
}

fn matches_raw_backspace(data: &str, expected_modifier: i64) -> bool {
    if data == "\x7f" {
        return expected_modifier == 0;
    }
    if data != "\x08" {
        return false;
    }
    if is_windows_terminal_session() {
        expected_modifier == MOD_CTRL
    } else {
        expected_modifier == 0
    }
}

fn raw_ctrl_char(key: &str) -> Option<String> {
    let char = key.to_lowercase();
    let code = char.as_bytes().first().copied().unwrap_or(0);
    if (97..=122).contains(&code) || matches!(char.as_str(), "[" | "\\" | "]" | "_") {
        return Some(String::from_utf8(vec![code & 0x1f]).unwrap_or_default());
    }
    if char == "-" {
        return Some(String::from_utf8(vec![31]).unwrap_or_default());
    }
    None
}

fn is_digit_key(key: &str) -> bool {
    key.len() == 1 && matches!(key.as_bytes()[0], b'0'..=b'9')
}

fn matches_printable_modify_other_keys(data: &str, expected_keycode: i64, expected_modifier: i64) -> bool {
    if expected_modifier == 0 {
        return false;
    }
    match parse_modify_other_keys_sequence(data) {
        Some(parsed) if parsed.modifier == expected_modifier => {
            normalize_shifted_letter_identity_codepoint(parsed.codepoint, parsed.modifier)
                == normalize_shifted_letter_identity_codepoint(expected_keycode, expected_modifier)
        }
        _ => false,
    }
}

/// Check if the last parsed key event was a key release.
pub fn is_key_release(data: &str) -> bool {
    if data.contains("\x1b[200~") {
        return false;
    }
    data.contains(":3u")
        || data.contains(":3~")
        || data.contains(":3A")
        || data.contains(":3B")
        || data.contains(":3C")
        || data.contains(":3D")
        || data.contains(":3H")
        || data.contains(":3F")
}

/// Check if the last parsed key event was a key repeat.
pub fn is_key_repeat(data: &str) -> bool {
    if data.contains("\x1b[200~") {
        return false;
    }
    data.contains(":2u")
        || data.contains(":2~")
        || data.contains(":2A")
        || data.contains(":2B")
        || data.contains(":2C")
        || data.contains(":2D")
        || data.contains(":2H")
        || data.contains(":2F")
}

// ---------------------------------------------------------------------------
// Key matching
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct ParsedKeyId {
    key: String,
    ctrl: bool,
    shift: bool,
    alt: bool,
    super_modifier: bool,
}

fn parse_key_id(key_id: &str) -> Option<ParsedKeyId> {
    let lowered = key_id.to_lowercase();
    let parts: Vec<&str> = lowered.split('+').collect();
    let key = parts.last()?.to_string();
    if key.is_empty() {
        return None;
    }
    Some(ParsedKeyId {
        key,
        ctrl: parts.contains(&"ctrl"),
        shift: parts.contains(&"shift"),
        alt: parts.contains(&"alt"),
        super_modifier: parts.contains(&"super"),
    })
}

/// Match input data against a key identifier string.
pub fn matches_key(data: &str, key_id: &KeyId) -> bool {
    let Some(parsed) = parse_key_id(key_id) else {
        return false;
    };
    let key = parsed.key.clone();
    let mut modifier = 0i64;
    if parsed.shift {
        modifier |= MOD_SHIFT;
    }
    if parsed.alt {
        modifier |= MOD_ALT;
    }
    if parsed.ctrl {
        modifier |= MOD_CTRL;
    }
    if parsed.super_modifier {
        modifier |= MOD_SUPER;
    }

    match key.as_str() {
        "escape" | "esc" => {
            if modifier != 0 {
                return false;
            }
            data == "\x1b"
                || matches_kitty_sequence(data, CODEPOINT_ESCAPE, 0)
                || matches_modify_other_keys(data, CODEPOINT_ESCAPE, 0)
        }
        "space" => {
            if !is_kitty_protocol_active() {
                if modifier == MOD_CTRL && data == "\x00" {
                    return true;
                }
                if modifier == MOD_ALT && data == "\x1b " {
                    return true;
                }
            }
            if modifier == 0 {
                data == " "
                    || matches_kitty_sequence(data, CODEPOINT_SPACE, 0)
                    || matches_modify_other_keys(data, CODEPOINT_SPACE, 0)
            } else {
                matches_kitty_sequence(data, CODEPOINT_SPACE, modifier)
                    || matches_modify_other_keys(data, CODEPOINT_SPACE, modifier)
            }
        }
        "tab" => {
            if modifier == MOD_SHIFT {
                data == "\x1b[Z"
                    || matches_kitty_sequence(data, CODEPOINT_TAB, MOD_SHIFT)
                    || matches_modify_other_keys(data, CODEPOINT_TAB, MOD_SHIFT)
            } else if modifier == 0 {
                data == "\t" || matches_kitty_sequence(data, CODEPOINT_TAB, 0)
            } else {
                matches_kitty_sequence(data, CODEPOINT_TAB, modifier)
                    || matches_modify_other_keys(data, CODEPOINT_TAB, modifier)
            }
        }
        "enter" | "return" => {
            if modifier == MOD_SHIFT {
                if matches_kitty_sequence(data, CODEPOINT_ENTER, MOD_SHIFT)
                    || matches_kitty_sequence(data, CODEPOINT_KP_ENTER, MOD_SHIFT)
                    || matches_modify_other_keys(data, CODEPOINT_ENTER, MOD_SHIFT)
                {
                    return true;
                }
                if is_kitty_protocol_active() {
                    return data == "\x1b\r" || data == "\n";
                }
                false
            } else if modifier == MOD_ALT {
                if matches_kitty_sequence(data, CODEPOINT_ENTER, MOD_ALT)
                    || matches_kitty_sequence(data, CODEPOINT_KP_ENTER, MOD_ALT)
                    || matches_modify_other_keys(data, CODEPOINT_ENTER, MOD_ALT)
                {
                    return true;
                }
                if !is_kitty_protocol_active() {
                    return data == "\x1b\r";
                }
                false
            } else if modifier == 0 {
                data == "\r"
                    || (!is_kitty_protocol_active() && data == "\n")
                    || data == "\x1bOM"
                    || matches_kitty_sequence(data, CODEPOINT_ENTER, 0)
                    || matches_kitty_sequence(data, CODEPOINT_KP_ENTER, 0)
            } else {
                matches_kitty_sequence(data, CODEPOINT_ENTER, modifier)
                    || matches_kitty_sequence(data, CODEPOINT_KP_ENTER, modifier)
                    || matches_modify_other_keys(data, CODEPOINT_ENTER, modifier)
            }
        }
        "backspace" => {
            if modifier == MOD_ALT {
                if data == "\x1b\x7f" || data == "\x1b\u{8}" {
                    return true;
                }
                matches_kitty_sequence(data, CODEPOINT_BACKSPACE, MOD_ALT)
                    || matches_modify_other_keys(data, CODEPOINT_BACKSPACE, MOD_ALT)
            } else if modifier == MOD_CTRL {
                if matches_raw_backspace(data, MOD_CTRL) {
                    return true;
                }
                matches_kitty_sequence(data, CODEPOINT_BACKSPACE, MOD_CTRL)
                    || matches_modify_other_keys(data, CODEPOINT_BACKSPACE, MOD_CTRL)
            } else if modifier == 0 {
                matches_raw_backspace(data, 0)
                    || matches_kitty_sequence(data, CODEPOINT_BACKSPACE, 0)
                    || matches_modify_other_keys(data, CODEPOINT_BACKSPACE, 0)
            } else {
                matches_kitty_sequence(data, CODEPOINT_BACKSPACE, modifier)
                    || matches_modify_other_keys(data, CODEPOINT_BACKSPACE, modifier)
            }
        }
        "insert" | "delete" | "clear" | "home" | "end" | "pageup" | "pagedown" => {
            let legacy_key = match key.as_str() {
                "pageup" => "pageUp",
                "pagedown" => "pageDown",
                other => other,
            };
            let func_code = match key.as_str() {
                "insert" => FUNC_INSERT,
                "delete" => FUNC_DELETE,
                "home" => FUNC_HOME,
                "end" => FUNC_END,
                "pageup" => FUNC_PAGE_UP,
                "pagedown" => FUNC_PAGE_DOWN,
                _ => 0,
            };
            if key == "clear" {
                if modifier == 0 {
                    return matches_legacy_sequence(data, legacy_sequences("clear"));
                }
                return matches_legacy_modifier_sequence(data, "clear", modifier);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_sequences(legacy_key))
                    || matches_kitty_sequence(data, func_code, 0);
            }
            matches_legacy_modifier_sequence(data, legacy_key, modifier)
                || matches_kitty_sequence(data, func_code, modifier)
        }
        "up" | "down" | "left" | "right" => {
            let arrow = match key.as_str() {
                "up" => ARROW_UP,
                "down" => ARROW_DOWN,
                "left" => ARROW_LEFT,
                _ => ARROW_RIGHT,
            };
            let alt_prefix = match key.as_str() {
                "up" => "\x1bp",
                "down" => "\x1bn",
                "left" => "\x1bb",
                _ => "\x1bf",
            };
            if modifier == MOD_ALT {
                if key == "left" {
                    return data == "\x1b[1;3D"
                        || (!is_kitty_protocol_active() && data == "\x1bB")
                        || data == "\x1bb"
                        || matches_kitty_sequence(data, ARROW_LEFT, MOD_ALT);
                }
                if key == "right" {
                    return data == "\x1b[1;3C"
                        || (!is_kitty_protocol_active() && data == "\x1bF")
                        || data == "\x1bf"
                        || matches_kitty_sequence(data, ARROW_RIGHT, MOD_ALT);
                }
                return data == alt_prefix || matches_kitty_sequence(data, arrow, MOD_ALT);
            }
            if modifier == MOD_CTRL && (key == "left" || key == "right") {
                let prefix = if key == "left" { "\x1b[1;5D" } else { "\x1b[1;5C" };
                return data == prefix
                    || matches_legacy_modifier_sequence(data, &key, MOD_CTRL)
                    || matches_kitty_sequence(data, arrow, MOD_CTRL);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_sequences(&key))
                    || matches_kitty_sequence(data, arrow, 0);
            }
            matches_legacy_modifier_sequence(data, &key, modifier)
                || matches_kitty_sequence(data, arrow, modifier)
        }
        "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12" => {
            if modifier != 0 {
                return false;
            }
            matches_legacy_sequence(data, legacy_sequences(&key))
        }
        _ => {
            // Single letter/digit/symbol keys.
            if key.len() == 1
                && (matches!(key.as_bytes()[0], b'a'..=b'z')
                    || is_digit_key(&key)
                    || is_symbol_key(&key))
            {
                let codepoint = key.as_bytes()[0] as i64;
                let raw_ctrl = raw_ctrl_char(&key);
                let is_letter = matches!(key.as_bytes()[0], b'a'..=b'z');
                let is_digit = is_digit_key(&key);

                if modifier == MOD_CTRL + MOD_ALT && !is_kitty_protocol_active() {
                    if let Some(raw_ctrl) = &raw_ctrl {
                        if data == format!("\x1b{raw_ctrl}") {
                            return true;
                        }
                    }
                }

                if modifier == MOD_ALT && !is_kitty_protocol_active() && (is_letter || is_digit || is_symbol_key(&key)) {
                    if data == format!("\x1b{key}") {
                        return true;
                    }
                }

                if modifier == MOD_CTRL {
                    if let Some(raw_ctrl) = &raw_ctrl {
                        if data == raw_ctrl {
                            return true;
                        }
                    }
                    return matches_kitty_sequence(data, codepoint, MOD_CTRL)
                        || matches_printable_modify_other_keys(data, codepoint, MOD_CTRL);
                }

                if modifier == MOD_SHIFT + MOD_CTRL {
                    return matches_kitty_sequence(data, codepoint, MOD_SHIFT + MOD_CTRL)
                        || matches_printable_modify_other_keys(data, codepoint, MOD_SHIFT + MOD_CTRL);
                }

                if modifier == MOD_SHIFT {
                    if is_letter && data == key.to_uppercase() {
                        return true;
                    }
                    return matches_kitty_sequence(data, codepoint, MOD_SHIFT)
                        || matches_printable_modify_other_keys(data, codepoint, MOD_SHIFT);
                }

                if modifier != 0 {
                    return matches_kitty_sequence(data, codepoint, modifier)
                        || matches_printable_modify_other_keys(data, codepoint, modifier);
                }

                data == key || matches_kitty_sequence(data, codepoint, 0)
            } else {
                false
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Key parsing
// ---------------------------------------------------------------------------

fn format_key_name_with_modifiers(key_name: &str, modifier: i64) -> Option<String> {
    let mut mods: Vec<String> = Vec::new();
    let effective_mod = modifier & !LOCK_MASK;
    let supported = MOD_SHIFT | MOD_CTRL | MOD_ALT | MOD_SUPER;
    if effective_mod & !supported != 0 {
        return None;
    }
    if effective_mod & MOD_SHIFT != 0 {
        mods.push("shift".to_string());
    }
    if effective_mod & MOD_CTRL != 0 {
        mods.push("ctrl".to_string());
    }
    if effective_mod & MOD_ALT != 0 {
        mods.push("alt".to_string());
    }
    if effective_mod & MOD_SUPER != 0 {
        mods.push("super".to_string());
    }
    if mods.is_empty() {
        Some(key_name.to_string())
    } else {
        Some(format!("{}+{key_name}", mods.join("+")))
    }
}

fn format_parsed_key(codepoint: i64, modifier: i64, base_layout_key: Option<i64>) -> Option<String> {
    let normalized = kitty_functional_equivalent(codepoint);
    let identity = normalize_shifted_letter_identity_codepoint(normalized, modifier);

    let is_latin_letter = (97..=122).contains(&identity);
    let is_digit = (48..=57).contains(&identity);
    let is_known_symbol = (32..=126).contains(&identity) && {
        let char = char::from_u32(identity as u32).unwrap_or(' ');
        is_symbol_key(&char.to_string())
    };
    let effective = if is_latin_letter || is_digit || is_known_symbol {
        identity
    } else {
        base_layout_key.unwrap_or(identity)
    };

    let key_name = if effective == CODEPOINT_ESCAPE {
        "escape"
    } else if effective == CODEPOINT_TAB {
        "tab"
    } else if effective == CODEPOINT_ENTER || effective == CODEPOINT_KP_ENTER {
        "enter"
    } else if effective == CODEPOINT_SPACE {
        "space"
    } else if effective == CODEPOINT_BACKSPACE {
        "backspace"
    } else if effective == FUNC_DELETE {
        "delete"
    } else if effective == FUNC_INSERT {
        "insert"
    } else if effective == FUNC_HOME {
        "home"
    } else if effective == FUNC_END {
        "end"
    } else if effective == FUNC_PAGE_UP {
        "pageUp"
    } else if effective == FUNC_PAGE_DOWN {
        "pageDown"
    } else if effective == ARROW_UP {
        "up"
    } else if effective == ARROW_DOWN {
        "down"
    } else if effective == ARROW_LEFT {
        "left"
    } else if effective == ARROW_RIGHT {
        "right"
    } else if (48..=57).contains(&effective) {
        return Some(format_key_name_with_modifiers(&(effective as u8 as char).to_string(), modifier)?);
    } else if (97..=122).contains(&effective) {
        return Some(format_key_name_with_modifiers(&(effective as u8 as char).to_string(), modifier)?);
    } else if (32..=126).contains(&effective) {
        let char = effective as u8 as char;
        if is_symbol_key(&char.to_string()) {
            return Some(format_key_name_with_modifiers(&char.to_string(), modifier)?);
        }
        return None;
    } else {
        return None;
    };

    format_key_name_with_modifiers(key_name, modifier)
}

/// Parse input data and return the key identifier if recognized.
pub fn parse_key(data: &str) -> Option<String> {
    if let Some(kitty) = parse_kitty_sequence(data) {
        return format_parsed_key(kitty.codepoint, kitty.modifier, kitty.base_layout_key);
    }
    if let Some(modify_other_keys) = parse_modify_other_keys_sequence(data) {
        return format_parsed_key(modify_other_keys.codepoint, modify_other_keys.modifier, None);
    }

    if is_kitty_protocol_active() {
        if data == "\x1b\r" || data == "\n" {
            return Some("shift+enter".to_string());
        }
    }

    if let Some(legacy) = legacy_sequence_key_id(data) {
        return Some(legacy.to_string());
    }

    if data == "\x1b" {
        return Some("escape".to_string());
    }
    if data == "\x1c" {
        return Some("ctrl+\\".to_string());
    }
    if data == "\x1d" {
        return Some("ctrl+]".to_string());
    }
    if data == "\x1f" {
        return Some("ctrl+-".to_string());
    }
    if data == "\x1b\x1b" {
        return Some("ctrl+alt+[".to_string());
    }
    if data == "\x1b\x1c" {
        return Some("ctrl+alt+\\".to_string());
    }
    if data == "\x1b\x1d" {
        return Some("ctrl+alt+]".to_string());
    }
    if data == "\x1b\x1f" {
        return Some("ctrl+alt+-".to_string());
    }
    if data == "\t" {
        return Some("tab".to_string());
    }
    if data == "\r" || (!is_kitty_protocol_active() && data == "\n") || data == "\x1bOM" {
        return Some("enter".to_string());
    }
    if data == "\x00" {
        return Some("ctrl+space".to_string());
    }
    if data == " " {
        return Some("space".to_string());
    }
    if data == "\x7f" {
        return Some("backspace".to_string());
    }
    if data == "\x08" {
        return Some(if is_windows_terminal_session() {
            "ctrl+backspace".to_string()
        } else {
            "backspace".to_string()
        });
    }
    if data == "\x1b[Z" {
        return Some("shift+tab".to_string());
    }
    if !is_kitty_protocol_active() && data == "\x1b\r" {
        return Some("alt+enter".to_string());
    }
    if !is_kitty_protocol_active() && data == "\x1b " {
        return Some("alt+space".to_string());
    }
    if data == "\x1b\x7f" || data == "\x1b\u{8}" {
        return Some("alt+backspace".to_string());
    }
    if !is_kitty_protocol_active() && data == "\x1bB" {
        return Some("alt+left".to_string());
    }
    if !is_kitty_protocol_active() && data == "\x1bF" {
        return Some("alt+right".to_string());
    }
    if !is_kitty_protocol_active() && data.len() == 2 && data.starts_with('\x1b') {
        let code = data.as_bytes()[1];
        if (1..=26).contains(&code) {
            return Some(format!("ctrl+alt+{}", (code + 96) as char));
        }
        let key = (code as char).to_string();
        if (97..=122).contains(&code) || (48..=57).contains(&code) || is_symbol_key(&key) {
            return Some(format!("alt+{key}"));
        }
    }
    if data == "\x1b[A" {
        return Some("up".to_string());
    }
    if data == "\x1b[B" {
        return Some("down".to_string());
    }
    if data == "\x1b[C" {
        return Some("right".to_string());
    }
    if data == "\x1b[D" {
        return Some("left".to_string());
    }
    if data == "\x1b[H" || data == "\x1bOH" {
        return Some("home".to_string());
    }
    if data == "\x1b[F" || data == "\x1bOF" {
        return Some("end".to_string());
    }
    if data == "\x1b[3~" {
        return Some("delete".to_string());
    }
    if data == "\x1b[5~" {
        return Some("pageUp".to_string());
    }
    if data == "\x1b[6~" {
        return Some("pageDown".to_string());
    }

    if data.len() == 1 {
        let code = data.as_bytes()[0];
        if (1..=26).contains(&code) {
            return Some(format!("ctrl+{}", (code + 96) as char));
        }
        if (32..=126).contains(&code) {
            return Some(data.to_string());
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Printable decoding
// ---------------------------------------------------------------------------

fn kitty_csi_u_parts(data: &str) -> Option<(i64, Option<i64>, Option<i64>, i64, Option<i64>)> {
    let rest = data.strip_prefix("\x1b[")?.strip_suffix('u')?;
    let mut segments = rest.split(';');
    let first = segments.next()?;
    let mut first_parts = first.split(':');
    let codepoint = first_parts.next()?.parse::<i64>().ok()?;
    let shifted = first_parts.next().filter(|s| !s.is_empty()).map(|s| s.parse::<i64>().ok()).flatten();
    let base = first_parts.next().map(|s| s.parse::<i64>().ok()).flatten();
    let mod_value = segments.next().map(|s| s.parse::<i64>().ok()).flatten().unwrap_or(1);
    let event_type = segments.next().map(|s| s.parse::<i64>().ok()).flatten();
    Some((codepoint, shifted, base, mod_value, event_type))
}

/// Decode a Kitty CSI-u sequence into a printable character.
pub fn decode_kitty_printable(data: &str) -> Option<String> {
    let (codepoint, shifted_key, _, mod_value, _) = kitty_csi_u_parts(data)?;
    let modifier = mod_value - 1;

    // Only plain or Shift-modified printable keys.
    if modifier & !(MOD_SHIFT | LOCK_MASK) != 0 {
        return None;
    }
    if modifier & (MOD_ALT | MOD_CTRL) != 0 {
        return None;
    }

    let mut effective_codepoint = codepoint;
    if modifier & MOD_SHIFT != 0 {
        if let Some(shifted_key) = shifted_key {
            effective_codepoint = shifted_key;
        }
    }
    effective_codepoint = kitty_functional_equivalent(effective_codepoint);
    if effective_codepoint < 32 {
        return None;
    }
    char::from_u32(effective_codepoint as u32).map(|c| c.to_string())
}

fn decode_modify_other_keys_printable(data: &str) -> Option<String> {
    let parsed = parse_modify_other_keys_sequence(data)?;
    let modifier = parsed.modifier & !LOCK_MASK;
    if modifier & !MOD_SHIFT != 0 {
        return None;
    }
    if parsed.codepoint < 32 {
        return None;
    }
    char::from_u32(parsed.codepoint as u32).map(|c| c.to_string())
}

pub fn decode_printable_key(data: &str) -> Option<String> {
    decode_kitty_printable(data).or_else(|| decode_modify_other_keys_printable(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_legacy_arrows() {
        assert!(matches_key("\x1b[A", &"up".to_string()));
        assert!(matches_key("\x1bOA", &"up".to_string()));
        assert!(matches_key("\x1b[B", &"down".to_string()));
        assert!(matches_key("\x1b[C", &"right".to_string()));
        assert!(matches_key("\x1b[D", &"left".to_string()));
        assert!(!matches_key("a", &"up".to_string()));
    }

    #[test]
    fn matches_shift_arrows() {
        assert!(matches_key("\x1b[a", &"shift+up".to_string()));
        assert!(matches_key("\x1b[d", &"shift+left".to_string()));
    }

    #[test]
    fn matches_ctrl_arrows() {
        assert!(matches_key("\x1bOa", &"ctrl+up".to_string()));
        assert!(matches_key("\x1b[1;5D", &"ctrl+left".to_string()));
    }

    #[test]
    fn matches_alt_arrows() {
        assert!(matches_key("\x1bb", &"alt+left".to_string()));
        assert!(matches_key("\x1bp", &"alt+up".to_string()));
        assert!(matches_key("\x1b[1;3C", &"alt+right".to_string()));
    }

    #[test]
    fn matches_plain_and_ctrl_letters() {
        assert!(matches_key("a", &"a".to_string()));
        assert!(matches_key("\x01", &"ctrl+a".to_string()));
        assert!(matches_key("\x1b[97;5u", &"ctrl+a".to_string()));
        assert!(matches_key("A", &"shift+a".to_string()));
        assert!(!matches_key("b", &"a".to_string()));
    }

    #[test]
    fn matches_ctrl_symbols() {
        assert!(matches_key("\x1c", &"ctrl+\\".to_string()));
        assert!(matches_key("\x1f", &"ctrl+-".to_string()));
        assert!(matches_key("\x1b[92;5u", &"ctrl+\\".to_string()));
    }

    #[test]
    fn matches_special_keys() {
        assert!(matches_key("\x1b", &"escape".to_string()));
        assert!(matches_key("\t", &"tab".to_string()));
        assert!(matches_key("\x1b[Z", &"shift+tab".to_string()));
        assert!(matches_key("\r", &"enter".to_string()));
        assert!(matches_key("\x1bOM", &"enter".to_string()));
        assert!(matches_key(" ", &"space".to_string()));
        assert!(matches_key("\x7f", &"backspace".to_string()));
        assert!(matches_key("\x00", &"ctrl+space".to_string()));
    }

    #[test]
    fn matches_functional_keys() {
        assert!(matches_key("\x1b[2~", &"insert".to_string()));
        assert!(matches_key("\x1b[3~", &"delete".to_string()));
        assert!(matches_key("\x1b[5~", &"pageUp".to_string()));
        assert!(matches_key("\x1b[6~", &"pageDown".to_string()));
        assert!(matches_key("\x1b[H", &"home".to_string()));
        assert!(matches_key("\x1b[F", &"end".to_string()));
        assert!(matches_key("\x1bOP", &"f1".to_string()));
        assert!(matches_key("\x1b[17~", &"f6".to_string()));
    }

    #[test]
    fn matches_kitty_csi_u() {
        assert!(matches_key("\x1b[13u", &"enter".to_string()));
        assert!(matches_key("\x1b[127u", &"backspace".to_string()));
        assert!(matches_key("\x1b[97u", &"a".to_string()));
        assert!(matches_key("\x1b[65;2u", &"shift+a".to_string()));
    }

    #[test]
    fn matches_modify_other_keys() {
        assert!(matches_key("\x1b[27;5;97~", &"ctrl+a".to_string()));
        assert!(matches_key("\x1b[27;2;65~", &"shift+a".to_string()));
    }

    #[test]
    fn parse_key_legacy() {
        assert_eq!(parse_key("\x1b[A"), Some("up".to_string()));
        assert_eq!(parse_key("\x1b[Z"), Some("shift+tab".to_string()));
        assert_eq!(parse_key("\x1bOP"), Some("f1".to_string()));
        assert_eq!(parse_key("\x01"), Some("ctrl+a".to_string()));
        assert_eq!(parse_key("x"), Some("x".to_string()));
        assert_eq!(parse_key("\x1b"), Some("escape".to_string()));
        assert_eq!(parse_key("\x1b[3~"), Some("delete".to_string()));
    }

    #[test]
    fn parse_key_kitty() {
        assert_eq!(parse_key("\x1b[97u"), Some("a".to_string()));
        assert_eq!(parse_key("\x1b[65;2u"), Some("shift+a".to_string()));
        assert_eq!(parse_key("\x1b[1;2A"), Some("shift+up".to_string()));
    }

    #[test]
    fn parse_key_alt_sequences() {
        assert_eq!(parse_key("\x1bb"), Some("alt+left".to_string()));
        assert_eq!(parse_key("\x1ba"), Some("alt+a".to_string()));
        assert_eq!(parse_key("\x1b\x01"), Some("ctrl+alt+a".to_string()));
    }

    #[test]
    fn release_and_repeat() {
        assert!(is_key_release("\x1b[97;5:3u"));
        assert!(!is_key_release("\x1b[97;5:2u"));
        assert!(is_key_repeat("\x1b[97;5:2u"));
        assert!(!is_key_repeat("\x1b[200~:2F"));
        assert!(!is_key_release("\x1b[200~:3F"));
    }

    #[test]
    fn decode_printables() {
        assert_eq!(decode_kitty_printable("\x1b[97u"), Some("a".to_string()));
        assert_eq!(decode_kitty_printable("\x1b[65;2u"), Some("A".to_string()));
        assert_eq!(decode_kitty_printable("\x1b[97;5u"), None); // ctrl rejected
        assert_eq!(decode_printable_key("\x1b[27;2;65~"), Some("A".to_string()));
        assert_eq!(decode_printable_key("plain"), None);
    }

    #[test]
    fn alt_enter_legacy_mode() {
        assert!(matches_key("\x1b\r", &"alt+enter".to_string()));
        assert_eq!(parse_key("\x1b\r"), Some("alt+enter".to_string()));
    }

    #[test]
    fn kitty_mode_changes_legacy_interpretation() {
        set_kitty_protocol_active(true);
        // \x1b\r is shift+enter under Kitty protocol.
        assert!(matches_key("\x1b\r", &"shift+enter".to_string()));
        assert_eq!(parse_key("\x1b\r"), Some("shift+enter".to_string()));
        // alt+enter no longer matches the raw legacy sequence.
        assert!(!matches_key("\x1b\r", &"alt+enter".to_string()));
        set_kitty_protocol_active(false);
        assert!(matches_key("\x1b\r", &"alt+enter".to_string()));
    }
}
