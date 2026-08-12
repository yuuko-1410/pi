//! Footer component, port of `components/footer.ts`.

use std::sync::Arc;

use pi_tui::tui::Component;
use pi_tui::utils::{truncate_to_width, visible_width};

use crate::core::agent_session::AgentSession;
use crate::core::experimental::are_experimental_features_enabled;
use crate::core::footer_data_provider::FooterDataProvider;
use crate::core::session_types::SessionEntry;
use crate::core::usage_totals::{add_usage_to_totals, UsageTotals};
use crate::modes::interactive::theme::theme::theme;

/// Sanitize text for display in a single-line status.
fn sanitize_status_text(text: &str) -> String {
    let replaced = text.replace(['\r', '\n', '\t'], " ");
    let mut result = String::new();
    let mut prev_space = false;
    for ch in replaced.chars() {
        if ch == ' ' {
            if prev_space {
                continue;
            }
            prev_space = true;
        } else {
            prev_space = false;
        }
        result.push(ch);
    }
    result.trim().to_string()
}

/// Format token counts for compact footer display.
pub fn format_tokens(count: f64) -> String {
    if count < 1000.0 {
        return format!("{}", count as i64);
    }
    if count < 10000.0 {
        return format!("{:.1}k", count / 1000.0);
    }
    if count < 1_000_000.0 {
        return format!("{}k", (count / 1000.0).round() as i64);
    }
    if count < 10_000_000.0 {
        return format!("{:.1}M", count / 1_000_000.0);
    }
    format!("{}M", (count / 1_000_000.0).round() as i64)
}

/// Replace home directory with ~ in a cwd.
pub fn format_cwd_for_footer(cwd: &str, home: Option<&str>) -> String {
    let Some(home) = home else {
        return cwd.to_string();
    };
    let home = home.trim_end_matches('/');
    if cwd == home {
        return "~".to_string();
    }
    if let Some(rest) = cwd.strip_prefix(home) {
        if rest.starts_with('/') {
            return format!("~{}", rest);
        }
    }
    cwd.to_string()
}

/// Footer component that shows pwd, token stats, and context usage.
pub struct FooterComponent {
    session: Arc<AgentSession>,
    footer_data: std::sync::Mutex<FooterDataProvider>,
    auto_compact_enabled: bool,
}

impl FooterComponent {
    pub fn new(session: Arc<AgentSession>, footer_data: FooterDataProvider) -> Self {
        Self {
            session,
            footer_data: std::sync::Mutex::new(footer_data),
            auto_compact_enabled: true,
        }
    }

    pub fn set_session(&mut self, session: Arc<AgentSession>) {
        self.session = session;
    }

    pub fn set_auto_compact_enabled(&mut self, enabled: bool) {
        self.auto_compact_enabled = enabled;
    }

