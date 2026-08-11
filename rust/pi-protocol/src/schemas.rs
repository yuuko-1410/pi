//! Protocol message models and validation.
//!
//! Rust port of `packages/protocol/src/schemas.ts`. Every type mirrors the
//! TypeBox schema: strict objects (no unknown keys), discriminated unions,
//! optional fields, and the same value constraints. Field order in `to_value`
//! follows the schema declaration order so encoded bytes match the JS
//! implementation.
//!
//! Parse errors are opaque (`()`): the public API reports the same uniform
//! message as JS (`Invalid client protocol message` / `Invalid server
//! protocol message`), because TypeBox error details are not part of the
//! protocol contract.

use crate::cbor::Value;

pub const PROTOCOL_VERSION: f64 = 1.0;

pub type ThinkingLevel = String;
pub type SessionPhase = String;

#[derive(Debug, Clone, PartialEq)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Text,
    Image,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelMetadata {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub api: String,
    pub reasoning: bool,
    pub input: Vec<InputKind>,
    pub context_window: f64,
    pub max_tokens: f64,
    pub cost: ModelCost,
    pub supported_thinking_levels: Vec<String>,
    pub authenticated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Text { text: String },
    Thinking { thinking: String, redacted: Option<bool> },
    Image { data: String, mime_type: String },
    ToolCall { tool_call_id: String, tool_name: String, input: Value },
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Usage {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub reasoning: Option<f64>,
    pub total_tokens: f64,
    pub cost: UsageCost,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserItem {
    pub id: String,
    pub content: Vec<Content>,
    pub timestamp: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AssistantStatus {
    Streaming,
    Complete { stop_reason: String },
    Error { error_message: Option<String> },
    Aborted { error_message: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssistantItem {
    pub id: String,
    pub content: Vec<Content>,
    pub model: ModelRef,
    pub response_model: Option<String>,
    pub usage: Option<Usage>,
    pub timestamp: f64,
    pub status: AssistantStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Complete,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolItem {
    pub id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub input: Value,
    pub content: Vec<Content>,
    pub details: Option<Value>,
    pub usage: Option<Usage>,
    pub timestamp: f64,
    pub status: ToolStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptItem {
    User(UserItem),
    Assistant(AssistantItem),
    Tool(ToolItem),
}

#[derive(Debug, Clone, PartialEq)]
pub enum AssistantOrTool {
    Assistant(AssistantItem),
    Tool(ToolItem),
}

#[derive(Debug, Clone, PartialEq)]
pub enum FinishedItem {
    AssistantComplete(AssistantItem),
    AssistantError(AssistantItem),
    AssistantAborted(AssistantItem),
    ToolComplete(ToolItem),
    ToolError(ToolItem),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptProgress {
    ItemStarted { item: TranscriptItem },
    AssistantDelta { message_id: String, content_index: f64, kind: String, delta: String },
    ItemUpdated { item: AssistantOrTool },
    ItemFinished { item: FinishedItem },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: f64,
    pub updated_at: Option<f64>,
    pub parent_session_id: Option<String>,
    pub session_name: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSnapshot {
    pub id: String,
    pub name: Option<String>,
    pub cwd: String,
    pub created_at: f64,
    pub updated_at: f64,
    pub phase: SessionPhase,
    pub model: ModelRef,
    pub thinking_level: ThinkingLevel,
    pub attached: bool,
    pub locked: bool,
    pub revision: f64,
    pub transcript: Vec<TranscriptItem>,
    pub queued_steer: Vec<UserItem>,
    pub queued_steer_count: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerSnapshot {
    pub server_id: String,
    pub protocol_version: f64,
    pub revision: f64,
    pub sessions: Vec<SessionMetadata>,
    pub models: Vec<ModelMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolErrorCode {
    Version,
    Busy,
    SessionLocked,
    NotFound,
    InvalidRequest,
    NotImplemented,
    InternalError,
}

impl ProtocolErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::Busy => "busy",
            Self::SessionLocked => "session_locked",
            Self::NotFound => "not_found",
            Self::InvalidRequest => "invalid_request",
            Self::NotImplemented => "not_implemented",
            Self::InternalError => "internal_error",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "version" => Self::Version,
            "busy" => Self::Busy,
            "session_locked" => Self::SessionLocked,
            "not_found" => Self::NotFound,
            "invalid_request" => Self::InvalidRequest,
            "not_implemented" => Self::NotImplemented,
            "internal_error" => Self::InternalError,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolError {
    pub code: ProtocolErrorCode,
    pub message: String,
    pub details: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    List,
    Create {
        cwd: Option<String>,
        name: Option<String>,
        model: Option<ModelRef>,
        thinking_level: Option<ThinkingLevel>,
    },
    Attach { session_id: String },
    Detach { session_id: String },
    Prompt { session_id: String, text: String },
    Steer { session_id: String, text: String },
    Abort { session_id: String },
    SetModel { session_id: String, model: ModelRef },
    SetThinking { session_id: String, thinking_level: ThinkingLevel },
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandResult {
    List { sessions: Vec<SessionMetadata> },
    Create { session: SessionSnapshot },
    Attach { session: SessionSnapshot },
    Prompt { session: SessionSnapshot },
    Steer { session: SessionSnapshot },
    Abort { session: SessionSnapshot },
    SetModel { session: SessionSnapshot },
    SetThinking { session: SessionSnapshot },
    Detach { session_id: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    /// `version` is intentionally any non-negative integer, not a coercible
    /// string, so peers can negotiate.
    Hello { version: f64 },
    Request { id: String, request: Command },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerEvent {
    ServerSnapshot { snapshot: ServerSnapshot },
    SessionSnapshot { snapshot: SessionSnapshot },
    SessionProgress { session_id: String, progress: TranscriptProgress },
    SessionRemoved { session_id: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResponseEnvelope {
    Ok { id: String, result: CommandResult },
    Err { id: String, error: ProtocolError },
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventEnvelope {
    pub event: ServerEvent,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    Hello { connection_id: String, snapshot: ServerSnapshot },
    HelloError { error: ProtocolError },
    Response(ResponseEnvelope),
    Event(EventEnvelope),
}

// ---------------------------------------------------------------------------
// Encoding helpers
// ---------------------------------------------------------------------------

fn kv(key: &str, value: Value) -> (String, Value) {
    (key.to_string(), value)
}

fn str(value: &str) -> Value {
    Value::String(value.to_string())
}

fn obj(entries: Vec<(String, Value)>) -> Value {
    Value::Map(entries)
}

fn arr(items: Vec<Value>) -> Value {
    Value::Array(items)
}

fn num(value: f64) -> Value {
    Value::Number(value)
}

fn model_ref_to_value(value: &ModelRef) -> Value {
    obj(vec![kv("provider", str(&value.provider)), kv("id", str(&value.id))])
}

fn model_cost_to_value(value: &ModelCost) -> Value {
    obj(vec![
        kv("input", num(value.input)),
        kv("output", num(value.output)),
        kv("cacheRead", num(value.cache_read)),
        kv("cacheWrite", num(value.cache_write)),
    ])
}

fn usage_cost_to_value(value: &UsageCost) -> Value {
    obj(vec![
        kv("input", num(value.input)),
        kv("output", num(value.output)),
        kv("cacheRead", num(value.cache_read)),
        kv("cacheWrite", num(value.cache_write)),
        kv("total", num(value.total)),
    ])
}

fn usage_to_value(value: &Usage) -> Value {
    let mut entries = vec![
        kv("input", num(value.input)),
        kv("output", num(value.output)),
        kv("cacheRead", num(value.cache_read)),
        kv("cacheWrite", num(value.cache_write)),
    ];
    if let Some(reasoning) = value.reasoning {
        entries.push(kv("reasoning", num(reasoning)));
    }
    entries.push(kv("totalTokens", num(value.total_tokens)));
    entries.push(kv("cost", usage_cost_to_value(&value.cost)));
    obj(entries)
}

fn content_to_value(value: &Content) -> Value {
    match value {
        Content::Text { text } => obj(vec![kv("type", str("text")), kv("text", str(text))]),
        Content::Thinking { thinking, redacted } => {
            let mut entries = vec![kv("type", str("thinking")), kv("thinking", str(thinking))];
            if let Some(redacted) = redacted {
                entries.push(kv("redacted", Value::Bool(*redacted)));
            }
            obj(entries)
        }
        Content::Image { data, mime_type } => obj(vec![
            kv("type", str("image")),
            kv("data", str(data)),
            kv("mimeType", str(mime_type)),
        ]),
        Content::ToolCall {
            tool_call_id,
            tool_name,
            input,
        } => obj(vec![
            kv("type", str("toolCall")),
            kv("toolCallId", str(tool_call_id)),
            kv("toolName", str(tool_name)),
            kv("input", input.clone()),
        ]),
    }
}

fn assistant_status_to_value(status: &AssistantStatus) -> Vec<(String, Value)> {
    match status {
        AssistantStatus::Streaming => vec![kv("status", str("streaming"))],
        AssistantStatus::Complete { stop_reason } => vec![
            kv("status", str("complete")),
            kv("stopReason", str(stop_reason)),
        ],
        AssistantStatus::Error { error_message } => {
            let mut entries = vec![kv("status", str("error")), kv("stopReason", str("error"))];
            if let Some(message) = error_message {
                entries.push(kv("errorMessage", str(message)));
            }
            entries
        }
        AssistantStatus::Aborted { error_message } => {
            let mut entries = vec![kv("status", str("aborted")), kv("stopReason", str("aborted"))];
            if let Some(message) = error_message {
                entries.push(kv("errorMessage", str(message)));
            }
            entries
        }
    }
}

fn assistant_item_to_value(value: &AssistantItem) -> Value {
    let mut entries = vec![
        kv("id", str(&value.id)),
        kv("role", str("assistant")),
        kv("content", arr(value.content.iter().map(content_to_value).collect())),
        kv("model", model_ref_to_value(&value.model)),
    ];
    if let Some(response_model) = &value.response_model {
        entries.push(kv("responseModel", str(response_model)));
    }
    if let Some(usage) = &value.usage {
        entries.push(kv("usage", usage_to_value(usage)));
    }
    entries.push(kv("timestamp", num(value.timestamp)));
    entries.extend(assistant_status_to_value(&value.status));
    obj(entries)
}

fn tool_item_to_value(value: &ToolItem) -> Value {
    let mut entries = vec![
        kv("id", str(&value.id)),
        kv("role", str("tool")),
        kv("toolCallId", str(&value.tool_call_id)),
        kv("toolName", str(&value.tool_name)),
        kv("input", value.input.clone()),
        kv("content", arr(value.content.iter().map(content_to_value).collect())),
    ];
    if let Some(details) = &value.details {
        entries.push(kv("details", details.clone()));
    }
    if let Some(usage) = &value.usage {
        entries.push(kv("usage", usage_to_value(usage)));
    }
    entries.push(kv("timestamp", num(value.timestamp)));
    entries.push(kv(
        "status",
        str(match value.status {
            ToolStatus::Running => "running",
            ToolStatus::Complete => "complete",
            ToolStatus::Error => "error",
        }),
    ));
    entries.push(kv(
        "isError",
        Value::Bool(value.status == ToolStatus::Error),
    ));
    obj(entries)
}

fn user_item_to_value(value: &UserItem) -> Value {
    obj(vec![
        kv("id", str(&value.id)),
        kv("role", str("user")),
        kv("content", arr(value.content.iter().map(content_to_value).collect())),
        kv("timestamp", num(value.timestamp)),
    ])
}

fn transcript_item_to_value(value: &TranscriptItem) -> Value {
    match value {
        TranscriptItem::User(item) => user_item_to_value(item),
        TranscriptItem::Assistant(item) => assistant_item_to_value(item),
        TranscriptItem::Tool(item) => tool_item_to_value(item),
    }
}

fn session_metadata_to_value(value: &SessionMetadata) -> Value {
    let mut entries = vec![kv("id", str(&value.id)), kv("createdAt", num(value.created_at))];
    if let Some(updated_at) = value.updated_at {
        entries.push(kv("updatedAt", num(updated_at)));
    }
    if let Some(parent_session_id) = &value.parent_session_id {
        entries.push(kv("parentSessionId", str(parent_session_id)));
    }
    if let Some(session_name) = &value.session_name {
        entries.push(kv("sessionName", str(session_name)));
    }
    if let Some(cwd) = &value.cwd {
        entries.push(kv("cwd", str(cwd)));
    }
    obj(entries)
}

fn session_snapshot_to_value(value: &SessionSnapshot) -> Value {
    let mut entries = vec![
        kv("id", str(&value.id)),
        kv("cwd", str(&value.cwd)),
        kv("createdAt", num(value.created_at)),
        kv("updatedAt", num(value.updated_at)),
        kv("phase", str(&value.phase)),
        kv("model", model_ref_to_value(&value.model)),
        kv("thinkingLevel", str(&value.thinking_level)),
        kv("attached", Value::Bool(value.attached)),
        kv("locked", Value::Bool(value.locked)),
        kv("revision", num(value.revision)),
        kv("transcript", arr(value.transcript.iter().map(transcript_item_to_value).collect())),
        kv("queuedSteer", arr(value.queued_steer.iter().map(user_item_to_value).collect())),
        kv("queuedSteerCount", num(value.queued_steer_count)),
    ];
    if let Some(name) = &value.name {
        entries.insert(1, kv("name", str(name)));
    }
    obj(entries)
}

fn server_snapshot_to_value(value: &ServerSnapshot) -> Value {
    obj(vec![
        kv("serverId", str(&value.server_id)),
        kv("protocolVersion", num(value.protocol_version)),
        kv("revision", num(value.revision)),
        kv("sessions", arr(value.sessions.iter().map(session_metadata_to_value).collect())),
        kv("models", arr(value.models.iter().map(model_metadata_to_value).collect())),
    ])
}

fn model_metadata_to_value(value: &ModelMetadata) -> Value {
    obj(vec![
        kv("provider", str(&value.provider)),
        kv("id", str(&value.id)),
        kv("name", str(&value.name)),
        kv("api", str(&value.api)),
        kv("reasoning", Value::Bool(value.reasoning)),
        kv(
            "input",
            arr(value
                .input
                .iter()
                .map(|kind| str(match kind {
                    InputKind::Text => "text",
                    InputKind::Image => "image",
                }))
                .collect()),
        ),
        kv("contextWindow", num(value.context_window)),
        kv("maxTokens", num(value.max_tokens)),
        kv("cost", model_cost_to_value(&value.cost)),
        kv("supportedThinkingLevels", arr(value.supported_thinking_levels.iter().map(|s| str(s)).collect())),
        kv("authenticated", Value::Bool(value.authenticated)),
    ])
}

fn protocol_error_to_value(value: &ProtocolError) -> Value {
    let mut entries = vec![kv("code", str(value.code.as_str())), kv("message", str(&value.message))];
    if let Some(details) = &value.details {
        entries.push(kv("details", details.clone()));
    }
    obj(entries)
}

fn command_to_value(value: &Command) -> Value {
    match value {
        Command::List => obj(vec![kv("command", str("list"))]),
        Command::Create {
            cwd,
            name,
            model,
            thinking_level,
        } => {
            let mut entries = vec![kv("command", str("create"))];
            if let Some(cwd) = cwd {
                entries.push(kv("cwd", str(cwd)));
            }
            if let Some(name) = name {
                entries.push(kv("name", str(name)));
            }
            if let Some(model) = model {
                entries.push(kv("model", model_ref_to_value(model)));
            }
            if let Some(thinking_level) = thinking_level {
                entries.push(kv("thinkingLevel", str(thinking_level)));
            }
            obj(entries)
        }
        Command::Attach { session_id } => obj(vec![
            kv("command", str("attach")),
            kv("sessionId", str(session_id)),
        ]),
        Command::Detach { session_id } => obj(vec![
            kv("command", str("detach")),
            kv("sessionId", str(session_id)),
        ]),
        Command::Prompt { session_id, text } => obj(vec![
            kv("command", str("prompt")),
            kv("sessionId", str(session_id)),
            kv("text", str(text)),
        ]),
        Command::Steer { session_id, text } => obj(vec![
            kv("command", str("steer")),
            kv("sessionId", str(session_id)),
            kv("text", str(text)),
        ]),
        Command::Abort { session_id } => obj(vec![
            kv("command", str("abort")),
            kv("sessionId", str(session_id)),
        ]),
        Command::SetModel { session_id, model } => obj(vec![
            kv("command", str("set_model")),
            kv("sessionId", str(session_id)),
            kv("model", model_ref_to_value(model)),
        ]),
        Command::SetThinking {
            session_id,
            thinking_level,
        } => obj(vec![
            kv("command", str("set_thinking")),
            kv("sessionId", str(session_id)),
            kv("thinkingLevel", str(thinking_level)),
        ]),
    }
}

fn command_result_to_value(value: &CommandResult) -> Value {
    match value {
        CommandResult::List { sessions } => obj(vec![
            kv("command", str("list")),
            kv("sessions", arr(sessions.iter().map(session_metadata_to_value).collect())),
        ]),
        CommandResult::Create { session } => obj(vec![
            kv("command", str("create")),
            kv("session", session_snapshot_to_value(session)),
        ]),
        CommandResult::Attach { session } => obj(vec![
            kv("command", str("attach")),
            kv("session", session_snapshot_to_value(session)),
        ]),
        CommandResult::Prompt { session } => obj(vec![
            kv("command", str("prompt")),
            kv("session", session_snapshot_to_value(session)),
        ]),
        CommandResult::Steer { session } => obj(vec![
            kv("command", str("steer")),
            kv("session", session_snapshot_to_value(session)),
        ]),
        CommandResult::Abort { session } => obj(vec![
            kv("command", str("abort")),
            kv("session", session_snapshot_to_value(session)),
        ]),
        CommandResult::SetModel { session } => obj(vec![
            kv("command", str("set_model")),
            kv("session", session_snapshot_to_value(session)),
        ]),
        CommandResult::SetThinking { session } => obj(vec![
            kv("command", str("set_thinking")),
            kv("session", session_snapshot_to_value(session)),
        ]),
        CommandResult::Detach { session_id } => obj(vec![
            kv("command", str("detach")),
            kv("sessionId", str(session_id)),
        ]),
    }
}

impl ClientMessage {
    pub fn to_value(&self) -> Value {
        match self {
            ClientMessage::Hello { version } => obj(vec![kv("type", str("hello")), kv("version", num(*version))]),
            ClientMessage::Request { id, request } => obj(vec![
                kv("type", str("request")),
                kv("id", str(id)),
                kv("request", command_to_value(request)),
            ]),
        }
    }
}

impl ServerMessage {
    pub fn to_value(&self) -> Value {
        match self {
            ServerMessage::Hello {
                connection_id,
                snapshot,
            } => obj(vec![
                kv("type", str("hello")),
                kv("version", num(PROTOCOL_VERSION)),
                kv("connectionId", str(connection_id)),
                kv("snapshot", server_snapshot_to_value(snapshot)),
            ]),
            ServerMessage::HelloError { error } => obj(vec![
                kv("type", str("hello_error")),
                kv("error", protocol_error_to_value(error)),
            ]),
            ServerMessage::Response(response) => match response {
                ResponseEnvelope::Ok { id, result } => obj(vec![
                    kv("type", str("response")),
                    kv("id", str(id)),
                    kv("ok", Value::Bool(true)),
                    kv("result", command_result_to_value(result)),
                ]),
                ResponseEnvelope::Err { id, error } => obj(vec![
                    kv("type", str("response")),
                    kv("id", str(id)),
                    kv("ok", Value::Bool(false)),
                    kv("error", protocol_error_to_value(error)),
                ]),
            },
            ServerMessage::Event(event) => obj(vec![
                kv("type", str("event")),
                kv("event", server_event_to_value(&event.event)),
            ]),
        }
    }
}

fn server_event_to_value(value: &ServerEvent) -> Value {
    match value {
        ServerEvent::ServerSnapshot { snapshot } => obj(vec![
            kv("type", str("server_snapshot")),
            kv("snapshot", server_snapshot_to_value(snapshot)),
        ]),
        ServerEvent::SessionSnapshot { snapshot } => obj(vec![
            kv("type", str("session_snapshot")),
            kv("snapshot", session_snapshot_to_value(snapshot)),
        ]),
        ServerEvent::SessionProgress {
            session_id,
            progress,
        } => obj(vec![
            kv("type", str("session_progress")),
            kv("sessionId", str(session_id)),
            kv("progress", transcript_progress_to_value(progress)),
        ]),
        ServerEvent::SessionRemoved { session_id } => obj(vec![
            kv("type", str("session_removed")),
            kv("sessionId", str(session_id)),
        ]),
    }
}

fn transcript_progress_to_value(value: &TranscriptProgress) -> Value {
    match value {
        TranscriptProgress::ItemStarted { item } => obj(vec![
            kv("type", str("item_started")),
            kv("item", transcript_item_to_value(item)),
        ]),
        TranscriptProgress::AssistantDelta {
            message_id,
            content_index,
            kind,
            delta,
        } => obj(vec![
            kv("type", str("assistant_delta")),
            kv("messageId", str(message_id)),
            kv("contentIndex", num(*content_index)),
            kv("kind", str(kind)),
            kv("delta", str(delta)),
        ]),
        TranscriptProgress::ItemUpdated { item } => {
            let item = match item {
                AssistantOrTool::Assistant(item) => assistant_item_to_value(item),
                AssistantOrTool::Tool(item) => tool_item_to_value(item),
            };
            obj(vec![kv("type", str("item_updated")), kv("item", item)])
        }
        TranscriptProgress::ItemFinished { item } => {
            let item = match item {
                FinishedItem::AssistantComplete(item)
                | FinishedItem::AssistantError(item)
                | FinishedItem::AssistantAborted(item) => assistant_item_to_value(item),
                FinishedItem::ToolComplete(item) | FinishedItem::ToolError(item) => tool_item_to_value(item),
            };
            obj(vec![kv("type", str("item_finished")), kv("item", item)])
        }
    }
}

impl ServerSnapshot {
    pub fn to_value(&self) -> Value {
        server_snapshot_to_value(self)
    }
}

impl SessionMetadata {
    pub fn to_value(&self) -> Value {
        session_metadata_to_value(self)
    }
}

impl SessionSnapshot {
    pub fn to_value(&self) -> Value {
        session_snapshot_to_value(self)
    }
}

impl ModelMetadata {
    pub fn to_value(&self) -> Value {
        model_metadata_to_value(self)
    }
}

impl ProtocolError {
    pub fn to_value(&self) -> Value {
        protocol_error_to_value(self)
    }
}

impl Command {
    pub fn to_value(&self) -> Value {
        command_to_value(self)
    }
}

impl CommandResult {
    pub fn to_value(&self) -> Value {
        command_result_to_value(self)
    }
}

impl TranscriptItem {
    pub fn to_value(&self) -> Value {
        transcript_item_to_value(self)
    }
}

impl TranscriptProgress {
    pub fn to_value(&self) -> Value {
        transcript_progress_to_value(self)
    }
}

// ---------------------------------------------------------------------------
// Parsing (validation + conversion from Value)
// ---------------------------------------------------------------------------

/// Mirrors `isProtocolValue`: the protocol value space excludes byte strings
/// and only contains plain JSON-like values. Rust's `Value` tree cannot
/// contain cycles, functions, or class instances, so this only rules out
/// `Value::Bytes`.
pub fn check_protocol_value(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => true,
        Value::Bytes(_) => false,
        Value::Array(items) => items.iter().all(check_protocol_value),
        Value::Map(entries) => entries.iter().all(|(_, value)| check_protocol_value(value)),
    }
}

type ParseResult<T> = Result<T, ()>;

fn as_obj<'a>(value: &'a Value) -> ParseResult<&'a [(String, Value)]> {
    value.as_map().ok_or(())
}

fn check_keys(entries: &[(String, Value)], allowed: &[&str]) -> ParseResult<()> {
    if entries.iter().all(|(key, _)| allowed.contains(&key.as_str())) {
        Ok(())
    } else {
        Err(())
    }
}

fn get<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn req_str(entries: &[(String, Value)], key: &str) -> ParseResult<String> {
    match get(entries, key) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => Err(()),
    }
}

fn opt_str(entries: &[(String, Value)], key: &str) -> ParseResult<Option<String>> {
    match get(entries, key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(()),
    }
}

fn req_num(entries: &[(String, Value)], key: &str) -> ParseResult<f64> {
    match get(entries, key) {
        Some(Value::Number(n)) => Ok(*n),
        _ => Err(()),
    }
}

fn req_bool(entries: &[(String, Value)], key: &str) -> ParseResult<bool> {
    match get(entries, key) {
        Some(Value::Bool(b)) => Ok(*b),
        _ => Err(()),
    }
}

fn req_arr<'a>(entries: &'a [(String, Value)], key: &str) -> ParseResult<&'a [Value]> {
    match get(entries, key) {
        Some(Value::Array(items)) => Ok(items),
        _ => Err(()),
    }
}

/// Type.String({ minLength: 1 }) / IdSchema.
fn req_id(entries: &[(String, Value)], key: &str) -> ParseResult<String> {
    let value = req_str(entries, key)?;
    if value.is_empty() {
        Err(())
    } else {
        Ok(value)
    }
}

fn opt_id(entries: &[(String, Value)], key: &str) -> ParseResult<Option<String>> {
    match get(entries, key) {
        None => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(Value::String(_)) => Err(()),
        Some(_) => Err(()),
    }
}

/// Type.Integer({ minimum: 0 }) — f64 keeps the JS `number` semantics.
fn req_integer_min0(entries: &[(String, Value)], key: &str) -> ParseResult<f64> {
    let value = req_num(entries, key)?;
    if value.fract() == 0.0 && value >= 0.0 {
        Ok(value)
    } else {
        Err(())
    }
}

/// Type.Integer({ minimum: 1 }).
fn req_integer_min1(entries: &[(String, Value)], key: &str) -> ParseResult<f64> {
    let value = req_num(entries, key)?;
    if value.fract() == 0.0 && value >= 1.0 {
        Ok(value)
    } else {
        Err(())
    }
}

/// Type.Number({ minimum: 0 }).
fn req_number_min0(entries: &[(String, Value)], key: &str) -> ParseResult<f64> {
    let value = req_num(entries, key)?;
    if value >= 0.0 {
        Ok(value)
    } else {
        Err(())
    }
}

fn opt_integer_min0(entries: &[(String, Value)], key: &str) -> ParseResult<Option<f64>> {
    match get(entries, key) {
        None => Ok(None),
        Some(Value::Number(n)) if n.fract() == 0.0 && *n >= 0.0 => Ok(Some(*n)),
        Some(_) => Err(()),
    }
}

fn opt_bool(entries: &[(String, Value)], key: &str) -> ParseResult<Option<bool>> {
    match get(entries, key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(()),
    }
}

const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
const SESSION_PHASES: [&str; 5] = ["idle", "turn", "compaction", "branch_summary", "retry"];
const ASSISTANT_STOP_REASONS: [&str; 3] = ["stop", "length", "toolUse"];
const DELTA_KINDS: [&str; 3] = ["text", "thinking", "toolCall"];
const ERROR_CODES: [&str; 7] = [
    "version",
    "busy",
    "session_locked",
    "not_found",
    "invalid_request",
    "not_implemented",
    "internal_error",
];

fn parse_enum<'a>(value: &'a Value, allowed: &[&str]) -> ParseResult<&'a str> {
    match value {
        Value::String(s) if allowed.contains(&s.as_str()) => Ok(s.as_str()),
        _ => Err(()),
    }
}

fn req_enum<'a>(entries: &'a [(String, Value)], key: &str, allowed: &[&str]) -> ParseResult<&'a str> {
    match get(entries, key) {
        Some(value) => parse_enum(value, allowed),
        None => Err(()),
    }
}

fn parse_model_ref(value: &Value) -> ParseResult<ModelRef> {
    let entries = as_obj(value)?;
    check_keys(entries, &["provider", "id"])?;
    Ok(ModelRef {
        provider: req_id(entries, "provider")?,
        id: req_id(entries, "id")?,
    })
}

fn parse_model_cost(value: &Value) -> ParseResult<ModelCost> {
    let entries = as_obj(value)?;
    check_keys(entries, &["input", "output", "cacheRead", "cacheWrite"])?;
    Ok(ModelCost {
        input: req_number_min0(entries, "input")?,
        output: req_number_min0(entries, "output")?,
        cache_read: req_number_min0(entries, "cacheRead")?,
        cache_write: req_number_min0(entries, "cacheWrite")?,
    })
}

fn parse_input_kind(value: &Value) -> ParseResult<InputKind> {
    match parse_enum(value, &["text", "image"])? {
        "text" => Ok(InputKind::Text),
        _ => Ok(InputKind::Image),
    }
}

fn parse_model_metadata(value: &Value) -> ParseResult<ModelMetadata> {
    let entries = as_obj(value)?;
    check_keys(
        entries,
        &[
            "provider",
            "id",
            "name",
            "api",
            "reasoning",
            "input",
            "contextWindow",
            "maxTokens",
            "cost",
            "supportedThinkingLevels",
            "authenticated",
        ],
    )?;
    let supported_thinking_levels = req_arr(entries, "supportedThinkingLevels")?;
    if supported_thinking_levels.is_empty() {
        return Err(());
    }
    let mut thinking_levels = Vec::with_capacity(supported_thinking_levels.len());
    for level in supported_thinking_levels {
        thinking_levels.push(parse_enum(level, &THINKING_LEVELS)?.to_string());
    }
    Ok(ModelMetadata {
        provider: req_id(entries, "provider")?,
        id: req_id(entries, "id")?,
        name: {
            let name = req_str(entries, "name")?;
            if name.is_empty() {
                return Err(());
            }
            name
        },
        api: req_id(entries, "api")?,
        reasoning: req_bool(entries, "reasoning")?,
        input: {
            let mut input = Vec::new();
            for kind in req_arr(entries, "input")? {
                input.push(parse_input_kind(kind)?);
            }
            input
        },
        context_window: req_integer_min1(entries, "contextWindow")?,
        max_tokens: req_integer_min1(entries, "maxTokens")?,
        cost: parse_model_cost(get(entries, "cost").ok_or(())?)?,
        supported_thinking_levels: thinking_levels,
        authenticated: req_bool(entries, "authenticated")?,
    })
}

/// Parses a content object by its `type` discriminator. The set of allowed
/// variants is enforced by the caller (user/assistant/tool contexts).
fn parse_content(value: &Value) -> ParseResult<Content> {
    let entries = as_obj(value)?;
    let kind = req_enum(entries, "type", &["text", "thinking", "image", "toolCall"])?;
    match kind {
        "text" => {
            check_keys(entries, &["type", "text"])?;
            Ok(Content::Text {
                text: req_str(entries, "text")?,
            })
        }
        "thinking" => {
            check_keys(entries, &["type", "thinking", "redacted"])?;
            Ok(Content::Thinking {
                thinking: req_str(entries, "thinking")?,
                redacted: opt_bool(entries, "redacted")?,
            })
        }
        "image" => {
            check_keys(entries, &["type", "data", "mimeType"])?;
            let mime_type = req_str(entries, "mimeType")?;
            if mime_type.is_empty() {
                return Err(());
            }
            Ok(Content::Image {
                data: req_str(entries, "data")?,
                mime_type,
            })
        }
        _ => {
            check_keys(entries, &["type", "toolCallId", "toolName", "input"])?;
            Ok(Content::ToolCall {
                tool_call_id: req_id(entries, "toolCallId")?,
                tool_name: req_id(entries, "toolName")?,
                input: get(entries, "input").ok_or(())?.clone(),
            })
        }
    }
}

fn parse_usage_cost(value: &Value) -> ParseResult<UsageCost> {
    let entries = as_obj(value)?;
    check_keys(entries, &["input", "output", "cacheRead", "cacheWrite", "total"])?;
    Ok(UsageCost {
        input: req_number_min0(entries, "input")?,
        output: req_number_min0(entries, "output")?,
        cache_read: req_number_min0(entries, "cacheRead")?,
        cache_write: req_number_min0(entries, "cacheWrite")?,
        total: req_number_min0(entries, "total")?,
    })
}

fn parse_usage(value: &Value) -> ParseResult<Usage> {
    let entries = as_obj(value)?;
    check_keys(entries, &["input", "output", "cacheRead", "cacheWrite", "reasoning", "totalTokens", "cost"])?;
    Ok(Usage {
        input: req_integer_min0(entries, "input")?,
        output: req_integer_min0(entries, "output")?,
        cache_read: req_integer_min0(entries, "cacheRead")?,
        cache_write: req_integer_min0(entries, "cacheWrite")?,
        reasoning: opt_integer_min0(entries, "reasoning")?,
        total_tokens: req_integer_min0(entries, "totalTokens")?,
        cost: parse_usage_cost(get(entries, "cost").ok_or(())?)?,
    })
}

/// Parses the shared assistant/tool properties, then the status variant.
fn parse_assistant_item(value: &Value) -> ParseResult<AssistantItem> {
    let entries = as_obj(value)?;
    check_keys(
        entries,
        &[
            "id",
            "role",
            "content",
            "model",
            "responseModel",
            "usage",
            "timestamp",
            "status",
            "stopReason",
            "errorMessage",
        ],
    )?;
    let status = match req_enum(entries, "status", &["streaming", "complete", "error", "aborted"])? {
        "streaming" => {
            if get(entries, "stopReason").is_some() || get(entries, "errorMessage").is_some() {
                return Err(());
            }
            AssistantStatus::Streaming
        }
        "complete" => {
            let stop_reason = match get(entries, "stopReason") {
                Some(value) => parse_enum(value, &ASSISTANT_STOP_REASONS)?.to_string(),
                None => return Err(()),
            };
            if get(entries, "errorMessage").is_some() {
                return Err(());
            }
            AssistantStatus::Complete { stop_reason }
        }
        "error" => {
            if parse_enum(get(entries, "stopReason").ok_or(())?, &["error"]).is_err() {
                return Err(());
            }
            let error_message = opt_id(entries, "errorMessage")?;
            AssistantStatus::Error { error_message }
        }
        _ => {
            if parse_enum(get(entries, "stopReason").ok_or(())?, &["aborted"]).is_err() {
                return Err(());
            }
            // aborted errorMessage is a plain Type.String() without minLength.
            AssistantStatus::Aborted {
                error_message: opt_str(entries, "errorMessage")?,
            }
        }
    };
    let content = req_arr(entries, "content")?;
    let mut content_items = Vec::with_capacity(content.len());
    for item in content {
        let parsed = parse_content(item)?;
        // AssistantContentSchema = Text | Thinking | ToolCall.
        if matches!(parsed, Content::Image { .. }) {
            return Err(());
        }
        content_items.push(parsed);
    }
    Ok(AssistantItem {
        id: req_id(entries, "id")?,
        content: content_items,
        model: parse_model_ref(get(entries, "model").ok_or(())?)?,
        response_model: opt_id(entries, "responseModel")?,
        usage: match get(entries, "usage") {
            Some(value) => Some(parse_usage(value)?),
            None => None,
        },
        timestamp: req_integer_min0(entries, "timestamp")?,
        status,
    })
}

fn parse_tool_item(value: &Value) -> ParseResult<ToolItem> {
    let entries = as_obj(value)?;
    check_keys(
        entries,
        &[
            "id",
            "role",
            "toolCallId",
            "toolName",
            "input",
            "content",
            "details",
            "usage",
            "timestamp",
            "status",
            "isError",
        ],
    )?;
    let status = match req_enum(entries, "status", &["running", "complete", "error"])? {
        "running" => {
            if !req_bool(entries, "isError")? {
                ToolStatus::Running
            } else {
                return Err(());
            }
        }
        "complete" => {
            if !req_bool(entries, "isError")? {
                ToolStatus::Complete
            } else {
                return Err(());
            }
        }
        _ => {
            if req_bool(entries, "isError")? {
                ToolStatus::Error
            } else {
                return Err(());
            }
        }
    };
    let content = req_arr(entries, "content")?;
    let mut content_items = Vec::with_capacity(content.len());
    for item in content {
        match parse_content(item)? {
            Content::Text { .. } | Content::Image { .. } => content_items.push(parse_content(item)?),
            _ => return Err(()),
        }
    }
    Ok(ToolItem {
        id: req_id(entries, "id")?,
        tool_call_id: req_id(entries, "toolCallId")?,
        tool_name: req_id(entries, "toolName")?,
        input: get(entries, "input").ok_or(())?.clone(),
        content: content_items,
        details: match get(entries, "details") {
            Some(value) => Some(value.clone()),
            None => None,
        },
        usage: match get(entries, "usage") {
            Some(value) => Some(parse_usage(value)?),
            None => None,
        },
        timestamp: req_integer_min0(entries, "timestamp")?,
        status,
    })
}

fn parse_user_item(value: &Value) -> ParseResult<UserItem> {
    let entries = as_obj(value)?;
    check_keys(entries, &["id", "role", "content", "timestamp"])?;
    // UserTranscriptItemSchema requires role: Literal("user").
    if req_enum(entries, "role", &["user"]).is_err() {
        return Err(());
    }
    let content = req_arr(entries, "content")?;
    let mut content_items = Vec::with_capacity(content.len());
    for item in content {
        match parse_content(item)? {
            Content::Text { .. } | Content::Image { .. } => content_items.push(parse_content(item)?),
            _ => return Err(()),
        }
    }
    Ok(UserItem {
        id: req_id(entries, "id")?,
        content: content_items,
        timestamp: req_integer_min0(entries, "timestamp")?,
    })
}

fn parse_transcript_item(value: &Value) -> ParseResult<TranscriptItem> {
    let entries = as_obj(value)?;
    match req_enum(entries, "role", &["user", "assistant", "tool"])? {
        "user" => Ok(TranscriptItem::User(parse_user_item(value)?)),
        "assistant" => Ok(TranscriptItem::Assistant(parse_assistant_item(value)?)),
        _ => Ok(TranscriptItem::Tool(parse_tool_item(value)?)),
    }
}

fn parse_assistant_or_tool(value: &Value) -> ParseResult<AssistantOrTool> {
    let entries = as_obj(value)?;
    match req_enum(entries, "role", &["assistant", "tool"])? {
        "assistant" => Ok(AssistantOrTool::Assistant(parse_assistant_item(value)?)),
        _ => Ok(AssistantOrTool::Tool(parse_tool_item(value)?)),
    }
}

fn parse_finished_item(value: &Value) -> ParseResult<FinishedItem> {
    let entries = as_obj(value)?;
    match req_enum(entries, "role", &["assistant", "tool"])? {
        "assistant" => {
            let status = req_enum(entries, "status", &["complete", "error", "aborted"])?;
            let item = parse_assistant_item(value)?;
            match status {
                "complete" => Ok(FinishedItem::AssistantComplete(item)),
                "error" => Ok(FinishedItem::AssistantError(item)),
                _ => Ok(FinishedItem::AssistantAborted(item)),
            }
        }
        _ => {
            let status = req_enum(entries, "status", &["complete", "error"])?;
            let item = parse_tool_item(value)?;
            match status {
                "complete" => Ok(FinishedItem::ToolComplete(item)),
                _ => Ok(FinishedItem::ToolError(item)),
            }
        }
    }
}

fn parse_transcript_progress(value: &Value) -> ParseResult<TranscriptProgress> {
    let entries = as_obj(value)?;
    let kind = req_enum(entries, "type", &["item_started", "assistant_delta", "item_updated", "item_finished"])?;
    match kind {
        "item_started" => {
            check_keys(entries, &["type", "item"])?;
            Ok(TranscriptProgress::ItemStarted {
                item: parse_transcript_item(get(entries, "item").ok_or(())?)?,
            })
        }
        "assistant_delta" => {
            check_keys(entries, &["type", "messageId", "contentIndex", "kind", "delta"])?;
            Ok(TranscriptProgress::AssistantDelta {
                message_id: req_id(entries, "messageId")?,
                content_index: req_integer_min0(entries, "contentIndex")?,
                kind: req_enum(entries, "kind", &DELTA_KINDS)?.to_string(),
                delta: req_str(entries, "delta")?,
            })
        }
        "item_updated" => {
            check_keys(entries, &["type", "item"])?;
            Ok(TranscriptProgress::ItemUpdated {
                item: parse_assistant_or_tool(get(entries, "item").ok_or(())?)?,
            })
        }
        _ => {
            check_keys(entries, &["type", "item"])?;
            Ok(TranscriptProgress::ItemFinished {
                item: parse_finished_item(get(entries, "item").ok_or(())?)?,
            })
        }
    }
}

fn parse_session_metadata(value: &Value) -> ParseResult<SessionMetadata> {
    let entries = as_obj(value)?;
    check_keys(entries, &["id", "createdAt", "updatedAt", "parentSessionId", "sessionName", "cwd"])?;
    Ok(SessionMetadata {
        id: req_id(entries, "id")?,
        created_at: req_integer_min0(entries, "createdAt")?,
        updated_at: opt_integer_min0(entries, "updatedAt")?,
        parent_session_id: opt_id(entries, "parentSessionId")?,
        session_name: opt_str(entries, "sessionName")?,
        cwd: opt_id(entries, "cwd")?,
    })
}

fn parse_session_snapshot(value: &Value) -> ParseResult<SessionSnapshot> {
    let entries = as_obj(value)?;
    check_keys(
        entries,
        &[
            "id",
            "name",
            "cwd",
            "createdAt",
            "updatedAt",
            "phase",
            "model",
            "thinkingLevel",
            "attached",
            "locked",
            "revision",
            "transcript",
            "queuedSteer",
            "queuedSteerCount",
        ],
    )?;
    let mut transcript = Vec::new();
    for item in req_arr(entries, "transcript")? {
        transcript.push(parse_transcript_item(item)?);
    }
    let mut queued_steer = Vec::new();
    for item in req_arr(entries, "queuedSteer")? {
        queued_steer.push(parse_user_item(item)?);
    }
    Ok(SessionSnapshot {
        id: req_id(entries, "id")?,
        name: opt_str(entries, "name")?,
        cwd: {
            let cwd = req_str(entries, "cwd")?;
            if cwd.is_empty() {
                return Err(());
            }
            cwd
        },
        created_at: req_integer_min0(entries, "createdAt")?,
        updated_at: req_integer_min0(entries, "updatedAt")?,
        phase: req_enum(entries, "phase", &SESSION_PHASES)?.to_string(),
        model: parse_model_ref(get(entries, "model").ok_or(())?)?,
        thinking_level: req_enum(entries, "thinkingLevel", &THINKING_LEVELS)?.to_string(),
        attached: req_bool(entries, "attached")?,
        locked: req_bool(entries, "locked")?,
        revision: req_integer_min0(entries, "revision")?,
        transcript,
        queued_steer,
        queued_steer_count: req_integer_min0(entries, "queuedSteerCount")?,
    })
}

fn parse_server_snapshot(value: &Value) -> ParseResult<ServerSnapshot> {
    let entries = as_obj(value)?;
    check_keys(entries, &["serverId", "protocolVersion", "revision", "sessions", "models"])?;
    let protocol_version = req_num(entries, "protocolVersion")?;
    if protocol_version != PROTOCOL_VERSION {
        return Err(());
    }
    let mut sessions = Vec::new();
    for session in req_arr(entries, "sessions")? {
        sessions.push(parse_session_metadata(session)?);
    }
    let mut models = Vec::new();
    for model in req_arr(entries, "models")? {
        models.push(parse_model_metadata(model)?);
    }
    Ok(ServerSnapshot {
        server_id: req_id(entries, "serverId")?,
        protocol_version,
        revision: req_integer_min0(entries, "revision")?,
        sessions,
        models,
    })
}

fn parse_protocol_error(value: &Value) -> ParseResult<ProtocolError> {
    let entries = as_obj(value)?;
    check_keys(entries, &["code", "message", "details"])?;
    let code = req_enum(entries, "code", &ERROR_CODES)?;
    Ok(ProtocolError {
        code: ProtocolErrorCode::parse(code).expect("checked against ERROR_CODES"),
        message: req_str(entries, "message")?,
        details: match get(entries, "details") {
            Some(value) => Some(value.clone()),
            None => None,
        },
    })
}

fn parse_command(value: &Value) -> ParseResult<Command> {
    let entries = as_obj(value)?;
    let command = req_enum(
        entries,
        "command",
        &[
            "list", "create", "attach", "detach", "prompt", "steer", "abort", "set_model", "set_thinking",
        ],
    )?;
    match command {
        "list" => {
            check_keys(entries, &["command"])?;
            Ok(Command::List)
        }
        "create" => {
            check_keys(entries, &["command", "cwd", "name", "model", "thinkingLevel"])?;
            Ok(Command::Create {
                cwd: opt_id(entries, "cwd")?,
                name: opt_str(entries, "name")?,
                model: match get(entries, "model") {
                    Some(value) => Some(parse_model_ref(value)?),
                    None => None,
                },
                thinking_level: match get(entries, "thinkingLevel") {
                    Some(value) => Some(parse_enum(value, &THINKING_LEVELS)?.to_string()),
                    None => None,
                },
            })
        }
        "attach" => {
            check_keys(entries, &["command", "sessionId"])?;
            Ok(Command::Attach {
                session_id: req_id(entries, "sessionId")?,
            })
        }
        "detach" => {
            check_keys(entries, &["command", "sessionId"])?;
            Ok(Command::Detach {
                session_id: req_id(entries, "sessionId")?,
            })
        }
        "prompt" => {
            check_keys(entries, &["command", "sessionId", "text"])?;
            Ok(Command::Prompt {
                session_id: req_id(entries, "sessionId")?,
                text: req_str(entries, "text")?,
            })
        }
        "steer" => {
            check_keys(entries, &["command", "sessionId", "text"])?;
            Ok(Command::Steer {
                session_id: req_id(entries, "sessionId")?,
                text: req_str(entries, "text")?,
            })
        }
        "abort" => {
            check_keys(entries, &["command", "sessionId"])?;
            Ok(Command::Abort {
                session_id: req_id(entries, "sessionId")?,
            })
        }
        "set_model" => {
            check_keys(entries, &["command", "sessionId", "model"])?;
            Ok(Command::SetModel {
                session_id: req_id(entries, "sessionId")?,
                model: parse_model_ref(get(entries, "model").ok_or(())?)?,
            })
        }
        _ => {
            check_keys(entries, &["command", "sessionId", "thinkingLevel"])?;
            Ok(Command::SetThinking {
                session_id: req_id(entries, "sessionId")?,
                thinking_level: req_enum(entries, "thinkingLevel", &THINKING_LEVELS)?.to_string(),
            })
        }
    }
}

fn parse_command_result(value: &Value) -> ParseResult<CommandResult> {
    let entries = as_obj(value)?;
    let command = req_enum(
        entries,
        "command",
        &[
            "list", "create", "attach", "detach", "prompt", "steer", "abort", "set_model", "set_thinking",
        ],
    )?;
    match command {
        "list" => {
            check_keys(entries, &["command", "sessions"])?;
            let mut sessions = Vec::new();
            for session in req_arr(entries, "sessions")? {
                sessions.push(parse_session_metadata(session)?);
            }
            Ok(CommandResult::List { sessions })
        }
        "detach" => {
            check_keys(entries, &["command", "sessionId"])?;
            Ok(CommandResult::Detach {
                session_id: req_id(entries, "sessionId")?,
            })
        }
        _ => {
            check_keys(entries, &["command", "session"])?;
            let session = parse_session_snapshot(get(entries, "session").ok_or(())?)?;
            match command {
                "create" => Ok(CommandResult::Create { session }),
                "attach" => Ok(CommandResult::Attach { session }),
                "prompt" => Ok(CommandResult::Prompt { session }),
                "steer" => Ok(CommandResult::Steer { session }),
                "abort" => Ok(CommandResult::Abort { session }),
                "set_model" => Ok(CommandResult::SetModel { session }),
                _ => Ok(CommandResult::SetThinking { session }),
            }
        }
    }
}

fn parse_client_message_inner(value: &Value) -> ParseResult<ClientMessage> {
    let entries = as_obj(value)?;
    let kind = req_enum(entries, "type", &["hello", "request"])?;
    match kind {
        "hello" => {
            check_keys(entries, &["type", "version"])?;
            Ok(ClientMessage::Hello {
                version: req_integer_min0(entries, "version")?,
            })
        }
        _ => {
            check_keys(entries, &["type", "id", "request"])?;
            Ok(ClientMessage::Request {
                id: req_id(entries, "id")?,
                request: parse_command(get(entries, "request").ok_or(())?)?,
            })
        }
    }
}

fn parse_server_message_inner(value: &Value) -> ParseResult<ServerMessage> {
    let entries = as_obj(value)?;
    let kind = req_enum(entries, "type", &["hello", "hello_error", "response", "event"])?;
    match kind {
        "hello" => {
            check_keys(entries, &["type", "version", "connectionId", "snapshot"])?;
            let version = req_num(entries, "version")?;
            if version != PROTOCOL_VERSION {
                return Err(());
            }
            Ok(ServerMessage::Hello {
                connection_id: req_id(entries, "connectionId")?,
                snapshot: parse_server_snapshot(get(entries, "snapshot").ok_or(())?)?,
            })
        }
        "hello_error" => {
            check_keys(entries, &["type", "error"])?;
            Ok(ServerMessage::HelloError {
                error: parse_protocol_error(get(entries, "error").ok_or(())?)?,
            })
        }
        "response" => {
            let ok = req_bool(entries, "ok")?;
            if ok {
                // ResponseEnvelopeSchema ok:true variant: strict keys.
                check_keys(entries, &["type", "id", "ok", "result"])?;
                Ok(ServerMessage::Response(ResponseEnvelope::Ok {
                    id: req_id(entries, "id")?,
                    result: parse_command_result(get(entries, "result").ok_or(())?)?,
                }))
            } else {
                check_keys(entries, &["type", "id", "ok", "error"])?;
                Ok(ServerMessage::Response(ResponseEnvelope::Err {
                    id: req_id(entries, "id")?,
                    error: parse_protocol_error(get(entries, "error").ok_or(())?)?,
                }))
            }
        }
        _ => {
            check_keys(entries, &["type", "event"])?;
            let event = as_obj(get(entries, "event").ok_or(())?)?;
            let event_kind = req_enum(
                event,
                "type",
                &["server_snapshot", "session_snapshot", "session_progress", "session_removed"],
            )?;
            let event_value = match event_kind {
                "server_snapshot" => {
                    check_keys(event, &["type", "snapshot"])?;
                    ServerEvent::ServerSnapshot {
                        snapshot: parse_server_snapshot(get(event, "snapshot").ok_or(())?)?,
                    }
                }
                "session_snapshot" => {
                    check_keys(event, &["type", "snapshot"])?;
                    ServerEvent::SessionSnapshot {
                        snapshot: parse_session_snapshot(get(event, "snapshot").ok_or(())?)?,
                    }
                }
                "session_progress" => {
                    check_keys(event, &["type", "sessionId", "progress"])?;
                    ServerEvent::SessionProgress {
                        session_id: req_id(event, "sessionId")?,
                        progress: parse_transcript_progress(get(event, "progress").ok_or(())?)?,
                    }
                }
                _ => {
                    check_keys(event, &["type", "sessionId"])?;
                    ServerEvent::SessionRemoved {
                        session_id: req_id(event, "sessionId")?,
                    }
                }
            };
            Ok(ServerMessage::Event(EventEnvelope {
                event: event_value,
            }))
        }
    }
}

pub fn parse_client_message(value: &Value) -> Result<ClientMessage, ()> {
    if check_protocol_value(value) {
        parse_client_message_inner(value)
    } else {
        Err(())
    }
}

pub fn parse_server_message(value: &Value) -> Result<ServerMessage, ()> {
    if check_protocol_value(value) {
        parse_server_message_inner(value)
    } else {
        Err(())
    }
}
