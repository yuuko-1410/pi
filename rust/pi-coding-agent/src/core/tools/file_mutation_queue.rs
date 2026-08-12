//! File mutation queue, port of `tools/file-mutation-queue.ts`.
//!
//! JS serializes mutations per canonical path across async callers. The
//! synchronous runtime executes callers serially on one thread, so a queue
//! would be a no-op; this module keeps the API shape and documents the
//! simplification.

/// Serialize file mutation operations targeting the same file. In the
/// synchronous runtime this executes the closure directly.
pub fn with_file_mutation_queue<T>(_file_path: &str, operation: impl FnOnce() -> T) -> T {
    operation()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executes_operation() {
        let result = with_file_mutation_queue("/tmp/x", || 42);
        assert_eq!(result, 42);
    }
}