    fn render_impl(&self, width: usize) -> Vec<String> {
        let state = self.session.state();

        // Cumulative usage from all session entries.
        let mut usage_totals = UsageTotals::default();
        let mut latest_cache_hit_rate: Option<f64> = None;

        for entry in self.session.session_manager.lock().unwrap().get_entries() {
            match &entry {
                SessionEntry::Message { message, .. } => match message {
                    crate::core::session_types::SessionMessage::Llm(message) => {
                        let usage = match message {
                            pi_ai::types::Message::Assistant(message) => Some(&message.usage),
                            _ => None,
                        };
                        if let Some(usage) = usage {
                            add_usage_to_totals(&mut usage_totals, usage);
                            let latest_prompt_tokens = usage.input + usage.cache_read + usage.cache_write;
                            latest_cache_hit_rate = if latest_prompt_tokens > 0.0 {
                                Some((usage.cache_read / latest_prompt_tokens) * 100.0)
                            } else {
                                None
                            };
                        }
                    }
                    crate::core::session_types::SessionMessage::Unknown(_) => {}
                    _ => {}
                },
                SessionEntry::BranchSummary { .. } | SessionEntry::Compaction { .. } => {}
                _ => {}
            }
        }

        let has_model = !state.model.provider.is_empty() && state.model.provider != "unknown";
        let context_window = if has_model { state.model.context_window } else { 0.0 };
        let context_percent_value = 0.0; // ponytail: context usage estimate not exposed; shows 0%/window
        let context_percent_display = format!(
            "{:.1}%/{}{}",
            context_percent_value,
            format_tokens(context_window),
            if self.auto_compact_enabled { " (auto)" } else { "" }
        );

        let mut pwd = format_cwd_for_footer(
            self.session.session_manager.lock().unwrap().get_cwd(),
            std::env::var("HOME").ok().as_deref(),
        );

        if let Some(branch) = self.footer_data.lock().unwrap().get_git_branch() {
            pwd = format!("{pwd} ({branch})");
        }
        if let Some(session_name) = self.session.get_session_name() {
            pwd = format!("{pwd} • {session_name}");
        }

        let mut stats_parts: Vec<String> = Vec::new();
        if usage_totals.input > 0.0 {
            stats_parts.push(format!("↑{}", format_tokens(usage_totals.input)));
        }
        if usage_totals.output > 0.0 {
            stats_parts.push(format!("↓{}", format_tokens(usage_totals.output)));
        }
        if usage_totals.cache_read > 0.0 {
            stats_parts.push(format!("R{}", format_tokens(usage_totals.cache_read)));
        }
        if usage_totals.cache_write > 0.0 {
            stats_parts.push(format!("W{}", format_tokens(usage_totals.cache_write)));
        }
        if usage_totals.cache_read > 0.0 || usage_totals.cache_write > 0.0 {
            if let Some(hit_rate) = latest_cache_hit_rate {
                stats_parts.push(format!("CH{hit_rate:.1}%"));
            }
        }

        let t = theme();
        let t = t.as_ref();

        // Kimi Coding is subscription-backed despite using API-key auth.
        let using_subscription = has_model && state.model.provider == "kimi-coding";
        if usage_totals.cost > 0.0 || using_subscription {
            let cost_str = format!("${:.3}{}", usage_totals.cost, if using_subscription { " (sub)" } else { "" });
            stats_parts.push(cost_str);
        }

        let context_percent_str = if context_percent_value > 90.0 {
            t.map(|t| t.fg("error", &context_percent_display))
                .unwrap_or(context_percent_display.clone())
        } else if context_percent_value > 70.0 {
            t.map(|t| t.fg("warning", &context_percent_display))
                .unwrap_or(context_percent_display.clone())
        } else {
            context_percent_display.clone()
        };
        stats_parts.push(context_percent_str);

        if are_experimental_features_enabled() {
            let bullet = t.map(|t| t.fg("dim", "•")).unwrap_or_else(|| "•".to_string());
            let xp = t
                .map(|t| t.bold(&t.fg("warning", "xp")))
                .unwrap_or_else(|| "xp".to_string());
            stats_parts.push(format!("{bullet} {xp}"));
        }

        let stats_left = stats_parts.join(" ");
        let mut stats_left_width = visible_width(&stats_left) as usize;

        let stats_left = if stats_left_width > width {
            let truncated = truncate_to_width(&stats_left, width as f64, "...", false);
            stats_left_width = visible_width(&truncated) as usize;
            truncated
        } else {
            stats_left
        };

        let model_name = if has_model { state.model.id.clone() } else { "no-model".to_string() };
        let mut right_side_without_provider = model_name.clone();
        if has_model && state.model.reasoning {
            let thinking_level = if state.thinking_level.is_empty() {
                "off".to_string()
            } else {
                state.thinking_level.clone()
            };
            right_side_without_provider = if thinking_level == "off" {
                format!("{model_name} • thinking off")
            } else {
                format!("{model_name} • {thinking_level}")
            };
        }

        let mut right_side = right_side_without_provider.clone();
        if self.footer_data.lock().unwrap().get_available_provider_count() > 1 && has_model {
            let provider = state.model.provider.clone();
            let candidate = format!("({provider}) {right_side_without_provider}");
            if stats_left_width + 2 + visible_width(&candidate) as usize <= width {
                right_side = candidate;
            }
        }

        let right_side_width = visible_width(&right_side) as usize;
        let total_needed = stats_left_width + 2 + right_side_width;

        let stats_line = if total_needed <= width {
            let padding = " ".repeat(width - stats_left_width - right_side_width);
            format!("{stats_left}{padding}{right_side}")
        } else {
            let available_for_right = width as isize - stats_left_width as isize - 2;
            if available_for_right > 0 {
                let truncated_right = truncate_to_width(&right_side, available_for_right as f64, "", false);
                let truncated_right_width = visible_width(&truncated_right) as usize;
                let padding = " ".repeat(width.saturating_sub(stats_left_width + truncated_right_width));
                format!("{stats_left}{padding}{truncated_right}")
            } else {
                stats_left.clone()
            }
        };

        // Apply dim to each part separately (stats_left may contain color codes).
        let dim = |text: &str| {
            t.map(|t| t.fg("dim", text)).unwrap_or_else(|| text.to_string())
        };
        let dim_stats_left = dim(&stats_left);
        let remainder = &stats_line[stats_left.len()..];
        let dim_remainder = dim(remainder);

        let pwd_line = truncate_to_width(&dim(&pwd), width as f64, &dim("..."), false);
        let mut lines = vec![pwd_line, format!("{dim_stats_left}{dim_remainder}")];

        let extension_statuses: Vec<(String, String)> = {
            let guard = self.footer_data.lock().unwrap();
            guard.get_extension_statuses().iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        if !extension_statuses.is_empty() {
            let mut sorted = extension_statuses.clone();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            let status_line = sorted
                .iter()
                .map(|(_, text)| sanitize_status_text(text))
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(truncate_to_width(&status_line, width as f64, &dim("..."), false));
        }

        lines
    }
}

impl Component for FooterComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.render_impl(width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_token_counts() {
        assert_eq!(format_tokens(999.0), "999");
        assert_eq!(format_tokens(1500.0), "1.5k");
        assert_eq!(format_tokens(12345.0), "12k");
        assert_eq!(format_tokens(1_500_000.0), "1.5M");
        assert_eq!(format_tokens(12_345_678.0), "12M");
    }

    #[test]
    fn formats_cwd_with_home() {
        assert_eq!(format_cwd_for_footer("/home/user", Some("/home/user")), "~");
        assert_eq!(format_cwd_for_footer("/home/user/proj", Some("/home/user")), "~/proj");
        assert_eq!(format_cwd_for_footer("/other", Some("/home/user")), "/other");
    }

    #[test]
    fn sanitizes_status_text() {
        assert_eq!(sanitize_status_text("a\nb\t c"), "a b c");
        assert_eq!(sanitize_status_text("  spaced  out  "), "spaced out");
    }
}
