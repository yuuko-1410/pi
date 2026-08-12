//! Context compaction for long sessions, port of `core/compaction/compaction.ts`.
//! LLM calls are synchronous: summarization runs through the StreamFn with a
//! completed stream (mirroring completeSummarization's produce + result()).

use pi_ai::types::{AssistantMessage, Context, Message, Model, SimpleStreamOptions, StreamOptions, ThinkingLevel, Usage, UsageCost};
use pi_ai::utils::estimate::{calculate_context_tokens, estimate_message_tokens};
use pi_ai::utils::retry::{retry_assistant_call, RetryCallbacks, RetryPolicy};
use pi_ai::utils::text::content_text;
use pi_ai::utils::uuid::uuidv7;
use pi_agent_core::types::{AgentMessage, StreamFn};

use crate::core::messages::convert_to_llm;
use crate::core::session_types::{build_session_context, SessionEntry};

use super::utils::{
    compute_file_lists, create_file_ops, extract_file_ops_from_message, format_file_operations,
    serialize_conversation, FileOperations, SUMMARIZATION_SYSTEM_PROMPT,
};

#[derive(Clone, Debug, PartialEq)]
pub struct CompactionDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

fn extract_file_operations(
    messages: &[AgentMessage],
    entries: &[SessionEntry],
    prev_compaction_index: i64,
) -> FileOperations {
    let mut file_ops = create_file_ops();

    // Collect from the previous compaction's details (if pi-generated).
    if prev_compaction_index >= 0 {
        if let Some(SessionEntry::Compaction { details, from_hook, .. }) = entries.get(prev_compaction_index as usize) {
            if *from_hook != Some(true) {
                if let Some(details) = details {
                    if let Some(fields) = details.as_map() {
                        if let Some(Value::Array(read_files)) = fields.iter().find(|(k, _)| k == "readFiles").map(|(_, v)| v) {
                            for file in read_files {
                                if let Some(file) = file.as_str() {
                                    file_ops.read.insert(file.to_string());
                                }
                            }
                        }
                        if let Some(Value::Array(modified_files)) = fields.iter().find(|(k, _)| k == "modifiedFiles").map(|(_, v)| v) {
                            for file in modified_files {
                                if let Some(file) = file.as_str() {
                                    file_ops.edited.insert(file.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Extract from tool calls in messages.
    for message in messages {
        if let AgentMessage::Llm(message) = message {
            extract_file_ops_from_message(message, &mut file_ops);
        }
    }

    file_ops
}

use pi_protocol::Value;

/// Extract an AgentMessage from an entry if it produces one.
fn get_message_from_entry_for_compaction(entry: &SessionEntry) -> Option<AgentMessage> {
    if matches!(entry, SessionEntry::Compaction { .. }) {
        return None;
    }
    crate::core::session_types::session_entry_to_context_messages(entry).into_iter().next()
}

#[derive(Clone, Debug)]
pub struct CompactionResult {
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: f64,
    pub estimated_tokens_after: Option<f64>,
    pub usage: Option<Usage>,
    pub details: Option<Value>,
}

fn combine_usage(first: &Usage, second: &Usage) -> Usage {
    Usage {
        input: first.input + second.input,
        output: first.output + second.output,
        cache_read: first.cache_read + second.cache_read,
        cache_write: first.cache_write + second.cache_write,
        cache_write_1h: if first.cache_write_1h.is_some() || second.cache_write_1h.is_some() {
            Some(first.cache_write_1h.unwrap_or(0.0) + second.cache_write_1h.unwrap_or(0.0))
        } else {
            None
        },
        reasoning: if first.reasoning.is_some() || second.reasoning.is_some() {
            Some(first.reasoning.unwrap_or(0.0) + second.reasoning.unwrap_or(0.0))
        } else {
            None
        },
        total_tokens: first.total_tokens + second.total_tokens,
        cost: UsageCost {
            input: first.cost.input + second.cost.input,
            output: first.cost.output + second.cost.output,
            cache_read: first.cost.cache_read + second.cost.cache_read,
            cache_write: first.cost.cache_write + second.cost.cache_write,
            total: first.cost.total + second.cost.total,
        },
    }
}

#[derive(Clone, Debug)]
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

/// Get usage from an assistant message if available (skips aborted, error,
/// and all-zero usage messages).
fn get_assistant_usage(message: &AgentMessage) -> Option<Usage> {
    if let AgentMessage::Llm(Message::Assistant(assistant)) = message {
        if assistant.stop_reason != pi_ai::types::StopReason::Aborted
            && assistant.stop_reason != pi_ai::types::StopReason::Error
            && calculate_context_tokens(&assistant.usage) > 0.0
        {
            return Some(assistant.usage.clone());
        }
    }
    None
}

/// Find the last valid assistant message usage from session entries.
pub fn get_last_assistant_usage(entries: &[SessionEntry]) -> Option<Usage> {
    for entry in entries.iter().rev() {
        if let SessionEntry::Message { .. } = entry {
            for message in crate::core::session_types::session_entry_to_context_messages(entry) {
                if let Some(usage) = get_assistant_usage(&message) {
                    return Some(usage);
                }
            }
        }
    }
    None
}

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

/// Estimate context tokens from messages, using the last assistant usage when
/// available.
pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    let usage_info = get_last_assistant_usage_info(messages);

    let Some((usage, index)) = usage_info else {
        let mut estimated = 0.0;
        for message in messages {
            estimated += estimate_message_tokens(&message_to_llm(message));
        }
        return ContextUsageEstimate {
            tokens: estimated,
            usage_tokens: 0.0,
            trailing_tokens: estimated,
            last_usage_index: None,
        };
    };

    let usage_tokens = calculate_context_tokens(&usage);
    let mut trailing_tokens = 0.0;
    for message in &messages[index + 1..] {
        trailing_tokens += estimate_message_tokens(&message_to_llm(message));
    }

    ContextUsageEstimate {
        tokens: usage_tokens + trailing_tokens,
        usage_tokens,
        trailing_tokens,
        last_usage_index: Some(index),
    }
}

/// Map an AgentMessage to its LLM form for token estimation.
fn message_to_llm(message: &AgentMessage) -> Message {
    match message {
        AgentMessage::Llm(message) => message.clone(),
        AgentMessage::Custom(_) => {
            // Custom messages are skipped by estimate; an empty user message
            // estimates to 0 tokens.
            Message::User(pi_ai::types::UserMessage {
                content: pi_ai::types::UserMessageContent::Text(String::new()),
                timestamp: 0.0,
            })
        }
    }
}

/// Check whether compaction should trigger based on context usage.
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
                    pi_ai::types::Content::Text(text) => chars += text.text.len() as f64,
                    pi_ai::types::Content::Image(_) => chars += ESTIMATED_IMAGE_CHARS,
                    _ => {}
                }
            }
            chars
        }
    }
}

/// Estimate the token count for a message using the chars/4 heuristic.
pub fn estimate_tokens(message: &AgentMessage) -> f64 {
    let mut chars = 0.0;

    match message {
        AgentMessage::Llm(Message::User(user)) => {
            chars = estimate_text_and_image_content_chars(&user.content);
            (chars / 4.0).ceil()
        }
        AgentMessage::Llm(Message::Assistant(assistant)) => {
            for block in &assistant.content {
                match block {
                    pi_ai::types::Content::Text(text) => chars += text.text.len() as f64,
                    pi_ai::types::Content::Thinking(thinking) => chars += thinking.thinking.len() as f64,
                    pi_ai::types::Content::ToolCall(tool_call) => {
                        chars += tool_call.name.len() as f64
                            + pi_ai::utils::json::json_stringify(&tool_call.arguments).len() as f64;
                    }
                    _ => {}
                }
            }
            (chars / 4.0).ceil()
        }
        AgentMessage::Llm(Message::ToolResult(tool_result)) => {
            chars = estimate_text_and_image_content_chars(&pi_ai::types::UserMessageContent::Blocks(
                tool_result.content.clone(),
            ));
            (chars / 4.0).ceil()
        }
        AgentMessage::Custom(_) => 0.0,
    }
}

fn is_cut_point_message(message: &Message) -> bool {
    match message {
        Message::User(_) | Message::Assistant(_) => true,
        Message::ToolResult(_) => false,
    }
}

fn is_turn_start_message(message: &Message) -> bool {
    matches!(message, Message::User(_)) && !matches!(message, Message::ToolResult(_))
}

fn is_turn_start_entry(entry: &SessionEntry) -> bool {
    if matches!(entry, SessionEntry::Compaction { .. }) {
        return false;
    }
    crate::core::session_types::session_entry_to_context_messages(entry)
        .iter()
        .any(|message| matches!(message, AgentMessage::Llm(message) if is_turn_start_message(message)))
}

/// Find valid cut points: indices of context-visible user-like or assistant
/// messages (never tool results).
fn find_valid_cut_points(entries: &[SessionEntry], start_index: usize, end_index: usize) -> Vec<usize> {
    let mut cut_points = Vec::new();
    for index in start_index..end_index {
        let entry = &entries[index];
        if matches!(entry, SessionEntry::Compaction { .. }) {
            continue;
        }
        if crate::core::session_types::session_entry_to_context_messages(entry)
            .iter()
            .any(|message| matches!(message, AgentMessage::Llm(message) if is_cut_point_message(message)))
        {
            cut_points.push(index);
        }
    }
    cut_points
}

/// Find the context-visible user-role message that starts the turn containing
/// the given entry index; -1 if none before the index.
pub fn find_turn_start_index(entries: &[SessionEntry], entry_index: usize, start_index: usize) -> i64 {
    for index in (start_index..=entry_index).rev() {
        if is_turn_start_entry(&entries[index]) {
            return index as i64;
        }
    }
    -1
}

#[derive(Clone, Debug, PartialEq)]
pub struct CutPointResult {
    pub first_kept_entry_index: usize,
    pub turn_start_index: i64,
    pub is_split_turn: bool,
}

/// Find the cut point keeping approximately `keep_recent_tokens`.
pub fn find_cut_point(
    entries: &[SessionEntry],
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

    let mut accumulated_tokens = 0.0;
    let mut cut_index = cut_points[0];

    let mut index = end_index;
    while index > start_index {
        index -= 1;
        let entry = &entries[index];
        let message_tokens: f64 = crate::core::session_types::session_entry_to_context_messages(entry)
            .iter()
            .map(estimate_tokens)
            .sum();
        if message_tokens == 0.0 {
            continue;
        }
        accumulated_tokens += message_tokens;

        if accumulated_tokens >= keep_recent_tokens {
            for point in &cut_points {
                if *point >= index {
                    cut_index = *point;
                    break;
                }
            }
            break;
        }
    }

    // Scan backwards to include adjacent metadata entries that do not affect
    // context.
    while cut_index > start_index {
        let prev_entry = &entries[cut_index - 1];
        if matches!(prev_entry, SessionEntry::Compaction { .. })
            || !crate::core::session_types::session_entry_to_context_messages(prev_entry).is_empty()
        {
            break;
        }
        cut_index -= 1;
    }

    let cut_entry = &entries[cut_index];
    let starts_turn = is_turn_start_entry(cut_entry);
    let turn_start_index = if starts_turn {
        -1
    } else {
        find_turn_start_index(entries, cut_index, start_index)
    };

    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index != -1,
    }
}

const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

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

const UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

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

fn create_summarization_options(
    model: &Model,
    max_tokens: f64,
    api_key: Option<&str>,
    headers: Option<&[(String, String)]>,
    env: Option<&[(String, String)]>,
    thinking_level: Option<&ThinkingLevel>,
) -> SimpleStreamOptions {
    let _ = model;
    let mut options = SimpleStreamOptions {
        stream: StreamOptions {
            request: pi_ai::types::ProviderRequestOptions {
                api_key: api_key.map(|value| value.to_string()),
                env: env.map(|value| value.to_vec()),
                headers: headers.map(|value| {
                    value
                        .iter()
                        .map(|(key, value)| (key.clone(), Some(value.clone())))
                        .collect()
                }),
                ..Default::default()
            },
            max_tokens: Some(max_tokens),
            ..Default::default()
        },
        reasoning: None,
        deferred: None,
        thinking_budgets: None,
    };
    if model.reasoning && thinking_level.is_some_and(|level| level != "off") {
        options.reasoning = thinking_level.cloned();
    }
    options
}

/// Shared choke point for compaction/branch-summary summarization calls.
pub fn complete_summarization(
    model: &Model,
    context: &Context,
    options: &SimpleStreamOptions,
    signal: Option<&pi_ai::utils::abort::CancellationToken>,
    stream_fn: Option<&StreamFn>,
    retry: Option<&RetryPolicy>,
    callbacks: Option<RetryCallbacks>,
) -> AssistantMessage {
    // Summaries are standalone requests: isolate routing and avoid cache
    // writes that cannot be reused.
    let mut request_options = options.clone();
    request_options.stream.cache_retention = Some("none".to_string());
    request_options.stream.session_id = Some(uuidv7());

    let produce = || -> AssistantMessage {
        match stream_fn {
            Some(stream_fn) => stream_fn(model, context, Some(&request_options)).result(),
            None => {
                // ponytail: without a stream_fn there is no default
                // completeSimple analog at this layer; callers always provide
                // the agent's stream function.
                panic!("complete_summarization requires a stream function")
            }
        }
    };
    retry_assistant_call(produce, retry, signal, callbacks)
}

pub struct SummaryCallOptions<'a> {
    pub api_key: Option<&'a str>,
    pub headers: Option<&'a [(String, String)]>,
    pub signal: Option<&'a pi_ai::utils::abort::CancellationToken>,
    pub custom_instructions: Option<&'a str>,
    pub previous_summary: Option<&'a str>,
    pub thinking_level: Option<&'a ThinkingLevel>,
    pub stream_fn: Option<&'a StreamFn>,
    pub env: Option<&'a [(String, String)]>,
    pub retry: Option<&'a RetryPolicy>,
    pub callbacks: Option<RetryCallbacks>,
}

/// Generate or update a conversation summary and return its provider usage.
pub fn generate_summary_with_usage(
    current_messages: &[AgentMessage],
    model: &Model,
    reserve_tokens: f64,
    options: &SummaryCallOptions,
) -> Result<(String, Usage), String> {
    let max_tokens = (0.8 * reserve_tokens).floor().min(if model.max_tokens > 0.0 {
        model.max_tokens
    } else {
        f64::INFINITY
    });

    let mut base_prompt = if options.previous_summary.is_some() {
        UPDATE_SUMMARIZATION_PROMPT.to_string()
    } else {
        SUMMARIZATION_PROMPT.to_string()
    };
    if let Some(custom_instructions) = options.custom_instructions {
        base_prompt = format!("{base_prompt}\n\nAdditional focus: {custom_instructions}");
    }

    let llm_messages = convert_to_llm(current_messages);
    let conversation_text = serialize_conversation(&llm_messages);

    let mut prompt_text = format!("<conversation>\n{conversation_text}\n</conversation>\n\n");
    if let Some(previous_summary) = options.previous_summary {
        prompt_text.push_str(&format!("<previous-summary>\n{previous_summary}\n</previous-summary>\n\n"));
    }
    prompt_text.push_str(&base_prompt);

    let summarization_messages = vec![Message::User(pi_ai::types::UserMessage {
        content: pi_ai::types::UserMessageContent::Blocks(vec![pi_ai::types::Content::Text(
            pi_ai::types::TextContent {
                text: prompt_text,
                text_signature: None,
            },
        )]),
        timestamp: 0.0,
    })];

    let completion_options = create_summarization_options(
        model,
        max_tokens,
        options.api_key,
        options.headers,
        options.env,
        options.thinking_level,
    );

    let response = complete_summarization(
        model,
        &Context {
            system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_string()),
            messages: summarization_messages,
            tools: None,
        },
        &completion_options,
        options.signal,
        options.stream_fn,
        options.retry,
        None,
    );

    if response.stop_reason == pi_ai::types::StopReason::Error {
        return Err(format!(
            "Summarization failed: {}",
            response.error_message.clone().unwrap_or_else(|| "Unknown error".to_string())
        ));
    }

    let text = content_text(&response.content, " ");
    Ok((text, response.usage))
}

