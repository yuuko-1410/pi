//! Session context building, port of
//! `packages/agent/src/harness/session/context.ts`.

use crate::types::AgentMessage;

use super::session_types::{CompactionEntry, CustomEntry, Entry};
use super::messages::{as_harness_custom, CustomMessageKind, HarnessCustomMessage};

#[derive(Clone, Debug, PartialEq)]
pub struct SessionContext {
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    pub model: Option<(String, String)>,
    pub active_tool_names: Option<Vec<String>>,
}

/// Projector for custom entries; returns projected messages or None to skip.
pub type CustomEntryContextMessageProjector =
    Box<dyn Fn(&CustomEntry, usize, &[Entry]) -> Option<Vec<AgentMessage>> + Send + Sync>;

pub struct SessionContextBuildOptions {
    pub entry_transforms: Vec<Box<dyn Fn(&[Entry]) -> Vec<Entry> + Send + Sync>>,
    pub entry_projectors: std::collections::HashMap<String, CustomEntryContextMessageProjector>,
}

impl Default for SessionContextBuildOptions {
    fn default() -> Self {
        Self {
            entry_transforms: Vec::new(),
            entry_projectors: std::collections::HashMap::new(),
        }
    }
}

fn derive_session_context_state(path_entries: &[Entry]) -> SessionContext {
    let mut context = SessionContext {
        messages: Vec::new(),
        thinking_level: "off".to_string(),
        model: None,
        active_tool_names: None,
    };

    for entry in path_entries {
        match entry {
            Entry::ThinkingLevelChange(entry) => {
                context.thinking_level = entry.thinking_level.clone();
            }
            Entry::ModelChange(entry) => {
                context.model = Some((entry.provider.clone(), entry.model_id.clone()));
            }
            Entry::Message(entry) => {
                if entry.message.role() == "assistant" {
                    let Some(assistant) = entry.message.as_assistant() else { continue };
                    context.model = Some((assistant.provider.clone(), assistant.model.clone()));
                }
            }
            Entry::ActiveToolsChange(entry) => {
                context.active_tool_names = Some(entry.active_tool_names.clone());
            }
            _ => {}
        }
    }

    context
}

/// Default transform: replace everything before the last compaction with
/// just that compaction entry.
pub fn default_context_entry_transform(path_entries: &[Entry]) -> Vec<Entry> {
    let mut compaction: Option<CompactionEntry> = None;
    let mut compaction_index = -1isize;
    for (index, entry) in path_entries.iter().enumerate().rev() {
        if let Entry::Compaction(entry) = entry {
            compaction = Some(entry.clone());
            compaction_index = index as isize;
            break;
        }
    }
    match compaction {
        None => path_entries.to_vec(),
        Some(entry) => {
            let mut result = vec![Entry::Compaction(entry)];
            result.extend(path_entries[(compaction_index as usize + 1)..].iter().cloned());
            result
        }
    }
}

pub fn build_context_entries(path_entries: &[Entry], options: &SessionContextBuildOptions) -> Vec<Entry> {
    let mut entries = default_context_entry_transform(path_entries);
    for transform in &options.entry_transforms {
        entries = transform(&entries);
    }
    entries
}

/// Convert one entry into context messages, mirroring
/// `sessionEntryToContextMessages`.
pub fn session_entry_to_context_messages(
    entry: &Entry,
    index: usize,
    entries: &[Entry],
    options: &SessionContextBuildOptions,
) -> Vec<AgentMessage> {
    match entry {
        Entry::Message(entry) => {
            // Deferred assistant messages are excluded from context.
            if entry.message.role() == "assistant" {
                if let Some(assistant) = entry.message.as_assistant() {
                    if assistant.stop_reason.as_str() == "deferred" {
                        return Vec::new();
                    }
                }
            }
            vec![entry.message.clone()]
        }
        Entry::Compaction(entry) => {
            let mut result = vec![AgentMessage::Custom(std::sync::Arc::new(
                HarnessCustomMessage::compaction_summary(
                    entry.summary.clone(),
                    entry.tokens_before,
                    entry.base.timestamp,
                ),
            ))];
            result.extend(entry.retained_tail.iter().cloned());
            result
        }
        Entry::BranchSummary(entry) => {
            if entry.summary.is_empty() {
                return Vec::new();
            }
            vec![AgentMessage::Custom(std::sync::Arc::new(
                HarnessCustomMessage::branch_summary(
                    entry.summary.clone(),
                    entry.from_id.clone(),
                    entry.base.timestamp,
                ),
            ))]
        }
        Entry::Custom(entry) => {
            if let Some(projector) = options.entry_projectors.get(&entry.custom_type) {
                projector(entry, index, entries).unwrap_or_default()
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

pub fn build_session_context(path_entries: &[Entry], options: &SessionContextBuildOptions) -> SessionContext {
    let mut context = derive_session_context_state(path_entries);
    let context_entries = build_context_entries(path_entries, options);
    context.messages = context_entries
        .iter()
        .enumerate()
        .flat_map(|(index, entry)| session_entry_to_context_messages(entry, index, &context_entries, options))
        .collect();
    context
}

/// Role accessor for custom messages via downcast.
pub fn custom_role(message: &AgentMessage) -> Option<&'static str> {
    let custom = as_harness_custom(message)?;
    Some(match custom.kind {
        CustomMessageKind::BashExecution => "bashExecution",
        CustomMessageKind::Custom => "custom",
        CustomMessageKind::BranchSummary => "branchSummary",
        CustomMessageKind::CompactionSummary => "compactionSummary",
    })
}
