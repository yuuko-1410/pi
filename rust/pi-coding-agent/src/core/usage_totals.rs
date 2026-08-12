//! Usage totals, port of `core/usage-totals.ts`.

use pi_ai::types::Usage;

use super::session_types::{SessionEntry, SessionMessage};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageTotals {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub cost: f64,
}

pub fn add_usage_to_totals(totals: &mut UsageTotals, usage: &Usage) {
    totals.input += usage.input;
    totals.output += usage.output;
    totals.cache_read += usage.cache_read;
    totals.cache_write += usage.cache_write;
    totals.cost += usage.cost.total;
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageCostBreakdownEntry {
    pub key: String,
    pub cost: f64,
    pub tokens: f64,
}

/// Group attributable assistant usage by model and all other usage into a
/// separate bucket.
pub fn get_usage_cost_breakdown(entries: &[SessionEntry]) -> Vec<UsageCostBreakdownEntry> {
    let mut totals_by_key: Vec<(String, UsageTotals)> = Vec::new();

    for entry in entries {
        let mut key: Option<String> = None;
        let mut usage: Option<Usage> = None;
        match entry {
            SessionEntry::Message {
                message: SessionMessage::Llm(pi_ai::types::Message::Assistant(assistant)),
                ..
            } => {
                key = Some(format!(
                    "{}/{}",
                    assistant.provider,
                    assistant.response_model.as_deref().unwrap_or(&assistant.model)
                ));
                usage = Some(assistant.usage.clone());
            }
            SessionEntry::Message {
                message:
                    SessionMessage::Llm(pi_ai::types::Message::ToolResult(tool_result)),
                ..
            } if tool_result.usage.is_some() => {
                key = Some("Tools/summaries".to_string());
                usage = tool_result.usage.clone();
            }
            SessionEntry::BranchSummary { usage: entry_usage, .. } | SessionEntry::Compaction { usage: entry_usage, .. }
                if entry_usage.is_some() =>
            {
                key = Some("Tools/summaries".to_string());
                usage = entry_usage.clone();
            }
            _ => {}
        }
        let (Some(key), Some(usage)) = (key, usage) else {
            continue;
        };

        match totals_by_key.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, totals)) => add_usage_to_totals(totals, &usage),
            None => {
                let mut totals = UsageTotals::default();
                add_usage_to_totals(&mut totals, &usage);
                totals_by_key.push((key, totals));
            }
        }
    }

    let mut breakdown: Vec<UsageCostBreakdownEntry> = totals_by_key
        .into_iter()
        .map(|(key, totals)| UsageCostBreakdownEntry {
            key,
            cost: totals.cost,
            tokens: totals.input + totals.output + totals.cache_read + totals.cache_write,
        })
        .filter(|entry| entry.cost > 0.0 || entry.tokens > 0.0)
        .collect();
    breakdown.sort_by(|a, b| b.cost.partial_cmp(&a.cost).unwrap_or(std::cmp::Ordering::Equal));
    breakdown
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::{AssistantMessage, UsageCost};

    fn usage(input: f64, cost_total: f64) -> Usage {
        Usage {
            input,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: input,
            cost: UsageCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: cost_total,
            },
        }
    }

    fn base(id: &str) -> super::super::session_types::SessionEntryBase {
        super::super::session_types::SessionEntryBase {
            id: id.to_string(),
            parent_id: None,
            timestamp: String::new(),
        }
    }

    fn assistant(provider: &str, model: &str, input: f64, cost: f64) -> SessionEntry {
        SessionEntry::Message {
            base: base("m"),
            message: SessionMessage::Llm(pi_ai::types::Message::Assistant(AssistantMessage {
                content: vec![],
                api: "api".into(),
                provider: provider.to_string(),
                model: model.to_string(),
                response_model: None,
                response_id: None,
                usage: usage(input, cost),
                stop_reason: pi_ai::types::StopReason::Stop,
                deferred: None,
                error_message: None,
                raw_stop_reason: None,
                end_turn: None,
                timestamp: 0.0,
            })),
        }
    }

    #[test]
    fn groups_by_model() {
        let entries = vec![
            assistant("anthropic", "claude", 100.0, 10.0),
            assistant("openai", "gpt", 200.0, 20.0),
            assistant("anthropic", "claude", 50.0, 5.0),
        ];
        let breakdown = get_usage_cost_breakdown(&entries);
        assert_eq!(breakdown.len(), 2);
        // Sorted by cost descending: openai/gpt (20) before anthropic/claude (15).
        assert_eq!(breakdown[0].key, "openai/gpt");
        assert_eq!(breakdown[0].cost, 20.0);
        assert_eq!(breakdown[0].tokens, 200.0);
        assert_eq!(breakdown[1].key, "anthropic/claude");
        assert_eq!(breakdown[1].cost, 15.0);
        assert_eq!(breakdown[1].tokens, 150.0);
    }

    #[test]
    fn empty_entries() {
        assert!(get_usage_cost_breakdown(&[]).is_empty());
    }

    #[test]
    fn response_model_overrides_model() {
        let mut entry = assistant("anthropic", "claude", 100.0, 10.0);
        if let SessionEntry::Message {
            message: SessionMessage::Llm(pi_ai::types::Message::Assistant(assistant)),
            ..
        } = &mut entry
        {
            assistant.response_model = Some("claude-sonnet".into());
        }
        let breakdown = get_usage_cost_breakdown(&[entry]);
        assert_eq!(breakdown[0].key, "anthropic/claude-sonnet");
    }
}