/// Generate a summary of the conversation using the LLM.
pub fn generate_summary(
    current_messages: &[AgentMessage],
    model: &Model,
    reserve_tokens: f64,
    options: &SummaryCallOptions,
) -> Result<String, String> {
    generate_summary_with_usage(current_messages, model, reserve_tokens, options).map(|(text, _)| text)
}

#[derive(Clone, Debug)]
pub struct CompactionPreparation {
    pub first_kept_entry_id: String,
    pub messages_to_summarize: Vec<AgentMessage>,
    pub turn_prefix_messages: Vec<AgentMessage>,
    pub is_split_turn: bool,
    pub tokens_before: f64,
    pub previous_summary: Option<String>,
    pub file_ops: FileOperations,
    pub settings: CompactionSettings,
}

/// Prepare compaction without running LLM calls.
pub fn prepare_compaction(
    path_entries: &[SessionEntry],
    settings: &CompactionSettings,
) -> Option<CompactionPreparation> {
    if path_entries.last().is_some_and(|entry| matches!(entry, SessionEntry::Compaction { .. })) {
        return None;
    }

    let mut prev_compaction_index: i64 = -1;
    for (index, entry) in path_entries.iter().enumerate().rev() {
        if matches!(entry, SessionEntry::Compaction { .. }) {
            prev_compaction_index = index as i64;
            break;
        }
    }

    let mut previous_summary: Option<String> = None;
    let mut boundary_start = 0usize;
    if prev_compaction_index >= 0 {
        if let Some(SessionEntry::Compaction { summary, first_kept_entry_id, .. }) =
            path_entries.get(prev_compaction_index as usize)
        {
            previous_summary = Some(summary.clone());
            let first_kept_entry_index = path_entries
                .iter()
                .position(|entry| entry.id() == first_kept_entry_id);
            boundary_start = match first_kept_entry_index {
                Some(index) => index,
                None => prev_compaction_index as usize + 1,
            };
        }
    }
    let boundary_end = path_entries.len();

    let entries = path_entries.to_vec();
    let by_id = crate::core::session_types::build_entry_index(&entries);
    let context = build_session_context(&entries, None, &by_id);
    let tokens_before = estimate_context_tokens(&context.messages).tokens;

    let cut_point = find_cut_point(path_entries, boundary_start, boundary_end, settings.keep_recent_tokens);

    let first_kept_entry = path_entries.get(cut_point.first_kept_entry_index);
    let Some(first_kept_entry) = first_kept_entry else {
        return None; // Session needs migration
    };
    let first_kept_entry_id = first_kept_entry.id().to_string();

    let history_end = if cut_point.is_split_turn {
        cut_point.turn_start_index as usize
    } else {
        cut_point.first_kept_entry_index
    };

    let mut messages_to_summarize: Vec<AgentMessage> = Vec::new();
    for index in boundary_start..history_end {
        if let Some(message) = get_message_from_entry_for_compaction(&path_entries[index]) {
            messages_to_summarize.push(message);
        }
    }

    let mut turn_prefix_messages: Vec<AgentMessage> = Vec::new();
    if cut_point.is_split_turn {
        for index in cut_point.turn_start_index as usize..cut_point.first_kept_entry_index {
            if let Some(message) = get_message_from_entry_for_compaction(&path_entries[index]) {
                turn_prefix_messages.push(message);
            }
        }
    }

    if messages_to_summarize.is_empty() && turn_prefix_messages.is_empty() {
        return None;
    }

    let mut file_ops = extract_file_operations(&messages_to_summarize, path_entries, prev_compaction_index);
    if cut_point.is_split_turn {
        for message in &turn_prefix_messages {
            if let AgentMessage::Llm(message) = message {
                extract_file_ops_from_message(message, &mut file_ops);
            }
        }
    }

    Some(CompactionPreparation {
        first_kept_entry_id,
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn: cut_point.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        settings: settings.clone(),
    })
}

