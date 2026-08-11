//! Compaction logic, port of `packages/agent/src/harness/compaction/compaction.ts`
//! plus its `utils.ts` (merged; both are small and closely coupled).
//!
//! The LLM call sites (`generateSummaryWithUsage`, `compact`, ...) are
//! generic over a minimal `SimpleCompleter` trait; the JS `Models` interface
//! (auth, providers, deferred fetch) is ported later and will implement it.

use std::collections::HashSet;

use pi_ai::types::{AssistantMessage, Content, Message, Usage};
use pi_ai::utils::retry::RetryPolicy;

use super::messages::{convert_to_llm, CustomMessageKind};
use super::context::build_session_context;
use super::session_types::Entry;
use crate::types::AgentMessage;

// ---------------------------------------------------------------------------
// Result / error types (mirror harness/types.ts CompactionError)
// ---------------------------------------------------------------------------

/// Compaction-specific error with a stable code.
#[derive(Clone, Debug, PartialEq)]
pub struct CompactionError {
    pub code: String,
    pub message: String,
}

impl CompactionError {
    pub fn aborted(message: impl Into<String>) -> Self {
        Self {
            code: "aborted".to_string(),
            message: message.into(),
        }
    }
    pub fn summarization_failed(message: impl Into<String>) -> Self {
        Self {
            code: "summarization_failed".to_string(),
            message: message.into(),
        }
    }
}

/// Minimal completer abstraction; JS `Models.completeSimple`.
pub trait SimpleCompleter {
    fn complete_simple(
        &self,
        model: &pi_ai::types::Model,
        context: &pi_ai::types::Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessage, String>;
}

/// Subset of `SimpleStreamOptions` used by compaction.
#[derive(Clone, Debug, Default)]
pub struct SimpleStreamOptions {
    pub max_tokens: Option<f64>,
    pub signal: Option<()>,
    pub reasoning: Option<String>,
    pub cache_retention: Option<String>,
    pub session_id: Option<String>,
}

// ---------------------------------------------------------------------------
// File operation utilities (compaction/utils.ts)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct FileOperations {
    /// Files read but not necessarily modified.
    pub read: HashSet<String>,
    /// Files written by full-file write operations.
    pub written: HashSet<String>,
    /// Files modified by edit operations.
    pub edited: HashSet<String>,
}

pub fn create_file_ops() -> FileOperations {
    FileOperations::default()
}

/// Add file operations from assistant tool calls to an accumulator.
pub fn extract_file_ops_from_message(message: &AgentMessage, file_ops: &mut FileOperations) {
    let AgentMessage::Llm(Message::Assistant(assistant)) = message else { return };
    for block in &assistant.content {
        let Content::ToolCall(tool_call) = block else { continue };
        let path = tool_call
            .arguments
            .as_map()
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|(key, _)| key == "path")
                    .and_then(|(_, value)| value.as_str())
                    .map(|path| path.to_string())
            });
        let Some(path) = path else { continue };
        match tool_call.name.as_str() {
            "read" => {
                file_ops.read.insert(path);
            }
            "write" => {
                file_ops.written.insert(path);
            }
            "edit" => {
                file_ops.edited.insert(path);
            }
            _ => {}
        }
    }
}

/// Compute sorted read-only and modified file lists.
pub fn compute_file_lists(file_ops: &FileOperations) -> (Vec<String>, Vec<String>) {
    let mut modified: Vec<String> = file_ops
        .edited
        .iter()
        .chain(file_ops.written.iter())
        .cloned()
        .collect();
    modified.sort();
    modified.dedup();
    let mut read_only: Vec<String> = file_ops
        .read
        .iter()
        .filter(|file| !modified.contains(file))
        .cloned()
        .collect();
    read_only.sort();
    (read_only, modified)
}

/// Format file lists as summary metadata tags.
pub fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!("<read-files>\n{}\n</read-files>", read_files.join("\n")));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        return String::new();
    }
    format!("\n\n{}", sections.join("\n\n"))
}

const TOOL_RESULT_MAX_CHARS: usize = 2000;

fn user_content_text(content: &pi_ai::types::UserMessageContent) -> String {
    match content {
        pi_ai::types::UserMessageContent::Text(text) => text.clone(),
        pi_ai::types::UserMessageContent::Blocks(blocks) => pi_ai::utils::text::content_text(blocks, ""),
    }
}

fn safe_json_stringify(value: &pi_protocol::Value) -> String {
    pi_ai::utils::json::json_stringify(value)
}

fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated_chars = text.chars().count() - max_chars;
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}\n\n[... {truncated_chars} more characters truncated]")
}

