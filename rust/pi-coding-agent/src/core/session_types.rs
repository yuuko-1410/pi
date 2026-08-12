//! Session file entry types and pure session logic, port of the type and
//! pure-function surface of `core/session-manager.ts` (the `SessionManager`
//! class itself lives in `session_manager.rs`).
//!
//! JSON shapes follow the JS `JSON.stringify` output: fields are emitted in
//! construction order and `undefined` fields are omitted. Message payload
//! round-trips reuse the pi-sqlite encoding (see pi-sqlite/src/util.rs) with
//! the same lossy mapping for unknown content blocks.

use pi_ai::types::{Message, Usage};
use pi_protocol::Value;
use pi_ai::utils::json::json_stringify;
use crate::core::messages::{
    parse_timestamp_ms, BashExecutionMessage, BranchSummaryMessage, CompactionSummaryMessage, ContentOrText,
    CustomMessage,
};

pub const CURRENT_SESSION_VERSION: i64 = 3;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct SessionHeader {
    pub version: Option<i64>, // v1 sessions don't have this
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    pub parent_session: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionEntryBase {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
}

/// Message payload of a session message entry. Custom roles (bashExecution,
/// custom, branchSummary, compactionSummary) map to their structs; unknown
/// roles are preserved verbatim like the JS parser does (session files are
/// parsed without validation).
#[derive(Clone, Debug, PartialEq)]
pub enum SessionMessage {
    Llm(Message),
    Bash(BashExecutionMessage),
    Custom(CustomMessage),
    BranchSummary(BranchSummaryMessage),
    CompactionSummary(CompactionSummaryMessage),
    Unknown(Value),
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionEntry {
    Message {
        base: SessionEntryBase,
        message: SessionMessage,
    },
    ThinkingLevelChange {
        base: SessionEntryBase,
        thinking_level: String,
    },
    ModelChange {
        base: SessionEntryBase,
        provider: String,
        model_id: String,
    },
    Compaction {
        base: SessionEntryBase,
        summary: String,
        first_kept_entry_id: String,
        tokens_before: f64,
        details: Option<Value>,
        usage: Option<Usage>,
        from_hook: Option<bool>,
        /// v1-only numeric index, migrated to first_kept_entry_id; never
        /// emitted after migration.
        first_kept_entry_index: Option<i64>,
    },
    BranchSummary {
        base: SessionEntryBase,
        from_id: String,
        summary: String,
        details: Option<Value>,
        usage: Option<Usage>,
        from_hook: Option<bool>,
    },
    Custom {
        base: SessionEntryBase,
        custom_type: String,
        data: Option<Value>,
    },
    CustomMessage {
        base: SessionEntryBase,
        custom_type: String,
        content: ContentOrText,
        details: Option<Value>,
        display: bool,
    },
    Label {
        base: SessionEntryBase,
        target_id: String,
        label: Option<String>,
    },
    SessionInfo {
        base: SessionEntryBase,
        name: Option<String>,
    },
}

impl SessionEntry {
    pub fn id(&self) -> &str {
        &self.base().id
    }

    pub fn parent_id(&self) -> Option<&str> {
        self.base().parent_id.as_deref()
    }

    pub fn timestamp(&self) -> &str {
        &self.base().timestamp
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            SessionEntry::Message { .. } => "message",
            SessionEntry::ThinkingLevelChange { .. } => "thinking_level_change",
            SessionEntry::ModelChange { .. } => "model_change",
            SessionEntry::Compaction { .. } => "compaction",
            SessionEntry::BranchSummary { .. } => "branch_summary",
            SessionEntry::Custom { .. } => "custom",
            SessionEntry::CustomMessage { .. } => "custom_message",
            SessionEntry::Label { .. } => "label",
            SessionEntry::SessionInfo { .. } => "session_info",
        }
    }

    fn base(&self) -> &SessionEntryBase {
        match self {
            SessionEntry::Message { base, .. }
            | SessionEntry::ThinkingLevelChange { base, .. }
            | SessionEntry::ModelChange { base, .. }
            | SessionEntry::Compaction { base, .. }
            | SessionEntry::BranchSummary { base, .. }
            | SessionEntry::Custom { base, .. }
            | SessionEntry::CustomMessage { base, .. }
            | SessionEntry::Label { base, .. }
            | SessionEntry::SessionInfo { base, .. } => base,
        }
    }

    pub fn with_parent(&self, parent_id: Option<String>) -> SessionEntry {
        let mut clone = self.clone();
        match &mut clone {
            SessionEntry::Message { base, .. }
            | SessionEntry::ThinkingLevelChange { base, .. }
            | SessionEntry::ModelChange { base, .. }
            | SessionEntry::Compaction { base, .. }
            | SessionEntry::BranchSummary { base, .. }
            | SessionEntry::Custom { base, .. }
            | SessionEntry::CustomMessage { base, .. }
            | SessionEntry::Label { base, .. }
            | SessionEntry::SessionInfo { base, .. } => base.parent_id = parent_id,
        }
        clone
    }
}

/// A raw file entry: the session header or a session entry.
#[derive(Clone, Debug, PartialEq)]
pub enum FileEntry {
    Header(SessionHeader),
    Entry(SessionEntry),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionTreeNode {
    pub entry: SessionEntry,
    pub children: Vec<SessionTreeNode>,
    pub label: Option<String>,
    pub label_timestamp: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionContext {
    pub messages: Vec<pi_agent_core::types::AgentMessage>,
    pub thinking_level: String,
    pub model: Option<(String, String)>, // (provider, model_id)
}

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub path: String,
    pub id: String,
    pub cwd: String,
    pub name: Option<String>,
    pub parent_session_path: Option<String>,
    pub created_ms: f64,
    pub modified_ms: f64,
    pub message_count: i64,
    pub first_message: String,
    pub all_messages_text: String,
}

// ---------------------------------------------------------------------------
// IDs
// ---------------------------------------------------------------------------

pub fn create_session_id() -> String {
    pi_ai::utils::uuid::uuidv7()
}

pub fn assert_valid_session_id(id: &str) -> Result<(), String> {
    let valid = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && id
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && id
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric());
    if valid {
        Ok(())
    } else {
        Err("Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character".to_string())
    }
}

/// Generate a unique short ID (8 hex chars, collision-checked against `has`).
pub fn generate_id(has: &dyn Fn(&str) -> bool) -> String {
    for _ in 0..100 {
        let id = format!("{:08x}", random_u32());
        if !has(&id) {
            return id;
        }
    }
    // Fallback: full v7 UUID if somehow we have collisions.
    create_session_id()
}

/// Random 32 bits from the OS entropy source.
fn random_u32() -> u32 {
    let mut bytes = [0u8; 4];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = file.read_exact(&mut bytes);
    }
    u32::from_be_bytes(bytes)
}

