//! Status indicators, port of `components/status-indicator.ts`.
//!
//! ponytail: the JS Loader spins on a setInterval. Here the host drives
//! `advance_frame()` from its event loop; retry countdowns use
//! CountdownTimer::tick().

use pi_tui::components::loader::{Loader, LoaderIndicatorOptions};
use pi_tui::tui::Component;

use crate::modes::interactive::components::countdown_timer::CountdownTimer;
use crate::modes::interactive::components::keybinding_hints::key_text;
use crate::modes::interactive::theme::theme::theme;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StatusIndicatorKind {
    Working,
    Retry,
    Compaction,
    BranchSummary,
}

pub struct WorkingIndicatorOptions {
    pub frames: Option<Vec<String>>,
    pub interval_ms: Option<f64>,
}

fn color_fn(color: &str) -> std::sync::Arc<dyn Fn(&str) -> String + Send + Sync> {
    // Resolve the ANSI code eagerly so the closure owns only a String.
    let ansi = theme()
        .as_ref()
        .map(|t| t.get_fg_ansi(color))
        .unwrap_or_default();
    if ansi.is_empty() {
        std::sync::Arc::new(|text: &str| text.to_string())
    } else {
        std::sync::Arc::new(move |text: &str| format!("{ansi}{text}\x1b[39m"))
    }
}

pub struct StatusIndicator {
    loader: Loader,
    pub kind: StatusIndicatorKind,
}

impl StatusIndicator {
    pub fn new(
        kind: StatusIndicatorKind,
        spinner_color: &str,
        message_color: &str,
        message: &str,
        indicator: Option<&WorkingIndicatorOptions>,
        request_render: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let loader_indicator = indicator.map(|options| LoaderIndicatorOptions {
            frames: options.frames.clone(),
            interval_ms: options.interval_ms,
        });
        let loader = Loader::new(
            color_fn(spinner_color),
            color_fn(message_color),
            message,
            loader_indicator.as_ref(),
            request_render,
        );
        Self { loader, kind }
    }

    pub fn set_message(&mut self, message: &str) {
        self.loader.set_message(message);
    }

    /// Advance the spinner animation frame (host-driven).
    pub fn advance_frame(&mut self) {
        self.loader.advance_frame();
    }
}

impl Component for StatusIndicator {
    fn render(&self, width: usize) -> Vec<String> {
        self.loader.render(width)
    }
}

pub struct WorkingStatusIndicator {
    inner: StatusIndicator,
}

impl WorkingStatusIndicator {
    pub fn new(message: &str, indicator: Option<&WorkingIndicatorOptions>) -> Self {
        Self {
            inner: StatusIndicator::new(
                StatusIndicatorKind::Working,
                "accent",
                "muted",
                message,
                indicator,
                std::sync::Arc::new(|| {}),
            ),
        }
    }
}

impl Component for WorkingStatusIndicator {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.render(width)
    }
}

pub struct RetryStatusIndicator {
    inner: StatusIndicator,
    countdown: Option<CountdownTimer>,
    seconds: std::sync::Arc<std::sync::Mutex<i64>>,
    attempt: i64,
    max_attempts: i64,
}

impl RetryStatusIndicator {
    pub fn new(attempt: i64, max_attempts: i64, delay_ms: f64) -> Self {
        let seconds = std::sync::Arc::new(std::sync::Mutex::new((delay_ms / 1000.0).ceil() as i64));
        let seconds_clone = seconds.clone();
        let countdown = CountdownTimer::new(
            delay_ms,
            Box::new(move |value| {
                *seconds_clone.lock().unwrap() = value;
            }),
            Box::new(|| {}),
        );
        let mut indicator = Self {
            inner: StatusIndicator::new(
                StatusIndicatorKind::Retry,
                "warning",
                "muted",
                "",
                None,
                std::sync::Arc::new(|| {}),
            ),
            countdown: Some(countdown),
            seconds,
            attempt,
            max_attempts,
        };
        indicator.update_message();
        indicator
    }

    fn update_message(&mut self) {
        let seconds = *self.seconds.lock().unwrap();
        let message = format!(
            "Retrying ({}/{}) in {seconds}s... ({} to cancel)",
            self.attempt,
            self.max_attempts,
            key_text("app.interrupt")
        );
        self.inner.set_message(&message);
    }

    /// Advance the retry countdown by one second.
    pub fn tick(&mut self) {
        if let Some(countdown) = &mut self.countdown {
            if countdown.tick() {
                self.countdown = None;
            }
        }
        self.update_message();
    }
}

impl Component for RetryStatusIndicator {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.render(width)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CompactionStatusReason {
    Manual,
    Threshold,
    Overflow,
}

pub struct CompactionStatusIndicator {
    inner: StatusIndicator,
}

impl CompactionStatusIndicator {
    pub fn new(reason: CompactionStatusReason) -> Self {
        let cancel_hint = format!("({} to cancel)", key_text("app.interrupt"));
        let label = match reason {
            CompactionStatusReason::Manual => format!("Compacting context... {cancel_hint}"),
            CompactionStatusReason::Threshold => format!("Auto-compacting... {cancel_hint}"),
            CompactionStatusReason::Overflow => {
                format!("Context overflow detected, Auto-compacting... {cancel_hint}")
            }
        };
        Self {
            inner: StatusIndicator::new(
                StatusIndicatorKind::Compaction,
                "accent",
                "muted",
                &label,
                None,
                std::sync::Arc::new(|| {}),
            ),
        }
    }
}

impl Component for CompactionStatusIndicator {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.render(width)
    }
}

pub struct BranchSummaryStatusIndicator {
    inner: StatusIndicator,
}

impl BranchSummaryStatusIndicator {
    pub fn new() -> Self {
        Self {
            inner: StatusIndicator::new(
                StatusIndicatorKind::BranchSummary,
                "accent",
                "muted",
                &format!("Summarizing branch... ({} to cancel)", key_text("app.interrupt")),
                None,
                std::sync::Arc::new(|| {}),
            ),
        }
    }
}

impl Component for BranchSummaryStatusIndicator {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.render(width)
    }
}

pub struct IdleStatus;

impl Component for IdleStatus {
    fn render(&self, width: usize) -> Vec<String> {
        let empty_line = " ".repeat(width);
        vec![empty_line.clone(), empty_line]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_status_renders_two_lines() {
        let idle = IdleStatus;
        assert_eq!(idle.render(10).len(), 2);
        assert_eq!(idle.render(10)[0].len(), 10);
    }

    #[test]
    fn working_indicator_renders() {
        let indicator = WorkingStatusIndicator::new("working...", None);
        assert!(!indicator.render(40).is_empty());
    }

    #[test]
    fn compaction_reason_labels() {
        let indicator = CompactionStatusIndicator::new(CompactionStatusReason::Overflow);
        assert!(!indicator.render(40).is_empty());
    }

    #[test]
    fn retry_tick_expires() {
        let mut indicator = RetryStatusIndicator::new(1, 3, 2000.0);
        assert!(!indicator.render(60).is_empty());
        indicator.tick();
        indicator.tick();
        // After expiry the countdown is cleared; further ticks are no-ops.
        indicator.tick();
    }
}
