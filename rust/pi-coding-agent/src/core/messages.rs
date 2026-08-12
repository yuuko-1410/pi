//! Custom message types and transformers, port of `core/messages.ts`.
//!
//! JS declares custom roles on AgentMessage via declaration merging; the Rust
//! analog is concrete structs implementing `CustomAgentMessage`. JSON round
//! trips for these live in `session_types.rs` (they appear inside session
//! file message entries).

use pi_agent_core::types::CustomAgentMessage;
use pi_ai::types::{Content, Message, TextContent, UserMessage, UserMessageContent};

pub const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:

<summary>
";

pub const COMPACTION_SUMMARY_SUFFIX: &str = "
</summary>";

pub const BRANCH_SUMMARY_PREFIX: &str = "The following is a summary of a branch that this conversation came back from:

<summary>
";

pub const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

/// Message type for bash executions via the `!` command.
#[derive(Clone, Debug, Default)]
pub struct BashExecutionMessage {
    pub command: String,
    pub output: String,
    pub exit_code: Option<i64>,
    pub cancelled: bool,
    pub truncated: bool,
    pub full_output_path: Option<String>,
    pub timestamp: f64,
    /// If true, this message is excluded from LLM context (`!!` prefix).
    pub exclude_from_context: Option<bool>,
}

impl CustomAgentMessage for BashExecutionMessage {}

/// Message type for extension-injected messages via sendMessage().
#[derive(Clone, Debug)]
pub struct CustomMessage {
    pub custom_type: String,
    /// String or TextContent/ImageContent blocks (JS union).
    pub content: ContentOrText,
    pub display: bool,
    pub details: Option<pi_protocol::Value>,
    pub timestamp: f64,
}

impl CustomAgentMessage for CustomMessage {}

/// Content payload of custom messages: a plain string or content blocks.
#[derive(Clone, Debug)]
pub enum ContentOrText {
    Text(String),
    Blocks(Vec<Content>),
}

impl PartialEq for ContentOrText {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ContentOrText::Text(a), ContentOrText::Text(b)) => a == b,
            (ContentOrText::Blocks(a), ContentOrText::Blocks(b)) => a == b,
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BranchSummaryMessage {
    pub summary: String,
    pub from_id: String,
    pub timestamp: f64,
}

impl CustomAgentMessage for BranchSummaryMessage {}

#[derive(Clone, Debug)]
pub struct CompactionSummaryMessage {
    pub summary: String,
    pub tokens_before: f64,
    pub timestamp: f64,
}

impl CustomAgentMessage for CompactionSummaryMessage {}

/// Convert a BashExecutionMessage to user message text for LLM context.
pub fn bash_execution_to_text(message: &BashExecutionMessage) -> String {
    let mut text = format!("Ran `{}`\n", message.command);
    if !message.output.is_empty() {
        text += &format!("```\n{}\n```", message.output);
    } else {
        text += "(no output)";
    }
    if message.cancelled {
        text += "\n\n(command cancelled)";
    } else if message.exit_code.is_some_and(|code| code != 0) {
        text += &format!("\n\nCommand exited with code {}", message.exit_code.unwrap());
    }
    if message.truncated {
        if let Some(path) = &message.full_output_path {
            text += &format!("\n\n[Output truncated. Full output: {path}]");
        }
    }
    text
}

/// Parse a timestamp string with `new Date(timestamp).getTime()` semantics.
pub fn parse_timestamp_ms(timestamp: &str) -> f64 {
    match timestamp.parse::<f64>() {
        Ok(n) => n,
        Err(_) => {
            // ISO 8601 via chrono-free fallback: only full ISO strings are
            // parsed; anything else is NaN, mirroring Date parsing failure.
            parse_iso_timestamp(timestamp).unwrap_or(f64::NAN)
        }
    }
}