// ---------------------------------------------------------------------------
// JSON round trips
// ---------------------------------------------------------------------------

fn kv(key: &str, value: Value) -> (String, Value) {
    (key.to_string(), value)
}

fn str(value: &str) -> Value {
    Value::String(value.to_string())
}

fn num(value: f64) -> Value {
    Value::Number(value)
}

fn base_entries(base: &SessionEntryBase, type_: &str) -> Vec<(String, Value)> {
    vec![
        kv("type", str(type_)),
        kv("id", str(&base.id)),
        kv(
            "parentId",
            match &base.parent_id {
                Some(parent_id) => str(parent_id),
                None => Value::Null,
            },
        ),
        kv("timestamp", str(&base.timestamp)),
    ]
}

pub fn session_message_to_json(message: &SessionMessage) -> Value {
    match message {
        SessionMessage::Llm(message) => message_to_json(message),
        SessionMessage::Bash(bash) => {
            let mut entries = vec![
                kv("role", str("bashExecution")),
                kv("command", str(&bash.command)),
                kv("output", str(&bash.output)),
            ];
            if let Some(exit_code) = bash.exit_code {
                entries.push(kv("exitCode", Value::Number(exit_code as f64)));
            }
            entries.push(kv("cancelled", Value::Bool(bash.cancelled)));
            entries.push(kv("truncated", Value::Bool(bash.truncated)));
            if let Some(path) = &bash.full_output_path {
                entries.push(kv("fullOutputPath", str(path)));
            }
            entries.push(kv("timestamp", num(bash.timestamp)));
            if let Some(exclude) = bash.exclude_from_context {
                entries.push(kv("excludeFromContext", Value::Bool(exclude)));
            }
            Value::Map(entries)
        }
        SessionMessage::Custom(custom) => {
            let mut entries = vec![
                kv("role", str("custom")),
                kv("customType", str(&custom.custom_type)),
                kv("content", content_or_text_to_json(&custom.content)),
                kv("display", Value::Bool(custom.display)),
            ];
            if let Some(details) = &custom.details {
                entries.push(kv("details", details.clone()));
            }
            entries.push(kv("timestamp", num(custom.timestamp)));
            Value::Map(entries)
        }
        SessionMessage::BranchSummary(branch) => Value::Map(vec![
            kv("role", str("branchSummary")),
            kv("summary", str(&branch.summary)),
            kv("fromId", str(&branch.from_id)),
            kv("timestamp", num(branch.timestamp)),
        ]),
        SessionMessage::CompactionSummary(compaction) => Value::Map(vec![
            kv("role", str("compactionSummary")),
            kv("summary", str(&compaction.summary)),
            kv("tokensBefore", num(compaction.tokens_before)),
            kv("timestamp", num(compaction.timestamp)),
        ]),
        SessionMessage::Unknown(value) => value.clone(),
    }
}

fn content_or_text_to_json(content: &ContentOrText) -> Value {
    match content {
        ContentOrText::Text(text) => str(text),
        ContentOrText::Blocks(blocks) => Value::Array(blocks.iter().map(content_to_json).collect()),
    }
}

fn content_to_json(content: &pi_ai::types::Content) -> Value {
    match content {
        pi_ai::types::Content::Text(text) => Value::Map(vec![kv("type", str("text")), kv("text", str(&text.text))]),
        pi_ai::types::Content::Image(image) => Value::Map(vec![
            kv("type", str("image")),
            kv("data", str(&image.data)),
            kv("mimeType", str(&image.mime_type)),
        ]),
        other => Value::Map(vec![kv("type", str("unknown"))]),
    }
}

/// Message -> JSON with the JS `JSON.stringify` shape. See pi-sqlite util.rs.
pub fn message_to_json(message: &Message) -> Value {
    match message {
        Message::User(user) => Value::Map(vec![
            kv("role", str("user")),
            kv(
                "content",
                match &user.content {
                    pi_ai::types::UserMessageContent::Text(text) => str(text),
                    pi_ai::types::UserMessageContent::Blocks(blocks) => {
                        Value::Array(blocks.iter().map(content_to_json).collect())
                    }
                },
            ),
            kv("timestamp", num(user.timestamp)),
        ]),
        Message::Assistant(assistant) => {
            let mut entries = vec![
                kv("role", str("assistant")),
                kv(
                    "content",
                    Value::Array(
                        assistant
                            .content
                            .iter()
                            .map(|block| match block {
                                pi_ai::types::Content::Text(text) => Value::Map(vec![
                                    kv("type", str("text")),
                                    kv("text", str(&text.text)),
                                ]),
                                pi_ai::types::Content::Thinking(thinking) => Value::Map(vec![
                                    kv("type", str("thinking")),
                                    kv("thinking", str(&thinking.thinking)),
                                ]),
                                pi_ai::types::Content::Image(image) => Value::Map(vec![
                                    kv("type", str("image")),
                                    kv("data", str(&image.data)),
                                    kv("mimeType", str(&image.mime_type)),
                                ]),
                                pi_ai::types::Content::ToolCall(tool_call) => Value::Map(vec![
                                    kv("type", str("toolCall")),
                                    kv("id", str(&tool_call.id)),
                                    kv("name", str(&tool_call.name)),
                                    kv("arguments", tool_call.arguments.clone()),
                                ]),
                            })
                            .collect(),
                    ),
                ),
                kv("api", str(&assistant.api)),
                kv("provider", str(&assistant.provider)),
                kv("model", str(&assistant.model)),
                kv("usage", usage_to_json(&assistant.usage)),
                kv("stopReason", str(assistant.stop_reason.as_str())),
                kv("timestamp", num(assistant.timestamp)),
            ];
            if let Some(response_model) = &assistant.response_model {
                entries.push(kv("responseModel", str(response_model)));
            }
            if let Some(error_message) = &assistant.error_message {
                entries.push(kv("errorMessage", str(error_message)));
            }
            Value::Map(entries)
        }
        Message::ToolResult(tool_result) => Value::Map(vec![
            kv("role", str("toolResult")),
            kv("toolCallId", str(&tool_result.tool_call_id)),
            kv("toolName", str(&tool_result.tool_name)),
            kv(
                "content",
                Value::Array(tool_result.content.iter().map(content_to_json).collect()),
            ),
            kv("isError", Value::Bool(tool_result.is_error)),
            kv("timestamp", num(tool_result.timestamp)),
        ]),
    }
}

