//! Native modifier key detection, port of `packages/tui/src/native-modifiers.ts`.
//!
//! The JS implementation loads a platform-specific `.node` native addon to
//! query physical modifier state. Rust has no equivalent, so
//! `is_native_modifier_pressed` always returns false; callers should track
//! modifiers from the terminal input stream instead.

pub type ModifierKey = &'static str; // "shift" | "command" | "control" | "option"

pub fn is_native_modifier_pressed(_key: ModifierKey) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_modifiers_unavailable() {
        assert!(!is_native_modifier_pressed("shift"));
        assert!(!is_native_modifier_pressed("command"));
        assert!(!is_native_modifier_pressed("control"));
        assert!(!is_native_modifier_pressed("option"));
    }
}
