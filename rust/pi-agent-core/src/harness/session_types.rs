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
    pub entry_id: Option<String>,
    pub order: Option<EntryOrder>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntryQuery {
    pub include_types: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BranchBounds {
    pub start_id: Option<String>,
    pub end_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordQuery {
    pub include_types: Option<Vec<String>>,
    pub lane: Option<String>,
    pub run_id: Option<String>,
    pub before_seq: Option<f64>,
    pub after_seq: Option<f64>,
    pub cursor: Option<EntryCursor>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: f64,
    pub updated_at: Option<f64>,
    pub parent_session_id: Option<String>,
    pub session_name: Option<String>,
    pub cwd: Option<String>,
}