/// Shared with pi-sqlite's usage encoding (identical field order).
pub fn usage_to_json(usage: &Usage) -> Value {
    Value::Map(vec![
        kv("input", num(usage.input)),
        kv("output", num(usage.output)),
        kv("cacheRead", num(usage.cache_read)),
        kv("cacheWrite", num(usage.cache_write)),
        kv("cacheWrite1h", usage.cache_write_1h.map(num).unwrap_or(Value::Null)),
        kv("reasoning", usage.reasoning.map(num).unwrap_or(Value::Null)),
        kv("totalTokens", num(usage.total_tokens)),
        kv(
            "cost",
            Value::Map(vec![
                kv("input", num(usage.cost.input)),
                kv("output", num(usage.cost.output)),
                kv("cacheRead", num(usage.cost.cache_read)),
                kv("cacheWrite", num(usage.cost.cache_write)),
                kv("total", num(usage.cost.total)),
            ]),
        ),
    ])
}

fn usage_from_entries(entries: &[(String, Value)]) -> Option<Usage> {
    let usage_entries = entries.iter().find(|(k, _)| k == "usage")?.as_map()?;
    fn number(entries: &[(String, Value)], key: &str) -> f64 {
        entries
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.as_number())
            .unwrap_or(0.0)
    }
    let cost: Vec<(String, Value)> = usage_entries
        .iter()
        .find(|(k, _)| k == "cost")
        .and_then(|(_, v)| v.as_map())
        .map(|map| map.to_vec())
        .unwrap_or_default();
    Some(Usage {
        input: number(usage_entries, "input"),
        output: number(usage_entries, "output"),
        cache_read: number(usage_entries, "cacheRead"),
        cache_write: number(usage_entries, "cacheWrite"),
        cache_write_1h: usage_entries
            .iter()
            .find(|(k, _)| k == "cacheWrite1h")
            .and_then(|(_, v)| v.as_number()),
        reasoning: usage_entries
            .iter()
            .find(|(k, _)| k == "reasoning")
            .and_then(|(_, v)| v.as_number()),
        total_tokens: number(usage_entries, "totalTokens"),
        cost: pi_ai::types::UsageCost {
            input: number(&cost, "input"),
            output: number(&cost, "output"),
            cache_read: number(&cost, "cacheRead"),
            cache_write: number(&cost, "cacheWrite"),
            total: number(&cost, "total"),
        },
    })
}

pub fn entry_to_json(entry: &SessionEntry) -> Value {
    match entry {
        SessionEntry::Message { base, message } => {
            let mut entries = base_entries(base, "message");
            entries.push(kv("message", session_message_to_json(message)));
            Value::Map(entries)
        }
        SessionEntry::ThinkingLevelChange { base, thinking_level } => {
            let mut entries = base_entries(base, "thinking_level_change");
            entries.push(kv("thinkingLevel", str(thinking_level)));
            Value::Map(entries)
        }
        SessionEntry::ModelChange { base, provider, model_id } => {
            let mut entries = base_entries(base, "model_change");
            entries.push(kv("provider", str(provider)));
            entries.push(kv("modelId", str(model_id)));
            Value::Map(entries)
        }
        SessionEntry::Compaction {
            base,
            summary,
            first_kept_entry_id,
            tokens_before,
            details,
            usage,
            from_hook,
            first_kept_entry_index,
        } => {
            let mut entries = base_entries(base, "compaction");
            entries.push(kv("summary", str(summary)));
            entries.push(kv("firstKeptEntryId", str(first_kept_entry_id)));
            entries.push(kv("tokensBefore", num(*tokens_before)));
            if let Some(index) = first_kept_entry_index {
                entries.push(kv("firstKeptEntryIndex", Value::Number(*index as f64)));
            }
            if let Some(details) = details {
                entries.push(kv("details", details.clone()));
            }
            if let Some(usage) = usage {
                entries.push(kv("usage", usage_to_json(usage)));
            }
            if let Some(from_hook) = from_hook {
                entries.push(kv("fromHook", Value::Bool(*from_hook)));
            }
            Value::Map(entries)
        }
        SessionEntry::BranchSummary {
            base,
            from_id,
            summary,
            details,
            usage,
            from_hook,
        } => {
            let mut entries = base_entries(base, "branch_summary");
            entries.push(kv("fromId", str(from_id)));
            entries.push(kv("summary", str(summary)));
            if let Some(details) = details {
                entries.push(kv("details", details.clone()));
            }
            if let Some(usage) = usage {
                entries.push(kv("usage", usage_to_json(usage)));
            }
            if let Some(from_hook) = from_hook {
                entries.push(kv("fromHook", Value::Bool(*from_hook)));
            }
            Value::Map(entries)
        }
        SessionEntry::Custom {
            base,
            custom_type,
            data,
        } => {
            let mut entries = base_entries(base, "custom");
            entries.push(kv("customType", str(custom_type)));
            if let Some(data) = data {
                entries.push(kv("data", data.clone()));
            }
            Value::Map(entries)
        }
        SessionEntry::CustomMessage {
            base,
            custom_type,
            content,
            details,
            display,
        } => {
            let mut entries = base_entries(base, "custom_message");
            entries.push(kv("customType", str(custom_type)));
            entries.push(kv("content", content_or_text_to_json(content)));
            if let Some(details) = details {
                entries.push(kv("details", details.clone()));
            }
            entries.push(kv("display", Value::Bool(*display)));
            Value::Map(entries)
        }
        SessionEntry::Label {
            base,
            target_id,
            label,
        } => {
            let mut entries = base_entries(base, "label");
            entries.push(kv("targetId", str(target_id)));
            entries.push(kv("label", label.as_deref().map(str).unwrap_or(Value::Null)));
            Value::Map(entries)
        }
        SessionEntry::SessionInfo { base, name } => {
            let mut entries = base_entries(base, "session_info");
            if let Some(name) = name {
                entries.push(kv("name", str(name)));
            }
            Value::Map(entries)
        }
    }
}

