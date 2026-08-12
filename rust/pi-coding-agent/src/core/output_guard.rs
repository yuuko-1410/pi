//! Stdout takeover, port of `core/output-guard.ts`.
//!
//! JS swaps `process.stdout.write` to stderr so TUI rendering owns stdout.
//! Rust has no swappable global writer; instead the TUI layer owns stdout and
//! every other module writes via `write_raw_stdout`, which bypasses the TUI
//! renderer by writing to stdout directly (line-buffered in raw mode, so no
//! interleaving with the render loop's cursor-positioned writes).
//! ponytail: no ENOBUFS retry loop — stdout writes on a local pipe block
//! synchronously; add a worker-thread queue only if a pipe ever signals
//! EAGAIN in practice.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

static TAKEN_OVER: AtomicBool = AtomicBool::new(false);

pub fn take_over_stdout() {
    TAKEN_OVER.store(true, Ordering::SeqCst);
}

pub fn restore_stdout() {
    TAKEN_OVER.store(false, Ordering::SeqCst);
}

pub fn is_stdout_taken_over() -> bool {
    TAKEN_OVER.load(Ordering::SeqCst)
}

/// Write directly to the raw terminal stdout, bypassing any TUI buffering.
pub fn write_raw_stdout(text: &str) {
    if text.is_empty() {
        return;
    }
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

pub fn flush_raw_stdout() {
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takeover_flag() {
        take_over_stdout();
        assert!(is_stdout_taken_over());
        restore_stdout();
        assert!(!is_stdout_taken_over());
        write_raw_stdout("");
        write_raw_stdout("x");
        flush_raw_stdout();
    }
}