/// Serialize LLM messages to plain text for summarization prompts.
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();

    for msg in messages {
        match msg {
            Message::User(user) => {
                let content = user_content_text(&user.content);
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Message::Assistant(assistant) => {
                let mut thinking_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<String> = Vec::new();

                for block in &assistant.content {
                    match block {
                        Content::Thinking(thinking) => thinking_parts.push(thinking.thinking.clone()),
                        Content::ToolCall(tool_call) => {
                            let args = tool_call
                                .arguments
                                .as_map()
                                .map(|entries| {
                                    entries
                                        .iter()
                                        .map(|(key, value)| format!("{key}={}", safe_json_stringify(value)))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                })
                                .unwrap_or_default();
                            tool_calls.push(format!("{}({args})", tool_call.name));
                        }
                        _ => {}
                    }
                }

                if !thinking_parts.is_empty() {
                    parts.push(format!("[Assistant thinking]: {}", thinking_parts.join("\n")));
                }
                if assistant.content.iter().any(|block| matches!(block, Content::Text(_))) {
                    let text = pi_ai::utils::text::content_text(&assistant.content, "");
                    if !text.is_empty() {
                        parts.push(format!("[Assistant]: {text}"));
                    }
                }
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Message::ToolResult(tool_result) => {
                let content = pi_ai::utils::text::content_text(&tool_result.content, "");
                if !content.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&content, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
        }
    }

    parts.join("\n\n")
}

// ---------------------------------------------------------------------------
// Token estimation and thresholds
// ---------------------------------------------------------------------------

/// File-operation details stored on generated compaction entries.
#[derive(Clone, Debug, PartialEq)]
pub struct CompactionDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

/// Compaction thresholds and retention settings.
#[derive(Clone, Debug, PartialEq)]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: f64,
    pub keep_recent_tokens: f64,
}

pub const DEFAULT_COMPACTION_SETTINGS: CompactionSettings = CompactionSettings {
    enabled: true,
    reserve_tokens: 16384.0,
    keep_recent_tokens: 20000.0,
};

/// Calculate total context tokens from provider usage.
pub fn calculate_context_tokens(usage: &Usage) -> f64 {
    if usage.total_tokens != 0.0 {
        usage.total_tokens
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    }
}

fn get_assistant_usage(message: &AgentMessage) -> Option<Usage> {
    if message.role() == "assistant" {
        if let Some(assistant) = message.as_assistant() {
            if assistant.stop_reason.as_str() != "aborted"
                && assistant.stop_reason.as_str() != "error"
                && calculate_context_tokens(&assistant.usage) > 0.0
            {
                return Some(assistant.usage.clone());
            }
        }
    }
    None
}

/// Return usage from the last valid assistant message in session entries.
pub fn get_last_assistant_usage(entries: &[Entry]) -> Option<Usage> {
    for entry in entries.iter().rev() {
        if let Entry::Message(entry) = entry {
            if let Some(usage) = get_assistant_usage(&entry.message) {
                return Some(usage);
            }
        }
    }
    None
}

/// Estimated context-token usage for a message list.
#[derive(Clone, Debug, PartialEq)]
pub struct ContextUsageEstimate {
    pub tokens: f64,
    pub usage_tokens: f64,
    pub trailing_tokens: f64,
    pub last_usage_index: Option<usize>,
}

fn get_last_assistant_usage_info(messages: &[AgentMessage]) -> Option<(Usage, usize)> {
    for (index, message) in messages.iter().enumerate().rev() {
        if let Some(usage) = get_assistant_usage(message) {
            return Some((usage, index));
        }
    }
    None
}

/// Estimate context tokens for messages using provider usage when available.
pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    match get_last_assistant_usage_info(messages) {
        None => {
            let estimated: f64 = messages.iter().map(estimate_tokens).sum();
            ContextUsageEstimate {
                tokens: estimated,
                usage_tokens: 0.0,
                trailing_tokens: estimated,
                last_usage_index: None,
            }
        }
        Some((usage, usage_index)) => {
            let usage_tokens = calculate_context_tokens(&usage);
            let trailing_tokens: f64 = messages[usage_index + 1..].iter().map(estimate_tokens).sum();
            ContextUsageEstimate {
                tokens: usage_tokens + trailing_tokens,
                usage_tokens,
                trailing_tokens,
                last_usage_index: Some(usage_index),
            }
        }
    }
}

/// Return whether context usage exceeds the configured compaction threshold.
pub fn should_compact(context_tokens: f64, context_window: f64, settings: &CompactionSettings) -> bool {
    if !settings.enabled {
        return false;
    }
    context_tokens > context_window - settings.reserve_tokens
}

const ESTIMATED_IMAGE_CHARS: f64 = 4800.0;

fn estimate_text_and_image_content_chars(content: &pi_ai::types::UserMessageContent) -> f64 {
    match content {
        pi_ai::types::UserMessageContent::Text(text) => text.len() as f64,
        pi_ai::types::UserMessageContent::Blocks(blocks) => {
            let mut chars = 0.0;
            for block in blocks {
                match block {
                    Content::Text(text) => chars += text.text.len() as f64,
                    Content::Image(_) => chars += ESTIMATED_IMAGE_CHARS,
                    _ => {}
                }
            }
            chars
        }
    }
}