pub fn file_entry_to_json(file_entry: &FileEntry) -> Value {
    match file_entry {
        FileEntry::Header(header) => {
            let mut entries = vec![
                kv("type", str("session")),
                kv("id", str(&header.id)),
                kv("timestamp", str(&header.timestamp)),
                kv("cwd", str(&header.cwd)),
            ];
            if let Some(version) = header.version {
                entries.push(kv("version", Value::Number(version as f64)));
            }
            if let Some(parent_session) = &header.parent_session {
                entries.push(kv("parentSession", str(parent_session)));
            }
            Value::Map(entries)
        }
        FileEntry::Entry(entry) => entry_to_json(entry),
    }
}

fn string_of(entries: &[(String, Value)], key: &str) -> String {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn number_of(entries: &[(String, Value)], key: &str) -> f64 {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_number())
        .unwrap_or(0.0)
}

fn opt_string_of(entries: &[(String, Value)], key: &str) -> Option<String> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_str())
        .map(|value| value.to_string())
}

fn json_content_to_ai(value: &Value) -> pi_ai::types::Content {
    let entries: Vec<(String, Value)> = value.as_map().map(|map| map.to_vec()).unwrap_or_default();
    let type_ = string_of(&entries, "type");
    match type_.as_str() {
        "text" => pi_ai::types::Content::Text(pi_ai::types::TextContent {
            text: string_of(&entries, "text"),
            text_signature: None,
        }),
        "thinking" => pi_ai::types::Content::Thinking(pi_ai::types::ThinkingContent {
            thinking: string_of(&entries, "thinking"),
            thinking_signature: None,
            redacted: entries
                .iter()
                .find(|(k, _)| k == "redacted")
                .and_then(|(_, v)| v.as_bool()),
        }),
        "image" => pi_ai::types::Content::Image(pi_ai::types::ImageContent {
            data: string_of(&entries, "data"),
            mime_type: string_of(&entries, "mimeType"),
        }),
        "toolCall" => pi_ai::types::Content::ToolCall(pi_ai::types::ToolCall {
            id: string_of(&entries, "id"),
            name: string_of(&entries, "name"),
            arguments: entries
                .iter()
                .find(|(k, _)| k == "arguments")
                .map(|(_, v)| v.clone())
                .unwrap_or(Value::Map(Vec::new())),
            thought_signature: None,
            namespace: None,
        }),
        _ => pi_ai::types::Content::Text(pi_ai::types::TextContent {
            text: String::new(),
            text_signature: None,
        }),
    }
}

fn message_from_entries(entries: &[(String, Value)]) -> SessionMessage {
    let role = string_of(entries, "role");
    match role.as_str() {
        "user" => {
            let content = entries
                .iter()
                .find(|(k, _)| k == "content")
                .map(|(_, v)| v.clone())
                .unwrap_or(Value::Null);
            let content = match content {
                Value::String(text) => pi_ai::types::UserMessageContent::Text(text),
                Value::Array(blocks) => {
                    pi_ai::types::UserMessageContent::Blocks(blocks.iter().map(json_content_to_ai).collect())
                }
                _ => pi_ai::types::UserMessageContent::Text(String::new()),
            };
            SessionMessage::Llm(Message::User(pi_ai::types::UserMessage {
                content,
                timestamp: number_of(entries, "timestamp"),
            }))
        }
        "assistant" => {
            let content = entries
                .iter()
                .find(|(k, _)| k == "content")
                .and_then(|(_, v)| v.as_array())
                .unwrap_or_default()
                .iter()
                .map(json_content_to_ai)
                .collect();
            let stop_reason = string_of(entries, "stopReason");
            let usage = usage_from_entries(entries).unwrap_or_else(empty_usage);
            SessionMessage::Llm(Message::Assistant(pi_ai::types::AssistantMessage {
                content,
                api: string_of(entries, "api"),
                provider: string_of(entries, "provider"),
                model: string_of(entries, "model"),
                response_model: opt_string_of(entries, "responseModel"),
                response_id: None,
                usage,
                stop_reason: pi_ai::types::StopReason::parse(&stop_reason).unwrap_or(pi_ai::types::StopReason::Stop),
                deferred: None,
                error_message: opt_string_of(entries, "errorMessage"),
                raw_stop_reason: None,
                end_turn: None,
                timestamp: number_of(entries, "timestamp"),
            }))
        }
        "toolResult" => SessionMessage::Llm(Message::ToolResult(pi_ai::types::ToolResultMessage {
            tool_call_id: string_of(entries, "toolCallId"),
            tool_name: string_of(entries, "toolName"),
            content: entries
                .iter()
                .find(|(k, _)| k == "content")
                .and_then(|(_, v)| v.as_array())
                .unwrap_or_default()
                .iter()
                .map(json_content_to_ai)
                .collect(),
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: entries
                .iter()
                .find(|(k, _)| k == "isError")
                .and_then(|(_, v)| v.as_bool())
                .unwrap_or(false),
            timestamp: number_of(entries, "timestamp"),
        })),
        "bashExecution" => SessionMessage::Bash(BashExecutionMessage {
            command: string_of(entries, "command"),
            output: string_of(entries, "output"),
            exit_code: entries
                .iter()
                .find(|(k, _)| k == "exitCode")
                .and_then(|(_, v)| v.as_number())
                .map(|value| value as i64),
            cancelled: entries
                .iter()
                .find(|(k, _)| k == "cancelled")
                .and_then(|(_, v)| v.as_bool())
                .unwrap_or(false),
            truncated: entries
                .iter()
                .find(|(k, _)| k == "truncated")
                .and_then(|(_, v)| v.as_bool())
                .unwrap_or(false),
            full_output_path: opt_string_of(entries, "fullOutputPath"),
            timestamp: number_of(entries, "timestamp"),
            exclude_from_context: entries
                .iter()
                .find(|(k, _)| k == "excludeFromContext")
                .and_then(|(_, v)| v.as_bool()),
        }),
        "custom" => SessionMessage::Custom(CustomMessage {
            custom_type: string_of(entries, "customType"),
            content: json_to_content_or_text(entries),
            display: entries
                .iter()
                .find(|(k, _)| k == "display")
                .and_then(|(_, v)| v.as_bool())
                .unwrap_or(false),
            details: entries
                .iter()
                .find(|(k, _)| k == "details")
                .map(|(_, v)| v.clone()),
            timestamp: number_of(entries, "timestamp"),
        }),
        "branchSummary" => SessionMessage::BranchSummary(BranchSummaryMessage {
            summary: string_of(entries, "summary"),
            from_id: string_of(entries, "fromId"),
            timestamp: number_of(entries, "timestamp"),
        }),
        "compactionSummary" => SessionMessage::CompactionSummary(CompactionSummaryMessage {
            summary: string_of(entries, "summary"),
            tokens_before: number_of(entries, "tokensBefore"),
            timestamp: number_of(entries, "timestamp"),
        }),
        _ => SessionMessage::Unknown(Value::Map(entries.to_vec())),
    }
}