const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = "This is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.

Summarize the prefix to provide context for the retained suffix:

## Original Request
[What did the user ask for in this turn?]

## Early Progress
- [Key decisions and work done in the prefix]

## Context for Suffix
- [Information needed to understand the retained recent work]

Be concise. Focus on what's needed to understand the kept suffix.";

/// Generate a summary for a turn prefix (when splitting a turn).
fn generate_turn_prefix_summary(
    messages: &[AgentMessage],
    model: &Model,
    reserve_tokens: f64,
    options: &SummaryCallOptions,
) -> Result<(String, Usage), String> {
    let max_tokens = (0.5 * reserve_tokens).floor().min(if model.max_tokens > 0.0 {
        model.max_tokens
    } else {
        f64::INFINITY
    });
    let llm_messages = convert_to_llm(messages);
    let conversation_text = serialize_conversation(&llm_messages);
    let prompt_text = format!(
        "<conversation>\n{conversation_text}\n</conversation>\n\n{TURN_PREFIX_SUMMARIZATION_PROMPT}"
    );
    let summarization_messages = vec![Message::User(pi_ai::types::UserMessage {
        content: pi_ai::types::UserMessageContent::Blocks(vec![pi_ai::types::Content::Text(
            pi_ai::types::TextContent {
                text: prompt_text,
                text_signature: None,
            },
        )]),
        timestamp: 0.0,
    })];

    let response = complete_summarization(
        model,
        &Context {
            system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_string()),
            messages: summarization_messages,
            tools: None,
        },
        &create_summarization_options(
            model,
            max_tokens,
            options.api_key,
            options.headers,
            options.env,
            options.thinking_level,
        ),
        options.signal,
        options.stream_fn,
        options.retry,
        None,
    );

    if response.stop_reason == pi_ai::types::StopReason::Error {
        return Err(format!(
            "Turn prefix summarization failed: {}",
            response.error_message.clone().unwrap_or_else(|| "Unknown error".to_string())
        ));
    }

    Ok((content_text(&response.content, " "), response.usage))
}