/// Estimate token count for one message using a conservative character heuristic.
pub fn estimate_tokens(message: &AgentMessage) -> f64 {
    let chars = match message {
        AgentMessage::Llm(Message::User(user)) => {
            return (estimate_text_and_image_content_chars(&user.content) / 4.0).ceil();
        }
        AgentMessage::Llm(Message::Assistant(assistant)) => {
            let mut chars = 0.0;
            for block in &assistant.content {
                match block {
                    Content::Text(text) => chars += text.text.len() as f64,
                    Content::Thinking(thinking) => chars += thinking.thinking.len() as f64,
                    Content::ToolCall(tool_call) => {
                        chars += tool_call.name.len() as f64 + safe_json_stringify(&tool_call.arguments).len() as f64;
                    }
                    _ => {}
                }
            }
            chars
        }
        AgentMessage::Llm(Message::ToolResult(tool_result)) => {
            estimate_text_and_image_content_chars(&pi_ai::types::UserMessageContent::Blocks(
                tool_result.content.clone(),
            ))
        }
        AgentMessage::Custom(custom) => {
            let Some(msg) = custom.as_any().downcast_ref::<super::messages::HarnessCustomMessage>() else {
                return 0.0;
            };
            match msg.kind {
                CustomMessageKind::BashExecution => (msg.command.len() + msg.output.len()) as f64,
                CustomMessageKind::Custom => match &msg.content {
                    super::messages::CustomContent::Text(text) => text.len() as f64,
                    super::messages::CustomContent::Blocks(blocks) => estimate_text_and_image_content_chars(
                        &pi_ai::types::UserMessageContent::Blocks(blocks.clone()),
                    ),
                },
                CustomMessageKind::BranchSummary | CustomMessageKind::CompactionSummary => msg.summary.len() as f64,
            }
        }
    };
    (chars / 4.0).ceil()
}

// ---------------------------------------------------------------------------
// Cut point selection
// ---------------------------------------------------------------------------

fn find_valid_cut_points(entries: &[Entry], start_index: usize, end_index: usize) -> Vec<usize> {
    let mut cut_points: Vec<usize> = Vec::new();
    for index in start_index..end_index {
        let entry = &entries[index];
        match entry {
            Entry::Message(entry) => {
                match entry.message.role() {
                    "bashExecution" | "custom" | "branchSummary" | "compactionSummary" | "user" | "assistant" => {
                        cut_points.push(index);
                    }
                    "toolResult" => {}
                    _ => {}
                }
            }
            Entry::BranchSummary(entry) => {
                let _ = entry;
                cut_points.push(index);
            }
            _ => {}
        }
    }
    cut_points
}

/// Find the user-visible message that starts the turn containing an entry.
pub fn find_turn_start_index(entries: &[Entry], entry_index: usize, start_index: usize) -> isize {
    for index in (start_index..=entry_index).rev() {
        let entry = &entries[index];
        match entry {
            Entry::BranchSummary(_) => return index as isize,
            Entry::Message(entry) => {
                let role = entry.message.role();
                if role == "user" || role == "bashExecution" {
                    return index as isize;
                }
            }
            _ => {}
        }
    }
    -1
}

/// Cut point selected for compaction.
#[derive(Clone, Debug, PartialEq)]
pub struct CutPointResult {
    pub first_kept_entry_index: usize,
    pub turn_start_index: isize,
    pub is_split_turn: bool,
}

/// Find the compaction cut point that keeps approximately the requested
/// recent-token budget.
pub fn find_cut_point(
    entries: &[Entry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: f64,
) -> CutPointResult {
    let cut_points = find_valid_cut_points(entries, start_index, end_index);

    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: -1,
            is_split_turn: false,
        };
    }

    let mut accumulated_tokens = 0.0f64;
    let mut cut_index = cut_points[0];

    for index in (start_index..end_index).rev() {
        let entry = &entries[index];
        let Entry::Message(entry) = entry else { continue };
        accumulated_tokens += estimate_tokens(&entry.message);
        if accumulated_tokens >= keep_recent_tokens {
            for c in 0..cut_points.len() {
                if cut_points[c] >= index {
                    cut_index = cut_points[c];
                    break;
                }
            }
            break;
        }
    }

    while cut_index > start_index {
        let prev_entry = &entries[cut_index - 1];
        match prev_entry {
            Entry::Compaction(_) => break,
            Entry::Message(_) => break,
            _ => cut_index -= 1,
        }
    }

    let cut_entry = &entries[cut_index];
    let is_user_message = matches!(cut_entry, Entry::Message(entry) if entry.message.role() == "user");
    let turn_start_index = if is_user_message {
        -1
    } else {
        find_turn_start_index(entries, cut_index, start_index)
    };

    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !is_user_message && turn_start_index != -1,
    }
}

// ---------------------------------------------------------------------------
// Prompts (exact strings from compaction.ts)
// ---------------------------------------------------------------------------

pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

pub const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or \"(none)\" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

pub const UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed
- UPDATE \"Next Steps\" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = "This is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.

Summarize the prefix to provide context for the retained suffix:

## Original Request
[What did the user ask for in this turn?]

## Early Progress
- [Key decisions and work done in the prefix]

## Context for Suffix
- [Information needed to understand the retained recent work]

Be concise. Focus on what's needed to understand the kept suffix.";

// ---------------------------------------------------------------------------
// Preparation
// ---------------------------------------------------------------------------