fn json_to_content_or_text(entries: &[(String, Value)]) -> ContentOrText {
    match entries.iter().find(|(k, _)| k == "content") {
        Some((_, Value::String(text))) => ContentOrText::Text(text.clone()),
        Some((_, Value::Array(blocks))) => {
            ContentOrText::Blocks(blocks.iter().map(json_content_to_ai).collect())
        }
        _ => ContentOrText::Text(String::new()),
    }
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
        cost: pi_ai::types::UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
    }
}

/// Parse one entry line; null for blank or malformed lines (skipped).
pub fn parse_session_entry_line(line: &str) -> Option<FileEntry> {
    if line.trim().is_empty() {
        return None;
    }
    parse_entry_json(line).or(None)
}

/// JSON.parse(line) with the JS shape; malformed lines return None.
pub fn parse_entry_json(line: &str) -> Option<FileEntry> {
    let value: Value = pi_ai::utils::json::parse_json_with_repair(line).ok()?;
    let entries: Vec<(String, Value)> = value.as_map().map(|map| map.to_vec()).unwrap_or_default();
    let type_ = string_of(&entries, "type");
    if type_ == "session" {
        return Some(FileEntry::Header(SessionHeader {
            version: entries
                .iter()
                .find(|(k, _)| k == "version")
                .and_then(|(_, v)| v.as_number())
                .map(|value| value as i64),
            id: string_of(&entries, "id"),
            timestamp: string_of(&entries, "timestamp"),
            cwd: string_of(&entries, "cwd"),
            parent_session: opt_string_of(&entries, "parentSession"),
        }));
    }
    Some(FileEntry::Entry(entry_from_entries(entries, &type_)))
}

fn entry_from_entries(entries: Vec<(String, Value)>, type_: &str) -> SessionEntry {
    let base = SessionEntryBase {
        id: string_of(&entries, "id"),
        parent_id: entries
            .iter()
            .find(|(k, _)| k == "parentId")
            .and_then(|(_, v)| v.as_str())
            .map(|value| value.to_string()),
        timestamp: string_of(&entries, "timestamp"),
    };
    match type_ {
        "message" => SessionEntry::Message {
            base,
            message: message_from_entries(
                entries
                    .iter()
                    .find(|(k, _)| k == "message")
                    .and_then(|(_, v)| v.as_map())
                    .map(|map| map.to_vec())
                    .unwrap_or_default()
                    .as_slice(),
            ),
        },
        "thinking_level_change" => SessionEntry::ThinkingLevelChange {
            base,
            thinking_level: string_of(&entries, "thinkingLevel"),
        },
        "model_change" => SessionEntry::ModelChange {
            base,
            provider: string_of(&entries, "provider"),
            model_id: string_of(&entries, "modelId"),
        },
        "compaction" => SessionEntry::Compaction {
            base,
            summary: string_of(&entries, "summary"),
            first_kept_entry_id: string_of(&entries, "firstKeptEntryId"),
            tokens_before: number_of(&entries, "tokensBefore"),
            details: entries.iter().find(|(k, _)| k == "details").map(|(_, v)| v.clone()),
            usage: usage_from_entries(&entries),
            from_hook: entries
                .iter()
                .find(|(k, _)| k == "fromHook")
                .and_then(|(_, v)| v.as_bool()),
            first_kept_entry_index: entries
                .iter()
                .find(|(k, _)| k == "firstKeptEntryIndex")
                .and_then(|(_, v)| v.as_number())
                .map(|value| value as i64),
        },
        "branch_summary" => SessionEntry::BranchSummary {
            base,
            from_id: string_of(&entries, "fromId"),
            summary: string_of(&entries, "summary"),
            details: entries.iter().find(|(k, _)| k == "details").map(|(_, v)| v.clone()),
            usage: usage_from_entries(&entries),
            from_hook: entries
                .iter()
                .find(|(k, _)| k == "fromHook")
                .and_then(|(_, v)| v.as_bool()),
        },
        "custom" => SessionEntry::Custom {
            base,
            custom_type: string_of(&entries, "customType"),
            data: entries.iter().find(|(k, _)| k == "data").map(|(_, v)| v.clone()),
        },
        "custom_message" => SessionEntry::CustomMessage {
            base,
            custom_type: string_of(&entries, "customType"),
            content: json_to_content_or_text(&entries),
            details: entries.iter().find(|(k, _)| k == "details").map(|(_, v)| v.clone()),
            display: entries
                .iter()
                .find(|(k, _)| k == "display")
                .and_then(|(_, v)| v.as_bool())
                .unwrap_or(false),
        },
        "label" => SessionEntry::Label {
            base,
            target_id: string_of(&entries, "targetId"),
            label: entries
                .iter()
                .find(|(k, _)| k == "label")
                .and_then(|(_, v)| v.as_str())
                .map(|value| value.to_string()),
        },
        _ => SessionEntry::SessionInfo {
            base,
            name: opt_string_of(&entries, "name"),
        },
    }
}