/// Minimal ISO-8601 `YYYY-MM-DDTHH:MM:SS(.sss)?Z` parser (Date.getTime ms).
/// ponytail: JS Date accepts more formats; session files always write ISO.
fn parse_iso_timestamp(value: &str) -> Option<f64> {
    let value = value.strip_suffix('Z')?;
    let (date, time) = value.split_once('T')?;
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    let mut parts = time.split(':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second_part = parts.next()?;
    let (second, millis) = match second_part.split_once('.') {
        Some((s, ms)) => {
            let s: i64 = s.parse().ok()?;
            let mut ms: i64 = ms.parse().ok()?;
            // Fractional digits beyond 3 are truncated (Date semantics).
            while ms > 999 {
                ms /= 10;
            }
            (s, ms)
        }
        None => (second_part.parse().ok()?, 0),
    };
    let days = days_from_civil(year, month, day)?;
    Some(
        days as f64 * 86_400_000.0
            + (hour as f64 * 3_600_000.0 + minute as f64 * 60_000.0 + second as f64 * 1_000.0 + millis as f64),
    )
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// algorithm), with month/day range validation.
fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = (if year >= 0 { year } else { year - 399 }) / 400;
    let yoe = year - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    if day > 31 || day < 1 {
        return None;
    }
    Some(era * 146_097 + doe - 719_468)
}

pub fn create_branch_summary_message(summary: String, from_id: String, timestamp: &str) -> BranchSummaryMessage {
    BranchSummaryMessage {
        summary,
        from_id,
        timestamp: parse_timestamp_ms(timestamp),
    }
}

pub fn create_compaction_summary_message(
    summary: String,
    tokens_before: f64,
    timestamp: &str,
) -> CompactionSummaryMessage {
    CompactionSummaryMessage {
        summary,
        tokens_before,
        timestamp: parse_timestamp_ms(timestamp),
    }
}

pub fn create_custom_message(
    custom_type: String,
    content: ContentOrText,
    display: bool,
    details: Option<pi_protocol::Value>,
    timestamp: &str,
) -> CustomMessage {
    CustomMessage {
        custom_type,
        content,
        display,
        details,
        timestamp: parse_timestamp_ms(timestamp),
    }
}

/// Transform AgentMessages (including custom types) to LLM-compatible Messages.
/// Returns only the messages that participate in context.
pub fn convert_to_llm(messages: &[pi_agent_core::types::AgentMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|message| match message {
            pi_agent_core::types::AgentMessage::Llm(message) => match message {
                Message::User(_) | Message::Assistant(_) | Message::ToolResult(_) => Some(message.clone()),
            },
            pi_agent_core::types::AgentMessage::Custom(custom) => {
                if let Some(bash) = custom.as_any().downcast_ref::<BashExecutionMessage>() {
                    if bash.exclude_from_context == Some(true) {
                        return None;
                    }
                    return Some(Message::User(UserMessage {
                        content: UserMessageContent::Blocks(vec![Content::Text(TextContent {
                            text: bash_execution_to_text(bash),
                            text_signature: None,
                        })]),
                        timestamp: bash.timestamp,
                    }));
                }
                if let Some(custom_msg) = custom.as_any().downcast_ref::<CustomMessage>() {
                    let content = match &custom_msg.content {
                        ContentOrText::Text(text) => UserMessageContent::Blocks(vec![Content::Text(TextContent {
                            text: text.clone(),
                            text_signature: None,
                        })]),
                        ContentOrText::Blocks(blocks) => UserMessageContent::Blocks(blocks.clone()),
                    };
                    return Some(Message::User(UserMessage {
                        content,
                        timestamp: custom_msg.timestamp,
                    }));
                }
                if let Some(branch) = custom.as_any().downcast_ref::<BranchSummaryMessage>() {
                    return Some(Message::User(UserMessage {
                        content: UserMessageContent::Blocks(vec![Content::Text(TextContent {
                            text: format!("{BRANCH_SUMMARY_PREFIX}{}{BRANCH_SUMMARY_SUFFIX}", branch.summary),
                            text_signature: None,
                        })]),
                        timestamp: branch.timestamp,
                    }));
                }
                if let Some(compaction) = custom.as_any().downcast_ref::<CompactionSummaryMessage>() {
                    return Some(Message::User(UserMessage {
                        content: UserMessageContent::Blocks(vec![Content::Text(TextContent {
                            text: format!(
                                "{COMPACTION_SUMMARY_PREFIX}{}{COMPACTION_SUMMARY_SUFFIX}",
                                compaction.summary
                            ),
                            text_signature: None,
                        })]),
                        timestamp: compaction.timestamp,
                    }));
                }
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_execution_text() {
        let message = BashExecutionMessage {
            command: "ls".into(),
            output: "a\nb".into(),
            exit_code: Some(2),
            cancelled: false,
            truncated: true,
            full_output_path: Some("/tmp/out".into()),
            timestamp: 0.0,
            exclude_from_context: None,
        };
        let text = bash_execution_to_text(&message);
        assert!(text.starts_with("Ran `ls`"));
        assert!(text.contains("Command exited with code 2"));
        assert!(text.contains("[Output truncated. Full output: /tmp/out]"));

        let cancelled = BashExecutionMessage {
            command: "ls".into(),
            output: String::new(),
            exit_code: None,
            cancelled: true,
            truncated: false,
            full_output_path: None,
            timestamp: 0.0,
            exclude_from_context: None,
        };
        let text = bash_execution_to_text(&cancelled);
        assert!(text.contains("(no output)"));
        assert!(text.contains("(command cancelled)"));
    }

    #[test]
    fn convert_to_llm_maps_custom_roles() {
        use pi_agent_core::types::AgentMessage;
        use std::sync::Arc;

        let messages = vec![
            AgentMessage::Llm(Message::User(UserMessage {
                content: UserMessageContent::Text("hi".into()),
                timestamp: 1.0,
            })),
            AgentMessage::Custom(Arc::new(BashExecutionMessage {
                command: "pwd".into(),
                ..Default::default()
            })),
            AgentMessage::Custom(Arc::new(BranchSummaryMessage {
                summary: "old path".into(),
                from_id: "e1".into(),
                timestamp: 2.0,
            })),
            AgentMessage::Custom(Arc::new(CompactionSummaryMessage {
                summary: "compacted".into(),
                tokens_before: 100.0,
                timestamp: 3.0,
            })),
            AgentMessage::Custom(Arc::new(CustomMessage {
                custom_type: "x".into(),
                content: ContentOrText::Text("ext".into()),
                display: true,
                details: None,
                timestamp: 4.0,
            })),
        ];
        let converted = convert_to_llm(&messages);
        assert_eq!(converted.len(), 5);
        for message in &converted {
            assert!(matches!(message, Message::User(_)));
        }

        // excludeFromContext skips bash messages.
        let hidden = AgentMessage::Custom(Arc::new(BashExecutionMessage {
            command: "pwd".into(),
            exclude_from_context: Some(true),
            ..Default::default()
        }));
        assert_eq!(convert_to_llm(&[hidden]).len(), 0);
    }

    #[test]
    fn iso_timestamp_parsing() {
        assert_eq!(parse_timestamp_ms("2024-01-15T10:30:00.000Z"), 1705314600000.0);
        assert_eq!(parse_timestamp_ms("1970-01-01T00:00:00.000Z"), 0.0);
        assert_eq!(parse_timestamp_ms("1970-01-01T00:00:01.500Z"), 1500.0);
        assert!(parse_timestamp_ms("garbage").is_nan());
        assert!(parse_timestamp_ms("").is_nan());
    }
}