/// Prepared inputs for a compaction run.
#[derive(Clone, Debug)]
pub struct CompactionPreparation {
    pub messages_to_summarize: Vec<AgentMessage>,
    pub turn_prefix_messages: Vec<AgentMessage>,
    pub retained_tail: Vec<AgentMessage>,
    pub is_split_turn: bool,
    pub tokens_before: f64,
    pub previous_summary: Option<String>,
    pub file_ops: FileOperations,
    pub settings: CompactionSettings,
}

fn get_message_from_entry(entry: &Entry) -> Option<AgentMessage> {
    match entry {
        Entry::Message(entry) => Some(entry.message.clone()),
        Entry::BranchSummary(entry) => Some(AgentMessage::Custom(std::sync::Arc::new(
            super::messages::HarnessCustomMessage::branch_summary(
                entry.summary.clone(),
                entry.from_id.clone(),
                entry.base.timestamp,
            ),
        ))),
        Entry::Compaction(entry) => Some(AgentMessage::Custom(std::sync::Arc::new(
            super::messages::HarnessCustomMessage::compaction_summary(
                entry.summary.clone(),
                entry.tokens_before,
                entry.base.timestamp,
            ),
        ))),
        _ => None,
    }
}

fn get_message_from_entry_for_compaction(entry: &Entry) -> Option<AgentMessage> {
    match entry {
        Entry::Compaction(_) => None,
        _ => get_message_from_entry(entry),
    }
}