// ---------------------------------------------------------------------------
// Parsing and migration
// ---------------------------------------------------------------------------

/// Parse a session file's content into file entries (malformed lines skipped).
pub fn parse_session_entries(content: &str) -> Vec<FileEntry> {
    let mut entries = Vec::new();
    for line in content.trim().split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(entry) = parse_entry_json(line) {
            entries.push(entry);
        }
    }
    entries
}

/// Migrate v1 → v2: add id/parentId tree structure. Mutates in place.
fn migrate_v1_to_v2(entries: &mut [FileEntry]) {
    let mut ids = std::collections::HashSet::new();
    let mut prev_id: Option<String> = None;

    for i in 0..entries.len() {
        match &mut entries[i] {
            FileEntry::Header(header) => {
                header.version = Some(2);
            }
            FileEntry::Entry(entry) => {
                let id = generate_id(&|candidate| ids.contains(candidate));
                ids.insert(id.clone());
                let parent_id = prev_id.clone();
                prev_id = Some(id.clone());
                let mut entry = entry.clone();
                entry = entry.with_parent(parent_id);
                entry = set_entry_id(entry, id.clone());

                // Convert firstKeptEntryIndex to firstKeptEntryId for compaction.
                // The index is into the full entries array, header included.
                if let SessionEntry::Compaction {
                    first_kept_entry_id,
                    first_kept_entry_index,
                    ..
                } = &mut entry
                {
                    if let Some(index) = *first_kept_entry_index {
                        let index = index as usize;
                        if let Some(FileEntry::Entry(target)) = entries.get(index) {
                            *first_kept_entry_id = target.id().to_string();
                        }
                        *first_kept_entry_index = None;
                    }
                }
                *entry = entry;
            }
        }
    }
}

fn set_entry_id(entry: SessionEntry, id: String) -> SessionEntry {
    match entry {
        SessionEntry::Message { mut base, message } => {
            base.id = id;
            SessionEntry::Message { base, message }
        }
        SessionEntry::ThinkingLevelChange { mut base, thinking_level } => {
            base.id = id;
            SessionEntry::ThinkingLevelChange { base, thinking_level }
        }
        SessionEntry::ModelChange { mut base, provider, model_id } => {
            base.id = id;
            SessionEntry::ModelChange { base, provider, model_id }
        }
        SessionEntry::Compaction {
            mut base,
            summary,
            first_kept_entry_id,
            tokens_before,
            details,
            usage,
            from_hook,
            first_kept_entry_index,
        } => {
            base.id = id;
            SessionEntry::Compaction {
                base,
                summary,
                first_kept_entry_id,
                tokens_before,
                details,
                usage,
                from_hook,
                first_kept_entry_index,
            }
        }
        SessionEntry::BranchSummary { mut base, from_id, summary, details, usage, from_hook } => {
            base.id = id;
            SessionEntry::BranchSummary { base, from_id, summary, details, usage, from_hook }
        }
        SessionEntry::Custom { mut base, custom_type, data } => {
            base.id = id;
            SessionEntry::Custom { base, custom_type, data }
        }
        SessionEntry::CustomMessage { mut base, custom_type, content, details, display } => {
            base.id = id;
            SessionEntry::CustomMessage { base, custom_type, content, details, display }
        }
        SessionEntry::Label { mut base, target_id, label } => {
            base.id = id;
            SessionEntry::Label { base, target_id, label }
        }
        SessionEntry::SessionInfo { mut base, name } => {
            base.id = id;
            SessionEntry::SessionInfo { base, name }
        }
    }
}

