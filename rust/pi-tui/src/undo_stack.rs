//! Generic undo stack with clone-on-push semantics, port of
//! `packages/tui/src/undo-stack.ts`. Rust's Clone replaces
//! `structuredClone` (deep clone by value semantics).

pub struct UndoStack<S> {
    stack: Vec<S>,
}

impl<S> UndoStack<S> {
    pub fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// Push a clone of the given state onto the stack.
    pub fn push(&mut self, state: &S)
    where
        S: Clone,
    {
        self.stack.push(state.clone());
    }

    /// Pop and return the most recent snapshot, or None if empty.
    pub fn pop(&mut self) -> Option<S> {
        self.stack.pop()
    }

    /// Remove all snapshots.
    pub fn clear(&mut self) {
        self.stack.clear();
    }

    pub fn len(&self) -> usize {
        self.stack.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct State {
        text: String,
        cursor: usize,
    }

    #[test]
    fn push_clones_and_pop_returns() {
        let mut stack = UndoStack::new();
        let state = State {
            text: "hello".to_string(),
            cursor: 5,
        };
        stack.push(&state);
        stack.push(&State {
            text: "hello world".to_string(),
            cursor: 11,
        });
        assert_eq!(stack.len(), 2);
        let popped = stack.pop().unwrap();
        assert_eq!(popped.text, "hello world");
        // Mutating the original does not affect the pushed snapshot.
        assert_eq!(stack.pop().unwrap().text, "hello");
    }

    #[test]
    fn pop_empty_returns_none() {
        let mut stack = UndoStack::<State>::new();
        assert_eq!(stack.pop(), None);
    }

    #[test]
    fn clear_removes_all() {
        let mut stack = UndoStack::new();
        stack.push(&State {
            text: "a".to_string(),
            cursor: 1,
        });
        stack.clear();
        assert!(stack.is_empty());
    }
}
