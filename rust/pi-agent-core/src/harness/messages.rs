//! Harness custom messages, port of `packages/agent/src/harness/messages.ts`.
//!
//! JS extends `AgentMessage` with role-union custom messages
//! (bashExecution/custom/branchSummary/compactionSummary); the Rust
//! `AgentMessage::Custom(Arc<dyn CustomAgentMessage>)` variant carries these
//! as one downcastable struct.

use pi_ai::types::{Content, Message, UserMessage, UserMessageContent};

use crate::types::{AgentMessage, CustomAgentMessage};

pub const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
pub const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";
pub const BRANCH_SUMMARY_PREFIX: &str = "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";
pub const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

/// Kind of a harness custom message.
#[derive(Clone, Debug, PartialEq)]
pub enum CustomMessageKind {
    BashExecution,
    Custom,
    BranchSummary,
    CompactionSummary,
}

/// Content of a custom message: plain text or content blocks.
#[derive(Clone, Debug, PartialEq)]
pub enum CustomContent {
    Text(String),
    Blocks(Vec<Content>),
}

/// Concrete harness custom message, downcastable from `AgentMessage::Custom`.
#[derive(Clone, Debug, PartialEq)]
pub struct HarnessCustomMessage {
    pub kind: CustomMessageKind,
    pub timestamp: f64,
    // bashExecution
    pub command: String,
    pub output: String,
    pub exit_code: Option<f64>,
    pub cancelled: bool,
    pub truncated: bool,
    pub full_output_path: Option<String>,
    pub exclude_from_context: bool,
    // custom
    pub custom_type: String,
    pub content: CustomContent,
    pub display: bool,
    pub details: Option<crate::harness::session_types::JsonValue>,
    // branchSummary / compactionSummary
    pub summary: String,
    pub tokens_before: f64,
    pub from_id: String,
}

impl CustomAgentMessage for HarnessCustomMessage {}

impl HarnessCustomMessage {
    pub fn bash_execution(
        command: String,
        output: String,
        exit_code: Option<f64>,
        cancelled: bool,
        truncated: bool,
        full_output_path: Option<String>,
        exclude_from_context: bool,
        timestamp: f64,
    ) -> Self {
        Self {
            kind: CustomMessageKind::BashExecution,
            timestamp,
            command,
            output,
            exit_code,
            cancelled,
            truncated,
            full_output_path,
            exclude_from_context,
            custom_type: String::new(),
            content: CustomContent::Text(String::new()),
            display: true,
            details: None,
            summary: String::new(),
            tokens_before: 0.0,
            from_id: String::new(),
        }
    }

    pub fn custom(
        custom_type: String,
        content: CustomContent,
        display: bool,
        details: Option<crate::harness::session_types::JsonValue>,
        timestamp: f64,
    ) -> Self {
        Self {
            kind: CustomMessageKind::Custom,
            timestamp,
            command: String::new(),
            output: String::new(),
            exit_code: None,
            cancelled: false,
            truncated: false,
            full_output_path: None,
            exclude_from_context: false,
            custom_type,
            content,
            display,
            details,
            summary: String::new(),
            tokens_before: 0.0,
            from_id: String::new(),
        }
    }

    pub fn branch_summary(summary: String, from_id: String, timestamp: f64) -> Self {
        Self {
            kind: CustomMessageKind::BranchSummary,
            timestamp,
            command: String::new(),
            output: String::new(),
            exit_code: None,
            cancelled: false,
            truncated: false,
            full_output_path: None,
            exclude_from_context: false,
            custom_type: String::new(),
            content: CustomContent::Text(String::new()),
            display: true,
            details: None,
            summary,
            tokens_before: 0.0,
            from_id,
        }
    }

    pub fn compaction_summary(summary: String, tokens_before: f64, timestamp: f64) -> Self {
        Self {
            kind: CustomMessageKind::CompactionSummary,
            timestamp,
            command: String::new(),
            output: String::new(),
            exit_code: None,
            cancelled: false,
            truncated: false,
            full_output_path: None,
            exclude_from_context: false,
            custom_type: String::new(),
            content: CustomContent::Text(String::new()),
            display: true,
            details: None,
            summary,
            tokens_before,
            from_id: String::new(),
        }
    }
}

/// Downcast an AgentMessage to a harness custom message.
pub fn as_harness_custom(message: &AgentMessage) -> Option<&HarnessCustomMessage> {
    match message {
        AgentMessage::Custom(custom) => custom.as_any().downcast_ref::<HarnessCustomMessage>(),
        AgentMessage::Llm(_) => None,
    }
}

/// Format a bashExecution message as plain text.
pub fn bash_execution_to_text(msg: &HarnessCustomMessage) -> String {
    let mut text = format!("Ran `{}`\n", msg.command);
    if !msg.output.is_empty() {
        text += &format!("```\n{}\n```", msg.output);
    } else {
        text += "(no output)";
    }
    if msg.cancelled {
        text += "\n\n(command cancelled)";
    } else if msg.exit_code.is_some_and(|code| code != 0.0) {
        text += &format!("\n\nCommand exited with code {}", msg.exit_code.unwrap_or(0.0));
    }
    if msg.truncated {
        if let Some(full_output_path) = &msg.full_output_path {
            text += &format!("\n\n[Output truncated. Full output: {full_output_path}]");
        }
    }
    text
}

/// Convert agent messages to LLM messages, mirroring `convertToLlm`.
pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::Custom(custom) => {
                let Some(msg) = custom.as_any().downcast_ref::<HarnessCustomMessage>() else {
                    return None;
                };
                match msg.kind {
                    CustomMessageKind::BashExecution => {
                        if msg.exclude_from_context {
                            return None;
                        }
                        Some(Message::User(UserMessage {
                            content: UserMessageContent::Text(bash_execution_to_text(msg)),
                            timestamp: msg.timestamp,
                        }))
                    }
                    CustomMessageKind::Custom => {
                        let content = match &msg.content {
                            CustomContent::Text(text) => UserMessageContent::Text(text.clone()),
                            CustomContent::Blocks(blocks) => UserMessageContent::Blocks(blocks.clone()),
                        };
                        Some(Message::User(UserMessage {
                            content,
                            timestamp: msg.timestamp,
                        }))
                    }
                    CustomMessageKind::BranchSummary => Some(Message::User(UserMessage {
                        content: UserMessageContent::Text(format!(
                            "{BRANCH_SUMMARY_PREFIX}{}{BRANCH_SUMMARY_SUFFIX}",
                            msg.summary
                        )),
                        timestamp: msg.timestamp,
                    })),
                    CustomMessageKind::CompactionSummary => Some(Message::User(UserMessage {
                        content: UserMessageContent::Text(format!(
                            "{COMPACTION_SUMMARY_PREFIX}{}{COMPACTION_SUMMARY_SUFFIX}",
                            msg.summary
                        )),
                        timestamp: msg.timestamp,
                    })),
                }
            }
            AgentMessage::Llm(message) => Some(message.clone()),
        })
        .collect()
}