/// Migrate v2 → v3: rename the hookMessage role to custom. Mutates in place.
fn migrate_v2_to_v3(entries: &mut [FileEntry]) {
    for entry in entries.iter_mut() {
        match entry {
            FileEntry::Header(header) => {
                header.version = Some(3);
            }
            FileEntry::Entry(SessionEntry::Message {
                message: SessionMessage::Unknown(value),
                ..
            }) => {
                // The decoder keeps unknown roles verbatim; rewrite the role
                // in place, mirroring the JS object mutation.
                if let Value::Map(fields) = value {
                    for (key, field) in fields.iter_mut() {
                        if key == "role" {
                            if let Value::String(role) = field {
                                if role == "hookMessage" {
                                    *role = "custom".to_string();
                                }
                            }
                        }
                    }
                }
            }
            FileEntry::Entry(_) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Pure session logic
// ---------------------------------------------------------------------------
// Pure session logic
// ---------------------------------------------------------------------------

/// Migrate entries to the current version; true if any migration applied.
/// The JS version mutates JSON in place; here Unknown messages are
/// re-encoded after migration (see `migrate_v2_to_v3` details).
pub fn migrate_session_entries(entries: &mut [FileEntry]) -> bool {
    let header_version = entries
        .iter()
        .find_map(|entry| match entry {
            FileEntry::Header(header) => header.version,
            _ => None,
        })
        .unwrap_or(1);
    if header_version >= CURRENT_SESSION_VERSION {
        return false;
    }
    if header_version < 2 {
        migrate_v1_to_v2(entries);
    }
    if header_version < 3 {
        migrate_v2_to_v3(entries);
    }
    true
}

pub fn get_latest_compaction_entry(entries: &[SessionEntry]) -> Option<&SessionEntry> {
    entries.iter().rev().find(|entry| matches!(entry, SessionEntry::Compaction { .. }))
}

/// Build the entry index.
pub fn build_entry_index(entries: &[SessionEntry]) -> std::collections::HashMap<String, &SessionEntry> {
    entries.iter().map(|entry| (entry.id().to_string(), entry)).collect()
}

/// Build the active path from the leaf to the root (root first).
pub fn build_session_path<'a>(
    entries: &'a [SessionEntry],
    leaf_id: Option<&str>,
    by_id: &std::collections::HashMap<String, &'a SessionEntry>,
) -> Vec<&'a SessionEntry> {
    let mut leaf: Option<&SessionEntry> = None;
    if let Some(leaf_id) = leaf_id {
        leaf = by_id.get(leaf_id).copied();
    }
    leaf = leaf.or_else(|| entries.last().copied());
    let Some(mut current) = leaf else {
        return Vec::new();
    };

    let mut path: Vec<&SessionEntry> = Vec::new();
    while let Some(entry) = by_id.get(current.id()) {
        path.push(*entry);
        current = match entry.parent_id() {
            Some(parent_id) => match by_id.get(parent_id) {
                Some(parent) => parent,
                None => break,
            },
            None => break,
        };
    }
    path.reverse();
    path
}

fn get_session_context_settings(path: &[&SessionEntry]) -> (String, Option<(String, String)>) {
    let mut thinking_level = "off".to_string();
    let mut model: Option<(String, String)> = None;

    for entry in path {
        match entry {
            SessionEntry::ThinkingLevelChange { thinking_level: level, .. } => thinking_level = level.clone(),
            SessionEntry::ModelChange { provider, model_id, .. } => {
                model = Some((provider.clone(), model_id.clone()))
            }
            SessionEntry::Message { message: SessionMessage::Llm(Message::Assistant(assistant)), .. } => {
                model = Some((assistant.provider.clone(), assistant.model.clone()));
            }
            _ => {}
        }
    }
    (thinking_level, model)
}

/// Project one selected session entry into LLM/runtime messages. Plain custom
/// entries are display/state entries and do not participate in context.
pub fn session_entry_to_context_messages(
    entry: &SessionEntry,
) -> Vec<pi_agent_core::types::AgentMessage> {
    use crate::session_messages::UnknownMessage;
    use pi_agent_core::types::AgentMessage;
    match entry {
        SessionEntry::Message { message, .. } => match message {
            SessionMessage::Llm(message) => {
                // Session files are parsed without validation; old versions,
                // forks, or hand-edited files can contain messages with
                // null/missing content. The decoder maps null content to an
                // empty Text (pi-sqlite parity); JS would emit content: [].
                // ponytail: emitted as-is; null-content messages only arise
                // from hand-edited files.
                vec![AgentMessage::Llm(message.clone())]
            }
            SessionMessage::Bash(bash) => vec![AgentMessage::Custom(std::sync::Arc::new(bash.clone()))],
            SessionMessage::Custom(custom) => vec![AgentMessage::Custom(std::sync::Arc::new(custom.clone()))],
            SessionMessage::BranchSummary(branch) => {
                vec![AgentMessage::Custom(std::sync::Arc::new(branch.clone()))]
            }
            SessionMessage::CompactionSummary(compaction) => {
                vec![AgentMessage::Custom(std::sync::Arc::new(compaction.clone()))]
            }
            SessionMessage::Unknown(value) => vec![AgentMessage::Custom(std::sync::Arc::new(UnknownMessage {
                role: String::new(),
                value: value.clone(),
            }))],
        },
        SessionEntry::CustomMessage {
            custom_type,
            content,
            details,
            display,
            base,
            ..
        } => vec![AgentMessage::Custom(std::sync::Arc::new(CustomMessage {
            custom_type: custom_type.clone(),
            content: content.clone(),
            display: *display,
            details: details.clone(),
            timestamp: parse_timestamp_ms(&base.timestamp),
        }))],
        SessionEntry::BranchSummary { summary, from_id, base, .. } if !summary.is_empty() => {
            vec![AgentMessage::Custom(std::sync::Arc::new(BranchSummaryMessage {
                summary: summary.clone(),
                from_id: from_id.clone(),
                timestamp: parse_timestamp_ms(&base.timestamp),
            }))]
        }
        SessionEntry::Compaction { summary, tokens_before, base, .. } => {
            vec![AgentMessage::Custom(std::sync::Arc::new(CompactionSummaryMessage {
                summary: summary.clone(),
                tokens_before: *tokens_before,
                timestamp: parse_timestamp_ms(&base.timestamp),
            }))]
        }
        _ => Vec::new(),
    }
}

/// Build the active, compaction-aware session entry list.
pub fn build_context_entries<'a>(
    entries: &'a [SessionEntry],
    leaf_id: Option<&str>,
    by_id: &std::collections::HashMap<String, &'a SessionEntry>,
) -> Vec<&'a SessionEntry> {
    let path = build_session_path(entries, leaf_id, by_id);
    let compaction = path.iter().find(|entry| matches!(entry, SessionEntry::Compaction { .. })).copied();

    let Some(compaction) = compaction else {
        return path;
    };
    let compaction_idx = path
        .iter()
        .position(|entry| entry.id() == compaction.id())
        .unwrap_or(usize::MAX);
    if compaction_idx == usize::MAX {
        return path;
    }

    let first_kept_entry_id = match compaction {
        SessionEntry::Compaction { first_kept_entry_id, .. } => first_kept_entry_id.as_str(),
        _ => "",
    };

    let mut context_entries: Vec<&'a SessionEntry> = vec![compaction];
    let mut found_first_kept = false;
    for entry in &path[..compaction_idx] {
        if entry.id() == first_kept_entry_id {
            found_first_kept = true;
        }
        if found_first_kept {
            context_entries.push(*entry);
        }
    }
    context_entries.extend_from_slice(&path[compaction_idx + 1..]);
    context_entries
}

/// Build the session context (messages + settings) from entries.
pub fn build_session_context<'a>(
    entries: &'a [SessionEntry],
    leaf_id: Option<&str>,
    by_id: &std::collections::HashMap<String, &'a SessionEntry>,
) -> SessionContext {
    let path = build_session_path(entries, leaf_id, by_id);
    let (thinking_level, model) = get_session_context_settings(&path);
    let messages = build_context_entries(entries, leaf_id, by_id)
        .into_iter()
        .flat_map(session_entry_to_context_messages)
        .collect();
    SessionContext {
        messages,
        thinking_level,
        model,
    }
}

