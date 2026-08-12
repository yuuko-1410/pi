//! Ring buffer for Emacs-style kill/yank operations, port of
//! `packages/tui/src/kill-ring.ts`.

pub struct KillRing {
    ring: Vec<String>,
}

impl KillRing {
    pub fn new() -> Self {
        Self { ring: Vec::new() }
    }

    /// Add text to the kill ring. When accumulating, merges with the most
    /// recent entry (prepend for backward deletion, append otherwise).
    pub fn push(&mut self, text: &str, prepend: bool, accumulate: Option<bool>) {
        if text.is_empty() {
            return;
        }
        if accumulate.unwrap_or(false) && !self.ring.is_empty() {
            let last = self.ring.pop().unwrap();
            if prepend {
                self.ring.push(format!("{text}{last}"));
            } else {
                self.ring.push(format!("{last}{text}"));
            }
        } else {
            self.ring.push(text.to_string());
        }
    }

    /// Get the most recent entry without modifying the ring.
    pub fn peek(&self) -> Option<&str> {
        self.ring.last().map(|entry| entry.as_str())
    }

    /// Move the last entry to the front (for yank-pop cycling).
    pub fn rotate(&mut self) {
        if self.ring.len() > 1 {
            let last = self.ring.pop().unwrap();
            self.ring.insert(0, last);
        }
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_peek() {
        let mut ring = KillRing::new();
        assert_eq!(ring.peek(), None);
        ring.push("first", false, None);
        ring.push("second", false, None);
        assert_eq!(ring.peek(), Some("second"));
        assert_eq!(ring.len(), 2);
    }

    #[test]
    fn empty_push_is_ignored() {
        let mut ring = KillRing::new();
        ring.push("", false, None);
        assert!(ring.is_empty());
    }

    #[test]
    fn accumulate_appends_by_default() {
        let mut ring = KillRing::new();
        ring.push("hello", false, None);
        ring.push(" world", false, Some(true));
        assert_eq!(ring.peek(), Some("hello world"));
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn accumulate_prepend() {
        let mut ring = KillRing::new();
        ring.push("world", false, None);
        ring.push("hello ", true, Some(true));
        assert_eq!(ring.peek(), Some("hello world"));
    }

    #[test]
    fn rotate_cycles() {
        let mut ring = KillRing::new();
        ring.push("one", false, None);
        ring.push("two", false, None);
        ring.push("three", false, None);
        assert_eq!(ring.peek(), Some("three"));
        ring.rotate();
        assert_eq!(ring.peek(), Some("two"));
        ring.rotate();
        assert_eq!(ring.peek(), Some("one"));
        ring.rotate();
        assert_eq!(ring.peek(), Some("three"));
    }

    #[test]
    fn rotate_single_is_noop() {
        let mut ring = KillRing::new();
        ring.push("only", false, None);
        ring.rotate();
        assert_eq!(ring.peek(), Some("only"));
    }
}