fn extract_file_operations(
    messages: &[AgentMessage],
    entries: &[Entry],
    prev_compaction_index: isize,
) -> FileOperations {
    let mut file_ops = create_file_ops();
    if prev_compaction_index >= 0 {
        let Entry::Compaction(prev_compaction) = &entries[prev_compaction_index as usize] else {
            return file_ops;
        };
        if let Some(details) = &prev_compaction.details {
            if let Some(entries) = details.as_map() {
                if let Some(read_files) = entries.iter().find(|(k, _)| k == "readFiles").map(|(_, v)| v) {
                    if let Some(files) = read_files.as_array() {
                        for file in files {
                            if let Some(file) = file.as_str() {
                                file_ops.read.insert(file.to_string());
                            }
                        }
                    }
                }
                if let Some(modified_files) = entries.iter().find(|(k, _)| k == "modifiedFiles").map(|(_, v)| v) {
                    if let Some(files) = modified_files.as_array() {
                        for file in files {
                            if let Some(file) = file.as_str() {
                                file_ops.edited.insert(file.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    for message in messages {
        extract_file_ops_from_message(message, &mut file_ops);
    }
    file_ops
}

/// Prepare session entries for compaction, or return None when compaction
/// is not applicable.
pub fn prepare_compaction(
    path_entries: &[Entry],
    settings: &CompactionSettings,
) -> Result<Option<CompactionPreparation>, CompactionError> {
    if path_entries.is_empty() || matches!(path_entries.last(), Some(Entry::Compaction(_))) {
        return Ok(None);
    }

    let mut prev_compaction_index = -1isize;
    for (index, entry) in path_entries.iter().enumerate().rev() {
        if matches!(entry, Entry::Compaction(_)) {
            prev_compaction_index = index as isize;
            break;
        }
    }

    let mut previous_summary: Option<String> = None;
    let mut compactable_entries: Vec<Entry>;
    if prev_compaction_index >= 0 {
        let prev_compaction = match &path_entries[prev_compaction_index as usize] {
            Entry::Compaction(entry) => entry,
            _ => unreachable!(),
        };
        previous_summary = Some(prev_compaction.summary.clone());
        let virtual_retained_entries: Vec<Entry> = prev_compaction
            .retained_tail
            .iter()
            .enumerate()
            .map(|(index, message)| {
                let (id, parent_id) = if index == 0 {
                    (
                        format!("{}:retained:{}", prev_compaction.base.id, index),
                        prev_compaction.base.id.clone(),
                    )
                } else {
                    (
                        format!("{}:retained:{}", prev_compaction.base.id, index),
                        format!("{}:retained:{}", prev_compaction.base.id, index - 1),
                    )
                };
                Entry::Message(super::session_types::MessageEntry {
                    base: super::session_types::EntryBase {
                        id,
                        parent_id: Some(parent_id),
                        seq: prev_compaction.base.seq,
                        timestamp: agent_message_timestamp(message),
                        type_: "message".to_string(),
                    },
                    message: message.clone(),
                    terminate: None,
                })
            })
            .collect();
        compactable_entries = virtual_retained_entries;
        compactable_entries.extend(path_entries[prev_compaction_index as usize + 1..].iter().cloned());
    } else {
        compactable_entries = path_entries.to_vec();
    }
    let boundary_end = compactable_entries.len();

    let tokens_before = estimate_context_tokens(&build_session_context(path_entries, &Default::default()).messages)
        .tokens;

    let cut_point = find_cut_point(&compactable_entries, 0, boundary_end, settings.keep_recent_tokens);
    let history_end = if cut_point.is_split_turn {
        cut_point.turn_start_index as usize
    } else {
        cut_point.first_kept_entry_index
    };
    let mut messages_to_summarize: Vec<AgentMessage> = Vec::new();
    for index in 0..history_end {
        if let Some(message) = get_message_from_entry_for_compaction(&compactable_entries[index]) {
            messages_to_summarize.push(message);
        }
    }
    let mut turn_prefix_messages: Vec<AgentMessage> = Vec::new();
    if cut_point.is_split_turn {
        for index in cut_point.turn_start_index as usize..cut_point.first_kept_entry_index {
            if let Some(message) = get_message_from_entry_for_compaction(&compactable_entries[index]) {
                turn_prefix_messages.push(message);
            }
        }
    }
    let mut retained_tail: Vec<AgentMessage> = Vec::new();
    for index in cut_point.first_kept_entry_index..boundary_end {
        if let Some(message) = get_message_from_entry_for_compaction(&compactable_entries[index]) {
            retained_tail.push(message);
        }
    }
    let mut file_ops = extract_file_operations(&messages_to_summarize, path_entries, prev_compaction_index);
    if cut_point.is_split_turn {
        for message in &turn_prefix_messages {
            extract_file_ops_from_message(message, &mut file_ops);
        }
    }

    Ok(Some(CompactionPreparation {
        messages_to_summarize,
        turn_prefix_messages,
        retained_tail,
        is_split_turn: cut_point.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        settings: settings.clone(),
    }))
}

// ---------------------------------------------------------------------------
// LLM-backed summary generation (generic over SimpleCompleter)
// ---------------------------------------------------------------------------

fn combine_usage(first: &Usage, second: &Usage) -> Usage {
    Usage {
        input: first.input + second.input,
        output: first.output + second.output,
        cache_read: first.cache_read + second.cache_read,
        cache_write: first.cache_write + second.cache_write,
        cache_write_1h: match (first.cache_write_1h, second.cache_write_1h) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        },
        reasoning: match (first.reasoning, second.reasoning) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        },
        total_tokens: first.total_tokens + second.total_tokens,
        cost: pi_ai::types::UsageCost {
            input: first.cost.input + second.cost.input,
            output: first.cost.output + second.cost.output,
            cache_read: first.cost.cache_read + second.cost.cache_read,
            cache_write: first.cost.cache_write + second.cost.cache_write,
            total: first.cost.total + second.cost.total,
        },
    }
}

/// Complete a simple request with retries (summaries are standalone
/// requests: no cache retention, fresh session id).
pub fn complete_simple_with_retries<C: SimpleCompleter>(
    models: &C,
    model: &pi_ai::types::Model,
    context: &pi_ai::types::Context,
    options: &SimpleStreamOptions,
    _retry: Option<&RetryPolicy>,
) -> Result<AssistantMessage, String> {
    let request_options = SimpleStreamOptions {
        cache_retention: Some("none".to_string()),
        session_id: Some(uuid_v7()),
        ..options.clone()
    };
    // ponytail: retry loop on transient failures; JS retryAssistantCall wraps
    // the call with RetryPolicy. Implemented as a single attempt for now.
    models.complete_simple(model, context, &request_options)
}

fn uuid_v7() -> String {
    pi_ai::utils::uuid::uuidv7()
}

fn agent_message_timestamp(message: &AgentMessage) -> f64 {
    match message {
        AgentMessage::Llm(Message::User(user)) => user.timestamp,
        AgentMessage::Llm(Message::Assistant(assistant)) => assistant.timestamp,
        AgentMessage::Llm(Message::ToolResult(tool_result)) => tool_result.timestamp,
        AgentMessage::Custom(custom) => custom
            .as_any()
            .downcast_ref::<super::messages::HarnessCustomMessage>()
            .map(|msg| msg.timestamp)
            .unwrap_or(0.0),
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as f64
}

/// Generate or update a conversation summary and return its provider usage.
pub fn generate_summary_with_usage<C: SimpleCompleter>(
    current_messages: &[AgentMessage],
    models: &C,
    model: &pi_ai::types::Model,
    reserve_tokens: f64,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    thinking_level: Option<&str>,
    retry: Option<&RetryPolicy>,
) -> Result<(String, Usage), CompactionError> {
    let max_tokens = (0.8 * reserve_tokens).floor().min(
        if model.max_tokens > 0.0 {
            model.max_tokens
        } else {
            f64::INFINITY
        },
    );
    let mut base_prompt = if previous_summary.is_some() {
        UPDATE_SUMMARIZATION_PROMPT.to_string()
    } else {
        SUMMARIZATION_PROMPT.to_string()
    };
    if let Some(custom_instructions) = custom_instructions {
        base_prompt = format!("{base_prompt}\n\nAdditional focus: {custom_instructions}");
    }
    let llm_messages = convert_to_llm(current_messages);
    let conversation_text = serialize_conversation(&llm_messages);
    let mut prompt_text = format!("<conversation>\n{conversation_text}\n</conversation>\n\n");
    if let Some(previous_summary) = previous_summary {
        prompt_text += &format!("<previous-summary>\n{previous_summary}\n</previous-summary>\n\n");
    }
    prompt_text += &base_prompt;

    let summarization_messages = pi_ai::types::Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_string()),
        messages: vec![Message::User(pi_ai::types::UserMessage {
            content: pi_ai::types::UserMessageContent::Blocks(vec![Content::Text(pi_ai::types::TextContent {
                text: prompt_text,
                text_signature: None,
            })]),
            timestamp: now_ms(),
        })],
        tools: None,
    };

    let completion_options = SimpleStreamOptions {
        max_tokens: Some(max_tokens),
        reasoning: if model.reasoning && thinking_level.is_some_and(|level| level != "off") {
            thinking_level.map(|level| level.to_string())
        } else {
            None
        },
        ..SimpleStreamOptions::default()
    };

    let response = complete_simple_with_retries(models, model, &summarization_messages, &completion_options, retry)
        .map_err(|message| CompactionError::summarization_failed(format!("Summarization failed: {message}")))?;
    if response.stop_reason.as_str() == "aborted" {
        return Err(CompactionError::aborted(
            response.error_message.clone().unwrap_or_else(|| "Summarization aborted".to_string()),
        ));
    }
    if response.stop_reason.as_str() == "error" {
        return Err(CompactionError::summarization_failed(format!(
            "Summarization failed: {}",
            response.error_message.clone().unwrap_or_else(|| "Unknown error".to_string())
        )));
    }

    let text_content = pi_ai::utils::text::content_text(&response.content, "");

    Ok((text_content, response.usage.clone()))
}

/// Generated compaction data ready to be persisted as a compaction entry.
#[derive(Clone, Debug)]
pub struct CompactResult {
    pub summary: String,
    pub tokens_before: f64,
    pub usage: Option<Usage>,
    pub retained_tail: Vec<AgentMessage>,
    pub details: CompactionDetails,
}

/// Generate compaction summary data from prepared session history.
pub fn compact<C: SimpleCompleter>(
    preparation: &CompactionPreparation,
    models: &C,
    model: &pi_ai::types::Model,
    custom_instructions: Option<&str>,
    thinking_level: Option<&str>,
    retry: Option<&RetryPolicy>,
) -> Result<CompactResult, CompactionError> {
    let mut summary: String;
    let summary_usage: Usage;

    if preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty() {
        let mut history_text = "No prior history.".to_string();
        let mut history_usage: Option<Usage> = None;
        if !preparation.messages_to_summarize.is_empty() {
            let (text, usage) = generate_summary_with_usage(
                &preparation.messages_to_summarize,
                models,
                model,
                preparation.settings.reserve_tokens,
                custom_instructions,
                preparation.previous_summary.as_deref(),
                thinking_level,
                retry,
            )?;
            history_text = text;
            history_usage = Some(usage);
        }
        let (prefix_text, prefix_usage) = generate_turn_prefix_summary(
            &preparation.turn_prefix_messages,
            models,
            model,
            preparation.settings.reserve_tokens,
            thinking_level,
            retry,
        )?;
        summary = format!("{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{prefix_text}");
        summary_usage = match history_usage {
            Some(history_usage) => combine_usage(&history_usage, &prefix_usage),
            None => prefix_usage,
        };
    } else {
        let (text, usage) = generate_summary_with_usage(
            &preparation.messages_to_summarize,
            models,
            model,
            preparation.settings.reserve_tokens,
            custom_instructions,
            preparation.previous_summary.as_deref(),
            thinking_level,
            retry,
        )?;
        summary = text;
        summary_usage = usage;
    }

    let (read_files, modified_files) = compute_file_lists(&preparation.file_ops);
    summary += &format_file_operations(&read_files, &modified_files);

    Ok(CompactResult {
        summary,
        tokens_before: preparation.tokens_before,
        usage: Some(summary_usage),
        retained_tail: preparation.retained_tail.clone(),
        details: CompactionDetails {
            read_files,
            modified_files,
        },
    })
}

/// Summarize the prefix of a split turn.
pub fn generate_turn_prefix_summary<C: SimpleCompleter>(
    messages: &[AgentMessage],
    models: &C,
    model: &pi_ai::types::Model,
    reserve_tokens: f64,
    thinking_level: Option<&str>,
    retry: Option<&RetryPolicy>,
) -> Result<(String, Usage), CompactionError> {
    let max_tokens = (0.5 * reserve_tokens).floor().min(
        if model.max_tokens > 0.0 {
            model.max_tokens
        } else {
            f64::INFINITY
        },
    );
    let llm_messages = convert_to_llm(messages);
    let conversation_text = serialize_conversation(&llm_messages);
    let prompt_text = format!(
        "<conversation>\n{conversation_text}\n</conversation>\n\n{TURN_PREFIX_SUMMARIZATION_PROMPT}"
    );
    let summarization_messages = pi_ai::types::Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_string()),
        messages: vec![Message::User(pi_ai::types::UserMessage {
            content: pi_ai::types::UserMessageContent::Blocks(vec![Content::Text(pi_ai::types::TextContent {
                text: prompt_text,
                text_signature: None,
            })]),
            timestamp: now_ms(),
        })],
        tools: None,
    };

    let completion_options = SimpleStreamOptions {
        max_tokens: Some(max_tokens),
        reasoning: if model.reasoning && thinking_level.is_some_and(|level| level != "off") {
            thinking_level.map(|level| level.to_string())
        } else {
            None
        },
        ..SimpleStreamOptions::default()
    };

    let response = complete_simple_with_retries(models, model, &summarization_messages, &completion_options, retry)
        .map_err(|message| {
            CompactionError::summarization_failed(format!("Turn prefix summarization failed: {message}"))
        })?;
    if response.stop_reason.as_str() == "aborted" {
        return Err(CompactionError::aborted(
            response
                .error_message
                .clone()
                .unwrap_or_else(|| "Turn prefix summarization aborted".to_string()),
        ));
    }
    if response.stop_reason.as_str() == "error" {
        return Err(CompactionError::summarization_failed(format!(
            "Turn prefix summarization failed: {}",
            response.error_message.clone().unwrap_or_else(|| "Unknown error".to_string())
        )));
    }

    Ok((
        pi_ai::utils::text::content_text(&response.content, ""),
        response.usage.clone(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::{AssistantMessage, StopReason, Usage, UsageCost};
    use crate::harness::session_types::{CompactionEntry, EntryBase, MessageEntry};
    use crate::types::{AgentMessage, ThinkingLevel};

    fn assistant_msg(text: &str, usage_tokens: f64) -> AgentMessage {
        AgentMessage::Llm(Message::Assistant(AssistantMessage {
            content: vec![Content::Text(pi_ai::types::TextContent {
                text: text.to_string(),
                text_signature: None,
            })],
            api: "test".to_string(),
            provider: "test".to_string(),
            model: "m".to_string(),
            response_model: None,
            response_id: None,
            usage: Usage {
                input: usage_tokens,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: usage_tokens,
                cost: UsageCost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    total: 0.0,
                },
            },
            stop_reason: StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 1.0,
        }))
    }

    fn user_msg(text: &str) -> AgentMessage {
        AgentMessage::Llm(Message::User(pi_ai::types::UserMessage {
            content: pi_ai::types::UserMessageContent::Text(text.to_string()),
            timestamp: 1.0,
        }))
    }

    fn msg_entry(message: AgentMessage, id: &str, seq: f64) -> Entry {
        Entry::Message(MessageEntry {
            base: EntryBase {
                id: id.to_string(),
                parent_id: None,
                seq,
                timestamp: 1.0,
                type_: "message".to_string(),
            },
            message,
            terminate: None,
        })
    }

    #[test]
    fn estimates_tokens_with_char_heuristic() {
        assert_eq!(estimate_tokens(&user_msg("hello world")), 3.0); // 11 chars / 4 ceil
        assert_eq!(estimate_tokens(&assistant_msg("abcd", 0.0)), 1.0);
    }

    #[test]
    fn context_tokens_falls_back_to_sum() {
        let usage = Usage {
            input: 3.0,
            output: 4.0,
            cache_read: 1.0,
            cache_write: 2.0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0.0,
            cost: UsageCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
        };
        assert_eq!(calculate_context_tokens(&usage), 10.0);
    }

    #[test]
    fn estimate_context_tokens_uses_last_usage() {
        let messages = vec![assistant_msg("aaaa", 100.0), user_msg("abcd")];
        let estimate = estimate_context_tokens(&messages);
        assert_eq!(estimate.usage_tokens, 100.0);
        assert_eq!(estimate.trailing_tokens, 1.0);
        assert_eq!(estimate.tokens, 101.0);
        assert_eq!(estimate.last_usage_index, Some(0));
    }

    #[test]
    fn should_compact_threshold() {
        let settings = CompactionSettings {
            enabled: true,
            reserve_tokens: 16384.0,
            keep_recent_tokens: 20000.0,
        };
        assert!(should_compact(120000.0, 128000.0, &settings));
        assert!(!should_compact(100000.0, 128000.0, &settings));
        let disabled = CompactionSettings {
            enabled: false,
            ..settings
        };
        assert!(!should_compact(120000.0, 128000.0, &disabled));
    }

    #[test]
    fn find_cut_point_keeps_recent_budget() {
        // 5 user turns, 10 tokens each (40 chars each).
        let entries: Vec<Entry> = (0..5)
            .map(|i| msg_entry(user_msg(&"x".repeat(40)), &format!("u{i}"), i as f64))
            .collect();
        let cut = find_cut_point(&entries, 0, 5, 12.0);
        // Walking back: 10 tokens accumulate; at >= 12.0 the first cut point
        // >= that index is chosen.
        assert!(cut.first_kept_entry_index < 5);
    }

    #[test]
    fn prepare_compaction_with_no_history_returns_none() {
        let settings = DEFAULT_COMPACTION_SETTINGS.clone();
        assert!(prepare_compaction(&[], &settings).unwrap().is_none());
        // Trailing compaction entry -> not applicable.
        let entries = vec![Entry::Compaction(CompactionEntry {
            base: EntryBase {
                id: "c1".to_string(),
                parent_id: None,
                seq: 1.0,
                timestamp: 1.0,
                type_: "compaction".to_string(),
            },
            summary: "s".to_string(),
            retained_tail: vec![],
            tokens_before: 10.0,
            details: None,
            usage: None,
        })];
        assert!(prepare_compaction(&entries, &settings).unwrap().is_none());
    }

    #[test]
    fn prepare_compaction_retains_tail_and_extracts_files() {
        let settings = CompactionSettings {
            enabled: true,
            reserve_tokens: 16384.0,
            keep_recent_tokens: 25.0,
        };
        let mut entries: Vec<Entry> = (0..5)
            .map(|i| msg_entry(user_msg(&"y".repeat(80)), &format!("u{i}"), i as f64))
            .collect();
        // Insert an assistant message with a read tool call into the
        // summarized history (early), so its file op is extracted.
        let mut assistant = match assistant_msg("early read", 200.0) {
            AgentMessage::Llm(Message::Assistant(a)) => a,
            _ => unreachable!(),
        };
        assistant.content.push(Content::ToolCall(pi_ai::types::ToolCall {
            id: "t1".to_string(),
            name: "read".to_string(),
            arguments: pi_protocol::Value::Map(vec![(
                "path".to_string(),
                pi_protocol::Value::String("src/a.ts".to_string()),
            )]),
            thought_signature: None,
            namespace: None,
        }));
        entries.insert(2, msg_entry(AgentMessage::Llm(Message::Assistant(assistant)), "a2", 2.0));

        let preparation = prepare_compaction(&entries, &settings).unwrap().unwrap();
        assert!(!preparation.messages_to_summarize.is_empty());
        assert!(preparation.file_ops.read.contains("src/a.ts"));
        assert!(!preparation.is_split_turn || preparation.tokens_before > 0.0);
    }

    #[test]
    fn file_ops_roundtrip() {
        let mut file_ops = create_file_ops();
        let mut assistant = match assistant_msg("", 0.0) {
            AgentMessage::Llm(Message::Assistant(a)) => a,
            _ => unreachable!(),
        };
        for (name, path) in [("read", "a"), ("write", "b"), ("edit", "c"), ("read", "a")] {
            assistant.content.push(Content::ToolCall(pi_ai::types::ToolCall {
                id: "t".to_string(),
                name: name.to_string(),
                arguments: pi_protocol::Value::Map(vec![(
                    "path".to_string(),
                    pi_protocol::Value::String(path.to_string()),
                )]),
                thought_signature: None,
                namespace: None,
            }));
        }
        extract_file_ops_from_message(&AgentMessage::Llm(Message::Assistant(assistant)), &mut file_ops);
        let (read_files, modified_files) = compute_file_lists(&file_ops);
        assert_eq!(read_files, vec!["a".to_string()]);
        assert_eq!(modified_files, vec!["b".to_string(), "c".to_string()]);
        let formatted = format_file_operations(&read_files, &modified_files);
        assert!(formatted.contains("<read-files>"));
        assert!(formatted.contains("<modified-files>"));
    }

    #[test]
    fn serialize_conversation_format() {
        let messages = vec![
            Message::User(pi_ai::types::UserMessage {
                content: pi_ai::types::UserMessageContent::Text("do it".to_string()),
                timestamp: 1.0,
            }),
            Message::Assistant(AssistantMessage {
                content: vec![
                    Content::Thinking(pi_ai::types::ThinkingContent {
                        thinking: "hmm".to_string(),
                        thinking_signature: None,
                        redacted: None,
                    }),
                    Content::Text(pi_ai::types::TextContent {
                        text: "done".to_string(),
                        text_signature: None,
                    }),
                ],
                api: "a".to_string(),
                provider: "p".to_string(),
                model: "m".to_string(),
                response_model: None,
                response_id: None,
                usage: empty_usage(),
                stop_reason: StopReason::Stop,
                deferred: None,
                error_message: None,
                raw_stop_reason: None,
                end_turn: None,
                timestamp: 1.0,
            }),
        ];
        let text = serialize_conversation(&messages);
        assert!(text.contains("[User]: do it"));
        assert!(text.contains("[Assistant thinking]: hmm"));
        assert!(text.contains("[Assistant]: done"));
    }

    fn empty_usage() -> Usage {
        Usage {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0.0,
            cost: UsageCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
        }
    }

    #[test]
    fn combine_usage_sums_and_preserves_optionals() {
        let a = Usage {
            input: 1.0,
            output: 2.0,
            cache_read: 3.0,
            cache_write: 4.0,
            cache_write_1h: None,
            reasoning: Some(5.0),
            total_tokens: 10.0,
            cost: UsageCost {
                input: 0.1,
                output: 0.2,
                cache_read: 0.3,
                cache_write: 0.4,
                total: 1.0,
            },
        };
        let b = Usage {
            cache_write_1h: Some(6.0),
            reasoning: None,
            ..a.clone()
        };
        let combined = combine_usage(&a, &b);
        assert_eq!(combined.input, 2.0);
        assert_eq!(combined.reasoning, Some(5.0));
        assert_eq!(combined.cache_write_1h, Some(6.0));
        assert_eq!(combined.cost.total, 2.0);
    }

    #[test]
    fn thinking_levels_are_comparable() {
        // ThinkingLevel is a plain String in Rust.
        let level: ThinkingLevel = "high".to_string();
        assert_eq!(level, "high");
        assert_ne!(level, "off");
    }
}