/// Defensive copy of the session file as a JSON string (used by tests).
pub fn file_entries_to_jsonl(entries: &[FileEntry]) -> String {
    entries
        .iter()
        .map(|entry| json_stringify(&file_entry_to_json(entry)))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_line(type_: &str) -> String {
        match type_ {
            "message" => r#"{"type":"message","id":"a1","parentId":null,"timestamp":"2024-01-01T00:00:00.000Z","message":{"role":"user","content":"hi","timestamp":1}}"#.to_string(),
            "compaction" => r#"{"type":"compaction","id":"c1","parentId":"a1","timestamp":"2024-01-01T00:00:00.000Z","summary":"s","firstKeptEntryId":"a2","tokensBefore":100}"#.to_string(),
            _ => format!(r#"{{"type":"{type_}","id":"x1","parentId":null,"timestamp":"2024-01-01T00:00:00.000Z"}}"#),
        }
    }

    fn session_line() -> String {
        r#"{"type":"session","version":3,"id":"s1","timestamp":"2024-01-01T00:00:00.000Z","cwd":"/tmp"}"#.to_string()
    }

    #[test]
    fn parse_session_entries_skips_malformed() {
        let content = format!("{}\n{}\nnot-json\n{}\n", session_line(), entry_line("message"), entry_line("model_change"));
        let entries = parse_session_entries(&content);
        assert_eq!(entries.len(), 3);
        assert!(matches!(entries[0], FileEntry::Header(_)));
        assert!(matches!(entries[1], FileEntry::Entry(SessionEntry::Message { .. })));
    }

    #[test]
    fn parse_round_trips_message_entry() {
        let line = entry_line("message");
        let entry = parse_entry_json(&line).unwrap();
        let json = file_entry_to_json(&entry);
        assert_eq!(json_stringify(&json), line);
    }

    #[test]
    fn parse_round_trips_compaction_entry() {
        let line = entry_line("compaction");
        let entry = parse_entry_json(&line).unwrap();
        match &entry {
            FileEntry::Entry(SessionEntry::Compaction { summary, tokens_before, first_kept_entry_id, .. }) => {
                assert_eq!(summary, "s");
                assert_eq!(*tokens_before, 100.0);
                assert_eq!(first_kept_entry_id, "a2");
            }
            _ => panic!("expected compaction"),
        }
        let json = file_entry_to_json(&entry);
        assert_eq!(json_stringify(&json), line);
    }

    #[test]
    fn header_round_trip() {
        let entry = parse_entry_json(&session_line()).unwrap();
        let json = file_entry_to_json(&entry);
        assert_eq!(json_stringify(&json), session_line());
    }

    #[test]
    fn assert_valid_session_id_rules() {
        assert!(assert_valid_session_id("abc123").is_ok());
        assert!(assert_valid_session_id("a-b_c.1").is_ok());
        assert!(assert_valid_session_id("").is_err());
        assert!(assert_valid_session_id("-abc").is_err());
        assert!(assert_valid_session_id("abc-").is_err());
        assert!(assert_valid_session_id("a b").is_err());
        assert!(assert_valid_session_id("a/b").is_err());
    }

    #[test]
    fn generate_id_is_8_hex_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            let id = generate_id(&|candidate| seen.contains(candidate));
            assert_eq!(id.len(), 8);
            assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
            seen.insert(id);
        }
    }

    #[test]
    fn build_session_context_follows_leaf_path() {
        let content = format!(
            "{}\n{}\n{}\n{}\n{}\n",
            session_line(),
            entry_line("message"),
            entry_line("thinking_level_change"),
            entry_line("message"),
            entry_line("label")
        );
        let entries = parse_session_entries(&content);
        let session_entries: Vec<SessionEntry> = entries
            .into_iter()
            .filter_map(|entry| match entry {
                FileEntry::Entry(entry) => Some(entry),
                _ => None,
            })
            .collect();
        let by_id = build_entry_index(&session_entries);
        let context = build_session_context(&session_entries, None, &by_id);
        // Default thinking level off; only message entries produce messages.
        assert_eq!(context.thinking_level, "off");
        assert_eq!(context.messages.len(), 2);
    }

    #[test]
    fn compaction_trims_older_entries() {
        let mut entries = parse_session_entries(&format!(
            "{}\n{}\n{}\n{}\n",
            session_line(),
            entry_line("message"),
            entry_line("compaction"),
            entry_line("message"),
        ))
        .into_iter()
        .filter_map(|entry| match entry {
            FileEntry::Entry(entry) => Some(entry),
            _ => None,
        })
        .collect::<Vec<_>>();
        // firstKeptEntryId references "a2" which doesn't exist in this small
        // fixture; the compaction entry itself plus everything after remains.
        let by_id = build_entry_index(&entries);
        let context = build_context_entries(&entries, None, &by_id);
        assert_eq!(context.len(), 2);
        assert!(matches!(context[0], SessionEntry::Compaction { .. }));
        assert!(matches!(context[1], SessionEntry::Message { .. }));

        // A compaction with a valid firstKeptEntryId keeps entries from there.
        let mut entries2 = parse_session_entries(&format!(
            "{}\n{}\n{}\n{}\n",
            session_line(),
            entry_line("message"),
            entry_line("compaction"),
            entry_line("message"),
        ))
        .into_iter()
        .filter_map(|entry| match entry {
            FileEntry::Entry(entry) => Some(entry),
            _ => None,
        })
        .collect::<Vec<_>>();
        if let SessionEntry::Compaction { first_kept_entry_id, .. } = &mut entries2[1] {
            *first_kept_entry_id = "a1".to_string();
        }
        let by_id = build_entry_index(&entries2);
        let context = build_context_entries(&entries2, None, &by_id);
        assert_eq!(context.len(), 3);
        let _ = &mut entries;
    }

    #[test]
    fn session_message_custom_roles_round_trip() {
        let line = r#"{"type":"message","id":"b1","parentId":null,"timestamp":"2024-01-01T00:00:00.000Z","message":{"role":"bashExecution","command":"ls","output":"x","exitCode":0,"cancelled":false,"truncated":false,"timestamp":1}}"#;
        let entry = parse_entry_json(line).unwrap();
        match &entry {
            FileEntry::Entry(SessionEntry::Message { message, .. }) => match message {
                SessionMessage::Bash(bash) => {
                    assert_eq!(bash.command, "ls");
                    assert_eq!(bash.exit_code, Some(0));
                }
                _ => panic!("expected bash message"),
            },
            _ => panic!("expected message entry"),
        }
        // Round trip preserves the raw shape.
        assert_eq!(json_stringify(&file_entry_to_json(&entry)), line);
    }
}