/// Generate summaries for compaction using prepared data.
pub fn compact(
    preparation: &CompactionPreparation,
    model: &Model,
    options: &SummaryCallOptions,
) -> Result<CompactionResult, String> {
    let summary_options = SummaryCallOptions {
        api_key: options.api_key,
        headers: options.headers,
        signal: options.signal,
        custom_instructions: options.custom_instructions,
        previous_summary: preparation.previous_summary.as_deref(),
        thinking_level: options.thinking_level,
        stream_fn: options.stream_fn,
        env: options.env,
        retry: options.retry,
        callbacks: None,
    };

    let mut summary: String;
    let summary_usage: Usage;

    if preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty() {
        let mut history_text = "No prior history.".to_string();
        let mut history_usage: Option<Usage> = None;
        if !preparation.messages_to_summarize.is_empty() {
            let (text, usage) = generate_summary_with_usage(
                &preparation.messages_to_summarize,
                model,
                preparation.settings.reserve_tokens,
                &summary_options,
            )?;
            history_text = text;
            history_usage = Some(usage);
        }
        let turn_prefix_result = generate_turn_prefix_summary(
            &preparation.turn_prefix_messages,
            model,
            preparation.settings.reserve_tokens,
            &summary_options,
        )?;
        summary = format!(
            "{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{}",
            turn_prefix_result.0
        );
        summary_usage = match history_usage {
            Some(history_usage) => combine_usage(&history_usage, &turn_prefix_result.1),
            None => turn_prefix_result.1,
        };
    } else {
        let result = generate_summary_with_usage(
            &preparation.messages_to_summarize,
            model,
            preparation.settings.reserve_tokens,
            &summary_options,
        )?;
        summary = result.0;
        summary_usage = result.1;
    }

    // Compute file lists and append to the summary.
    let (read_files, modified_files) = compute_file_lists(&preparation.file_ops);
    summary.push_str(&format_file_operations(&read_files, &modified_files));

    if preparation.first_kept_entry_id.is_empty() {
        return Err("First kept entry has no UUID - session may need migration".to_string());
    }

    Ok(CompactionResult {
        summary,
        first_kept_entry_id: preparation.first_kept_entry_id.clone(),
        tokens_before: preparation.tokens_before,
        estimated_tokens_after: None,
        usage: Some(summary_usage),
        details: Some(Value::Map(vec![
            (
                "readFiles".to_string(),
                Value::Array(read_files.into_iter().map(Value::String).collect()),
            ),
            (
                "modifiedFiles".to_string(),
                Value::Array(modified_files.into_iter().map(Value::String).collect()),
            ),
        ])),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session_types::SessionEntryBase;
    use crate::core::session_types::SessionMessage;

    fn user_entry(id: &str, text: &str, parent: Option<&str>) -> SessionEntry {
        SessionEntry::Message {
            base: SessionEntryBase {
                id: id.to_string(),
                parent_id: parent.map(|value| value.to_string()),
                timestamp: "2024-01-01T00:00:00.000Z".to_string(),
            },
            message: SessionMessage::Llm(Message::User(pi_ai::types::UserMessage {
                content: pi_ai::types::UserMessageContent::Text(text.to_string()),
                timestamp: 0.0,
            })),
        }
    }

    fn assistant_entry(id: &str, text: &str, parent: Option<&str>) -> SessionEntry {
        SessionEntry::Message {
            base: SessionEntryBase {
                id: id.to_string(),
                parent_id: parent.map(|value| value.to_string()),
                timestamp: "2024-01-01T00:00:00.000Z".to_string(),
            },
            message: SessionMessage::Llm(Message::Assistant(AssistantMessage {
                content: vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
                    text: text.to_string(),
                    text_signature: None,
                })],
                api: "api".into(),
                provider: "p".into(),
                model: "m".into(),
                response_model: None,
                response_id: None,
                usage: Usage {
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
                },
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
    fn estimate_tokens_chars_over_four() {
        let message = AgentMessage::Llm(Message::User(pi_ai::types::UserMessage {
            content: pi_ai::types::UserMessageContent::Text("x".repeat(10)),
            timestamp: 0.0,
        }));
        assert_eq!(estimate_tokens(&message), 3.0); // ceil(10/4)
    }

    #[test]
    fn should_compact_threshold() {
        let settings = DEFAULT_COMPACTION_SETTINGS;
        // contextWindow - reserve = 50000 - 16384 = 33616.
        assert!(!should_compact(30000.0, 50000.0, &settings));
        assert!(should_compact(40000.0, 50000.0, &settings));
        let disabled = CompactionSettings {
            enabled: false,
            ..DEFAULT_COMPACTION_SETTINGS
        };
        assert!(!should_compact(40000.0, 50000.0, &disabled));
    }

    #[test]
    fn find_cut_point_keeps_recent() {
        // 5 user messages of 1000 chars each (~250 tokens each).
        let mut entries = Vec::new();
        let mut parent: Option<String> = None;
        for index in 0..5 {
            entries.push(user_entry(&format!("e{index}"), &"x".repeat(1000), parent.as_deref()));
            parent = Some(format!("e{index}"));
        }
        // keep 400 tokens: cuts after ~2 messages from the end.
        let result = find_cut_point(&entries, 0, entries.len(), 400.0);
        assert_eq!(result.first_kept_entry_index, 3);
        assert!(!result.is_split_turn);
    }

    #[test]
    fn cut_point_never_at_tool_result() {
        let entries = vec![
            user_entry("e0", "hi", None),
            assistant_entry("e1", "response", Some("e0")),
        ];
        let result = find_cut_point(&entries, 0, entries.len(), 1.0);
        assert_eq!(result.first_kept_entry_index, 1);
        // assistant cut: turn start is the user entry -> split turn.
        assert_eq!(result.turn_start_index, 0);
        assert!(result.is_split_turn);
    }

    #[test]
    fn get_last_assistant_usage_skips_zero() {
        let entries = vec![
            user_entry("e0", "hi", None),
            assistant_entry("e1", "resp", Some("e0")),
        ];
        // Zero usage is skipped.
        assert_eq!(get_last_assistant_usage(&entries), None);
    }

    #[test]
    fn prepare_compaction_basic() {
        let mut entries = Vec::new();
        let mut parent: Option<String> = None;
        for index in 0..10 {
            entries.push(user_entry(&format!("e{index}"), &"y".repeat(10000), parent.as_deref()));
            parent = Some(format!("e{index}"));
        }
        let settings = DEFAULT_COMPACTION_SETTINGS;
        let preparation = prepare_compaction(&entries, &settings).unwrap();
        assert!(!preparation.first_kept_entry_id.is_empty());
        assert!(preparation.tokens_before > 0.0);
        assert!(!preparation.messages_to_summarize.is_empty());
    }

    #[test]
    fn prepare_compaction_after_compaction_boundary() {
        let entries = vec![
            user_entry("e0", "hi", None),
            SessionEntry::Compaction {
                base: SessionEntryBase {
                    id: "c1".into(),
                    parent_id: Some("e0".into()),
                    timestamp: "2024-01-01T00:00:00.000Z".into(),
                },
                summary: "old".into(),
                first_kept_entry_id: "e0".into(),
                tokens_before: 0.0,
                details: None,
                usage: None,
                from_hook: None,
                first_kept_entry_index: None,
            },
            user_entry("e1", "more", Some("c1")),
        ];
        let settings = DEFAULT_COMPACTION_SETTINGS;
        // The trailing compaction entry blocks a new compaction.
        assert!(prepare_compaction(&entries, &settings).is_none());
    }
}
