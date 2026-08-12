//! Bash execution component, port of `components/bash-execution.ts`.
//!
//! ponytail: the JS Loader runs on a timer; the Rust version renders the
//! running state statically (host advances frames when wired up). The
//! content is rebuilt on every render instead of caching.

use pi_tui::tui::Component;
use pi_tui::utils::strip_terminal_sequences;

use crate::core::tools::truncate::{truncate_tail, TruncationOptions, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};
use crate::modes::interactive::components::keybinding_hints::{key_hint, key_text};
use crate::modes::interactive::theme::theme::theme;

// Preview line limit when not expanded (matches tool execution behavior).
const PREVIEW_LINES: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BashStatus {
    Running,
    Complete,
    Cancelled,
    Error,
}

pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
}

/// Component for displaying bash command execution with streaming output.
pub struct BashExecutionComponent {
    command: String,
    output_lines: Vec<String>,
    status: BashStatus,
    exit_code: Option<i64>,
    truncation_result: Option<TruncationResult>,
    full_output_path: Option<String>,
    expanded: bool,
    exclude_from_context: bool,
}

impl BashExecutionComponent {
    pub fn new(command: &str, exclude_from_context: bool) -> Self {
        Self {
            command: command.to_string(),
            output_lines: Vec::new(),
            status: BashStatus::Running,
            exit_code: None,
            truncation_result: None,
            full_output_path: None,
            expanded: false,
            exclude_from_context,
        }
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    pub fn append_output(&mut self, chunk: &str) {
        let clean = strip_terminal_sequences(chunk)
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        let new_lines: Vec<String> = clean.split('\n').map(|s| s.to_string()).collect();
        if !self.output_lines.is_empty() && !new_lines.is_empty() {
            let last = self.output_lines.len() - 1;
            let first = new_lines[0].clone();
            self.output_lines[last].push_str(&first);
            self.output_lines.extend(new_lines[1..].iter().cloned());
        } else {
            self.output_lines.extend(new_lines);
        }
    }

    pub fn set_complete(
        &mut self,
        exit_code: Option<i64>,
        cancelled: bool,
        truncation_result: Option<TruncationResult>,
        full_output_path: Option<String>,
    ) {
        self.exit_code = exit_code;
        self.status = if cancelled {
            BashStatus::Cancelled
        } else if exit_code.is_some_and(|code| code != 0) {
            BashStatus::Error
        } else {
            BashStatus::Complete
        };
        self.truncation_result = truncation_result;
        self.full_output_path = full_output_path;
    }

    /// Get the raw output.
    pub fn get_output(&self) -> String {
        self.output_lines.join("\n")
    }

    /// Get the command.
    pub fn get_command(&self) -> &str {
        &self.command
    }
}

impl Component for BashExecutionComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        let color_key = if self.exclude_from_context { "dim" } else { "bashMode" };
        let colored = |text: &str| {
            t.map(|t| t.fg(color_key, text))
                .unwrap_or_else(|| text.to_string())
        };
        let muted = |text: &str| {
            t.map(|t| t.fg("muted", text))
                .unwrap_or_else(|| text.to_string())
        };
        let border = colored(&"─".repeat(width.max(1)));

        let mut lines: Vec<String> = Vec::new();
        lines.push(String::new());
        lines.push(border.clone());

        // Command header.
        let header = colored(&format!("$ {}", self.command));
        lines.push(format!(" {}", header));

        // Output.
        let full_output = self.output_lines.join("\n");
        let context_truncation = truncate_tail(
            &full_output,
            TruncationOptions {
                max_lines: Some(DEFAULT_MAX_LINES),
                max_bytes: Some(DEFAULT_MAX_BYTES),
            },
        );
        let available_lines: Vec<String> = if context_truncation.content.is_empty() {
            Vec::new()
        } else {
            context_truncation.content.split('\n').map(|s| s.to_string()).collect()
        };
        let preview_start = available_lines.len().saturating_sub(PREVIEW_LINES);
        let preview_lines: Vec<String> = available_lines[preview_start..].to_vec();
        let hidden_line_count = available_lines.len() - preview_lines.len();

        if !available_lines.is_empty() {
            let shown: Vec<String> = if self.expanded {
                available_lines.iter().map(|line| muted(line)).collect()
            } else {
                preview_lines.iter().map(|line| muted(line)).collect()
            };
            for line in shown {
                lines.push(format!(" {line}"));
            }
        }

        // Status.
        if self.status == BashStatus::Running {
            let cancel_hint = key_text("tui.select.cancel");
            lines.push(format!("  {}Running... ({cancel_hint} to cancel)", colored("")));
        } else {
            let mut status_parts: Vec<String> = Vec::new();

            if hidden_line_count > 0 {
                let hint = key_hint("app.tools.expand", if self.expanded { "to collapse" } else { "to expand" });
                if self.expanded {
                    status_parts.push(format!("({hint})"));
                } else {
                    status_parts.push(format!("... {hidden_line_count} more lines ({hint})"));
                }
            }

            if self.status == BashStatus::Cancelled {
                status_parts.push(t.map(|t| t.fg("warning", "(cancelled)")).unwrap_or_else(|| "(cancelled)".to_string()));
            } else if self.status == BashStatus::Error {
                let code = self.exit_code.unwrap_or(0);
                status_parts.push(t.map(|t| t.fg("error", &format!("(exit {code})"))).unwrap_or_else(|| format!("(exit {code})")));
            }

            let was_truncated = self.truncation_result.as_ref().is_some_and(|r| r.truncated)
                || context_truncation.truncated;
            if was_truncated {
                if let Some(path) = &self.full_output_path {
                    status_parts.push(
                        t.map(|t| t.fg("warning", &format!("Output truncated. Full output: {path}")))
                            .unwrap_or_else(|| format!("Output truncated. Full output: {path}")),
                    );
                }
            }

            if !status_parts.is_empty() {
                lines.push(format!(" {}", status_parts.join(" ")));
            }
        }

        lines.push(border);
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_command_and_output() {
        let mut component = BashExecutionComponent::new("ls -la", false);
        component.append_output("total 8\nfile1\nfile2");
        component.set_complete(Some(0), false, None, None);
        let lines = component.render(60);
        assert!(lines.iter().any(|line| line.contains("ls -la")));
        assert!(lines.iter().any(|line| line.contains("file1")));
        assert_eq!(component.get_output(), "total 8\nfile1\nfile2");
    }

    #[test]
    fn cancelled_status_shown() {
        let mut component = BashExecutionComponent::new("sleep 1", false);
        component.append_output("x");
        component.set_complete(None, true, None, None);
        let lines = component.render(60);
        assert!(lines.iter().any(|line| line.contains("cancelled")));
        assert_eq!(component.status, BashStatus::Cancelled);
    }

    #[test]
    fn error_exit_code_shown() {
        let mut component = BashExecutionComponent::new("false", false);
        component.set_complete(Some(1), false, None, None);
        let lines = component.render(60);
        assert!(lines.iter().any(|line| line.contains("exit 1")));
    }
}
