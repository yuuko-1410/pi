//! Cancellation and interruptible sleep for provider retries.
//!
//! Port of the abort/sleep helpers in `packages/ai/src/utils/retry.ts` and
//! `provider-retry.ts`. JS `AbortSignal` maps to a shared cancellation token;
//! `sleep`/`abortableSleep` are synchronous and poll the token in small
//! steps so a cancelled sleep returns promptly.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    aborted: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::Relaxed)
    }

    pub fn abort(&self) {
        self.aborted.store(true, Ordering::Relaxed);
    }

    pub fn throw_if_aborted(&self) -> Result<(), AbortError> {
        if self.is_aborted() {
            Err(AbortError)
        } else {
            Ok(())
        }
    }
}

/// Mirrors the JS `AbortError` (name "AbortError", message "Request aborted"
/// or "The operation was aborted" depending on the call site).
#[derive(Clone, Copy, Debug)]
pub struct AbortError;

impl std::fmt::Display for AbortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Request aborted")
    }
}

impl std::error::Error for AbortError {}

/// Sleeps for `ms` milliseconds, returning early with `Err(AbortError)` when
/// the token aborts. Mirrors `abortableSleep`.
pub fn abortable_sleep(ms: u64, token: Option<&CancellationToken>) -> Result<(), AbortError> {
    if let Some(token) = token {
        if token.is_aborted() {
            return Err(AbortError);
        }
    }
    // Poll in 10ms steps so cancellation is observed promptly.
    let mut remaining = ms;
    while remaining > 0 {
        std::thread::sleep(Duration::from_millis(remaining.min(10)));
        remaining = remaining.saturating_sub(10);
        if let Some(token) = token {
            if token.is_aborted() {
                return Err(AbortError);
            }
        }
    }
    Ok(())
}

/// Mirrors `sleep` for the retry policy loop; the JS version rejects with an
/// "Aborted" error that callers normalize to an aborted assistant message.
pub fn retry_sleep(ms: u64, token: Option<&CancellationToken>) -> Result<(), AbortError> {
    abortable_sleep(ms, token)
}
