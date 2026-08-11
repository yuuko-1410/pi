//! Session types, port of `packages/agent/src/harness/session/types.ts`.
//!
//! Entry/record models for durable session storage. `ProvisionedEntry` is an
//! entry without storage-assigned fields (parentId/seq/timestamp), mirroring
//! the JS Omit.

use pi_ai::types::Usage;
use crate::types::AgentMessage;

pub type JsonValue = pi_ai::types::JsonValue;

pub type SessionStopReason = String; // Exclude<StopReason, "pending"> | "deferred"

pub trait IdGenerator {
    fn next(&self) -> String;
}

#[derive(Clone, Debug, PartialEq)]
pub struct EntryBase {
    pub type_: String,
    pub id: String,
    /// Shared sequence; read-side, storage-assigned.
    pub seq: f64,
    /// Storage-assigned: the appending lane's leaf.
    pub parent_id: Option<String>,
    /// Unix ms, storage-assigned.
    pub timestamp: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MessageEntry {
    pub base: EntryBase,
    pub message: AgentMessage,
    pub terminate: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelChangeEntry {
    pub base: EntryBase,
    pub provider: String,
    pub model_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThinkingLevelEntry {
    pub base: EntryBase,
    pub thinking_level: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveToolsEntry {
    pub base: EntryBase,
    pub active_tool_names: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompactionEntry {
    pub base: EntryBase,
    pub summary: String,
    pub retained_tail: Vec<AgentMessage>,
    pub tokens_before: f64,
    pub details: Option<JsonValue>,
    pub usage: Option<Usage>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BranchSummaryEntry {
    pub base: EntryBase,
    pub from_id: String,
    pub summary: String,
    pub details: Option<JsonValue>,
    pub usage: Option<Usage>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CustomEntry {
    pub base: EntryBase,
    pub custom_type: String,
    pub data: Option<JsonValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Message(MessageEntry),
    ModelChange(ModelChangeEntry),
    ThinkingLevelChange(ThinkingLevelEntry),
    ActiveToolsChange(ActiveToolsEntry),
    Compaction(CompactionEntry),
    BranchSummary(BranchSummaryEntry),
    Custom(CustomEntry),
}

/// An entry without storage-assigned fields (parent_id/seq/timestamp).
pub type ProvisionedEntry = Entry;

#[derive(Clone, Debug, PartialEq)]
pub struct RecordBase {
    pub id: String,
    pub seq: f64,
    pub lane: String,
    pub timestamp: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RunIntent {
    Run {
        /// Normalized caller input before before_run; kept for suspended
        /// operations and before_resume.
        original_prompt: Vec<AgentMessage>,
        /// Captured nextRun items, then the prompt, then before_run
        /// injections.
        initial_messages: Vec<ProvisionedEntry>,
        system_prompt_override: Option<String>,
        resume_data: Option<Vec<(String, JsonValue)>>,
    },
    Compaction {
        custom_instructions: Option<String>,
        result_entry_id: String,
    },
    Navigation {
        target_id: Option<String>,
        summarize: bool,
        custom_instructions: Option<String>,
        label: Option<String>,
        summary_entry_id: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct OperationStartedRecord {
    pub base: RecordBase,
    pub source_leaf_id: Option<String>,
    pub intent: RunIntent,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AbortRequestedRecord {
    pub base: RecordBase,
    pub run_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OperationOutcome {
    Completed,
    Aborted,
    Failed,
    Declined,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OperationFinishedRecord {
    pub base: RecordBase,
    pub run_id: String,
    pub outcome: OperationOutcome,
    pub error: Option<(String, String)>,
}

pub type CompactionReason = String;

#[derive(Clone, Debug, PartialEq)]
pub struct StepAttemptRecord {
    pub base: RecordBase,
    pub run_id: String,
    pub step: String,
    pub attempt: f64,
    pub result_entry_id: String,
    pub compaction_reason: Option<CompactionReason>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolStartedRecord {
    pub base: RecordBase,
    pub run_id: String,
    pub assistant_entry_id: String,
    pub tool_index: f64,
    pub tool_call_id: String,
    pub tool_name: String,
    pub effective_args: Vec<(String, JsonValue)>,
    pub result_entry_id: String,
    pub replay: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueEnqueuedRecord {
    pub base: RecordBase,
    pub queue: String,
    pub run_id: Option<String>,
    pub target: ProvisionedEntry,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueCancelledRecord {
    pub base: RecordBase,
    pub run_id: Option<String>,
    pub entry_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WriteDeferredRecord {
    pub base: RecordBase,
    pub run_id: String,
    pub target: ProvisionedEntry,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageRecord {
    pub base: RecordBase,
    pub usage: Usage,
    pub cause: String,
    pub run_id: Option<String>,
    pub entry_id: Option<String>,
    pub attempt: Option<f64>,
    pub stop_reason: Option<SessionStopReason>,
    pub tool_call_id: Option<String>,
    pub details: Option<JsonValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LaneRecord {
    OperationStarted(OperationStartedRecord),
    AbortRequested(AbortRequestedRecord),
    OperationFinished(OperationFinishedRecord),
    StepAttempt(StepAttemptRecord),
    ToolStarted(ToolStartedRecord),
    QueueEnqueued(QueueEnqueuedRecord),
    QueueCancelled(QueueCancelledRecord),
    WriteDeferred(WriteDeferredRecord),
    Usage(UsageRecord),
}

pub type NewRecord = LaneRecord;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryOrder {
    NewestFirst,
    OldestFirst,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntryCursor {
    pub after_seq: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntryQuery {
    pub type_: Option<String>,
    /// For type "custom".
    pub custom_type: Option<String>,
    /// Default newestFirst.
    pub order: Option<EntryOrder>,
    /// Positive maximum number of matching entries.
    pub limit: Option<f64>,
    pub cursor: Option<EntryCursor>,
}

/// Bounds of a branch scan. Default: the whole path, leaf to root.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BranchBounds {
    /// Default: the view's lane leaf.
    pub start: Option<String>,
    /// Scan ends after the first match, inclusive.
    pub stop_at_type: Option<String>,
    pub stop_at_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordQuery {
    /// Exact lane match. Omit to query every lane.
    pub lane: Option<String>,
    /// Exact record discriminant match.
    pub type_: Option<String>,
    /// Operation identity: matches OperationStartedRecord.id and the runId
    /// property of operation-owned records.
    pub run_id: Option<String>,
    /// Exact operation intent kind. Valid only with type "operation_started".
    pub operation_kind: Option<String>,
    /// Exclusive chronological lower bound: seq > after_seq.
    pub after_seq: Option<f64>,
    /// Sequence order. Default: newestFirst.
    pub order: Option<EntryOrder>,
    /// Positive maximum number of matching records.
    pub limit: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: f64,
    pub parent_session_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionStats {
    pub message_count: f64,
    pub cached_tokens: f64,
    pub uncached_tokens: f64,
    pub total_tokens: f64,
    pub cost_total: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LanePointer {
    pub lane: String,
    pub leaf_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LogItem {
    Entry { seq: f64, entry: Entry },
    Record { seq: f64, record: LaneRecord },
    Lane { seq: f64, lane: String, leaf_id: Option<String> },
    NameFact { seq: f64, name: Option<String> },
    LabelFact { seq: f64, target_id: String, label: Option<String> },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogOptions {
    pub after_seq: Option<f64>,
    pub limit: Option<f64>,
}

pub type SessionErrorCode = &'static str;

#[derive(Clone, Debug, PartialEq)]
pub struct SessionError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SessionError {}

impl SessionError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// Storage backend contract, port of `SessionStorage`.
pub trait SessionStorage: Send + Sync {
    fn get_metadata(&self) -> Result<SessionMetadata, SessionError>;

    // Lanes
    fn get_lanes(&self) -> Result<Vec<LanePointer>, SessionError>;
    fn create_lane(&self, lane: &str, at: Option<&str>) -> Result<(), SessionError>;
    fn move_lane(&self, lane: &str, to: Option<&str>) -> Result<(), SessionError>;

    // Entries and Records
    fn append_entry(&self, entry: Entry, lane: &str) -> Result<Entry, SessionError>;
    fn append_record(&self, record: LaneRecord) -> Result<LaneRecord, SessionError>;

    // Reads
    fn get_entry(&self, id: &str) -> Result<Option<Entry>, SessionError>;
    fn find_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>, SessionError>;
    /// start is mandatory here (as opposed to SessionTree's
    /// find_entries_on_branch); defaulting to a lane's leaf is view sugar.
    fn find_entries_on_branch(&self, query: &EntryQuery, bounds: &BranchBounds, start: &str) -> Result<Vec<Entry>, SessionError>;
    fn find_records(&self, query: &RecordQuery) -> Result<Vec<LaneRecord>, SessionError>;
    /// Returns unfinished operation starts newest first.
    fn find_open_operations(&self, lane: &str, limit: Option<f64>) -> Result<Vec<OperationStartedRecord>, SessionError>;
    fn get_log(&self, options: &LogOptions) -> Result<Vec<LogItem>, SessionError>;

    // Global facts
    fn get_name(&self) -> Result<Option<String>, SessionError>;
    fn set_name(&self, name: Option<&str>) -> Result<(), SessionError>;
    fn get_label(&self, id: &str) -> Result<Option<String>, SessionError>;
    fn set_label(&self, id: &str, label: Option<&str>) -> Result<(), SessionError>;
    fn get_stats(&self) -> Result<SessionStats, SessionError>;
}

/// Read/write view contract, port of `SessionTree`.
pub trait SessionTree {
    fn get_leaf_id(&self) -> Result<Option<String>, SessionError>;
    fn get_entry(&self, id: &str) -> Result<Option<Entry>, SessionError>;
    fn get_stats(&self) -> Result<SessionStats, SessionError>;

    fn get_name(&self) -> Result<Option<String>, SessionError>;
    fn set_name(&self, name: Option<&str>) -> Result<(), SessionError>;
    fn get_label(&self, target_id: &str) -> Result<Option<String>, SessionError>;
    fn set_label(&self, target_id: &str, label: Option<&str>) -> Result<(), SessionError>;

    /// Session-wide, all branches, sequence order.
    fn find_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>, SessionError>;
    fn find_entry(&self, query: &EntryQuery) -> Result<Option<Entry>, SessionError>;

    /// Branch-scoped: the path from start toward root.
    fn find_entries_on_branch(&self, query: &EntryQuery, bounds: &BranchBounds) -> Result<Vec<Entry>, SessionError>;
    fn find_entry_on_branch(&self, query: &EntryQuery, bounds: &BranchBounds) -> Result<Option<Entry>, SessionError>;

    // Writes. Resolve on durable acceptance; the returned id is the entry's id.
    fn append_message(&self, message: AgentMessage) -> Result<String, SessionError>;
    fn append_custom_entry(&self, custom_type: &str, data: Option<JsonValue>) -> Result<String, SessionError>;
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionCreateOptions {
    pub id: Option<String>,
    pub parent_session_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ForkOptions {
    Branch {
        entry_id: Option<String>,
        position: Option<String>,
    },
    Tree,
}

/// Repository contract, port of `SessionRepo`.
pub trait SessionRepo {
    fn create(&mut self, options: &SessionCreateOptions) -> Result<(), SessionError>;
    /// Opens the session for writing and acquires any backend writer claim.
    fn open(&self, metadata: &SessionMetadata) -> Result<(), SessionError>;
    /// Lists session metadata without opening sessions.
    fn list(&self) -> Result<Vec<SessionMetadata>, SessionError>;
    fn delete(&mut self, metadata: &SessionMetadata) -> Result<(), SessionError>;
    fn fork(&mut self, source: &SessionMetadata, options: &ForkOptions, create: &SessionCreateOptions) -> Result<(), SessionError>;
}

impl Entry {
    pub fn type_name(&self) -> &'static str {
        match self {
            Entry::Message(_) => "message",
            Entry::ModelChange(_) => "model_change",
            Entry::ThinkingLevelChange(_) => "thinking_level_change",
            Entry::ActiveToolsChange(_) => "active_tools_change",
            Entry::Compaction(_) => "compaction",
            Entry::BranchSummary(_) => "branch_summary",
            Entry::Custom(_) => "custom",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Entry::Message(entry) => &entry.base.id,
            Entry::ModelChange(entry) => &entry.base.id,
            Entry::ThinkingLevelChange(entry) => &entry.base.id,
            Entry::ActiveToolsChange(entry) => &entry.base.id,
            Entry::Compaction(entry) => &entry.base.id,
            Entry::BranchSummary(entry) => &entry.base.id,
            Entry::Custom(entry) => &entry.base.id,
        }
    }

    pub fn seq(&self) -> f64 {
        match self {
            Entry::Message(entry) => entry.base.seq,
            Entry::ModelChange(entry) => entry.base.seq,
            Entry::ThinkingLevelChange(entry) => entry.base.seq,
            Entry::ActiveToolsChange(entry) => entry.base.seq,
            Entry::Compaction(entry) => entry.base.seq,
            Entry::BranchSummary(entry) => entry.base.seq,
            Entry::Custom(entry) => entry.base.seq,
        }
    }

    pub fn parent_id(&self) -> Option<&str> {
        match self {
            Entry::Message(entry) => entry.base.parent_id.as_deref(),
            Entry::ModelChange(entry) => entry.base.parent_id.as_deref(),
            Entry::ThinkingLevelChange(entry) => entry.base.parent_id.as_deref(),
            Entry::ActiveToolsChange(entry) => entry.base.parent_id.as_deref(),
            Entry::Compaction(entry) => entry.base.parent_id.as_deref(),
            Entry::BranchSummary(entry) => entry.base.parent_id.as_deref(),
            Entry::Custom(entry) => entry.base.parent_id.as_deref(),
        }
    }

    pub fn set_seq(&mut self, seq: f64) {
        let base = match self {
            Entry::Message(entry) => &mut entry.base,
            Entry::ModelChange(entry) => &mut entry.base,
            Entry::ThinkingLevelChange(entry) => &mut entry.base,
            Entry::ActiveToolsChange(entry) => &mut entry.base,
            Entry::Compaction(entry) => &mut entry.base,
            Entry::BranchSummary(entry) => &mut entry.base,
            Entry::Custom(entry) => &mut entry.base,
        };
        base.seq = seq;
    }

    pub fn set_parent_id(&mut self, parent_id: Option<String>) {
        let base = match self {
            Entry::Message(entry) => &mut entry.base,
            Entry::ModelChange(entry) => &mut entry.base,
            Entry::ThinkingLevelChange(entry) => &mut entry.base,
            Entry::ActiveToolsChange(entry) => &mut entry.base,
            Entry::Compaction(entry) => &mut entry.base,
            Entry::BranchSummary(entry) => &mut entry.base,
            Entry::Custom(entry) => &mut entry.base,
        };
        base.parent_id = parent_id;
    }

    pub fn custom_type(&self) -> Option<&str> {
        match self {
            Entry::Custom(entry) => Some(&entry.custom_type),
            _ => None,
        }
    }
}

impl LaneRecord {
    pub fn type_name(&self) -> &'static str {
        match self {
            LaneRecord::OperationStarted(_) => "operation_started",
            LaneRecord::AbortRequested(_) => "abort_requested",
            LaneRecord::OperationFinished(_) => "operation_finished",
            LaneRecord::StepAttempt(_) => "step_attempt",
            LaneRecord::ToolStarted(_) => "tool_started",
            LaneRecord::QueueEnqueued(_) => "queue_enqueued",
            LaneRecord::QueueCancelled(_) => "queue_cancelled",
            LaneRecord::WriteDeferred(_) => "write_deferred",
            LaneRecord::Usage(_) => "usage",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            LaneRecord::OperationStarted(record) => &record.base.id,
            LaneRecord::AbortRequested(record) => &record.base.id,
            LaneRecord::OperationFinished(record) => &record.base.id,
            LaneRecord::StepAttempt(record) => &record.base.id,
            LaneRecord::ToolStarted(record) => &record.base.id,
            LaneRecord::QueueEnqueued(record) => &record.base.id,
            LaneRecord::QueueCancelled(record) => &record.base.id,
            LaneRecord::WriteDeferred(record) => &record.base.id,
            LaneRecord::Usage(record) => &record.base.id,
        }
    }

    pub fn seq(&self) -> f64 {
        match self {
            LaneRecord::OperationStarted(record) => record.base.seq,
            LaneRecord::AbortRequested(record) => record.base.seq,
            LaneRecord::OperationFinished(record) => record.base.seq,
            LaneRecord::StepAttempt(record) => record.base.seq,
            LaneRecord::ToolStarted(record) => record.base.seq,
            LaneRecord::QueueEnqueued(record) => record.base.seq,
            LaneRecord::QueueCancelled(record) => record.base.seq,
            LaneRecord::WriteDeferred(record) => record.base.seq,
            LaneRecord::Usage(record) => record.base.seq,
        }
    }

    pub fn lane(&self) -> &str {
        match self {
            LaneRecord::OperationStarted(record) => &record.base.lane,
            LaneRecord::AbortRequested(record) => &record.base.lane,
            LaneRecord::OperationFinished(record) => &record.base.lane,
            LaneRecord::StepAttempt(record) => &record.base.lane,
            LaneRecord::ToolStarted(record) => &record.base.lane,
            LaneRecord::QueueEnqueued(record) => &record.base.lane,
            LaneRecord::QueueCancelled(record) => &record.base.lane,
            LaneRecord::WriteDeferred(record) => &record.base.lane,
            LaneRecord::Usage(record) => &record.base.lane,
        }
    }

    /// The runId property when the record is operation-owned.
    pub fn run_id(&self) -> Option<&str> {
        match self {
            LaneRecord::OperationStarted(_) => None, // matched by record id
            LaneRecord::AbortRequested(record) => Some(&record.run_id),
            LaneRecord::OperationFinished(record) => Some(&record.run_id),
            LaneRecord::StepAttempt(record) => Some(&record.run_id),
            LaneRecord::ToolStarted(record) => Some(&record.run_id),
            LaneRecord::QueueEnqueued(record) => record.run_id.as_deref(),
            LaneRecord::QueueCancelled(record) => record.run_id.as_deref(),
            LaneRecord::WriteDeferred(record) => Some(&record.run_id),
            LaneRecord::Usage(record) => record.run_id.as_deref(),
        }
    }

    pub fn set_seq_timestamp(&mut self, seq: f64, timestamp: f64) {
        let base = match self {
            LaneRecord::OperationStarted(record) => &mut record.base,
            LaneRecord::AbortRequested(record) => &mut record.base,
            LaneRecord::OperationFinished(record) => &mut record.base,
            LaneRecord::StepAttempt(record) => &mut record.base,
            LaneRecord::ToolStarted(record) => &mut record.base,
            LaneRecord::QueueEnqueued(record) => &mut record.base,
            LaneRecord::QueueCancelled(record) => &mut record.base,
            LaneRecord::WriteDeferred(record) => &mut record.base,
            LaneRecord::Usage(record) => &mut record.base,
        };
        base.seq = seq;
        base.timestamp = timestamp;
    }
}

impl OperationStartedRecord {
    pub fn intent_kind(&self) -> &str {
        match &self.intent {
            RunIntent::Run { .. } => "run",
            RunIntent::Compaction { .. } => "compaction",
            RunIntent::Navigation { .. } => "navigation",
        }
    }
}
