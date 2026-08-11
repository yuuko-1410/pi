//! Lane reducer, port of `packages/agent/src/harness/reducer.ts`.
//!
//! Purely reconstructs one lane's orchestration state from its bounded
//! recovery inputs, validating the single-writer record protocol.

use std::collections::{HashMap, HashSet};

use pi_ai::types::{AssistantMessage, DeferredHandle, StopReason};

use crate::harness::session_types::{
    Entry, LaneRecord, OperationStartedRecord, ProvisionedEntry, QueueEnqueuedRecord, RunIntent,
    StepAttemptRecord, ToolStartedRecord,
};
use crate::types::{AgentMessage, AgentToolCall};

#[derive(Clone, Debug, PartialEq)]
pub struct RecordLogCorruption {
    pub reason: String,
    pub message: String,
}

impl std::fmt::Display for RecordLogCorruption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.reason, self.message)
    }
}

impl std::error::Error for RecordLogCorruption {}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordLogSlice {
    pub lane: String,
    pub open_operations: Vec<OperationStartedRecord>,
    pub records: Vec<LaneRecord>,
    /// Operation-owned entries plus entries fetched directly by provisioned
    /// or referenced ids.
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EffectiveLaneConfiguration {
    pub model: (String, String),
    pub thinking_level: String,
    pub active_tool_names: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalFailureState {
    pub entry_id: String,
    pub source: String,
    pub message: AssistantMessage,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolBatchCallState {
    pub tool_index: f64,
    pub tool_call: AgentToolCall,
    pub started: Option<ToolStartedRecord>,
    pub result_exists: bool,
    pub terminate: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolBatchState {
    pub assistant_entry_id: String,
    pub calls: Vec<ToolBatchCallState>,
    pub truncated: bool,
    pub unresolved: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaneStepState {
    pub kind: String,
    pub attempts: f64,
    pub result_entry_id: String,
    pub compaction_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewestOwnState {
    pub entry_id: String,
    pub type_: String,
    pub role: Option<String>,
    pub stop_reason: Option<StopReason>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaneOperationState {
    pub id: String,
    pub kind: String,
    pub intent: RunIntent,
    pub aborting: bool,
    pub step: Option<LaneStepState>,
    pub tool_batch: Option<ToolBatchState>,
    pub missing_initial_messages: Vec<ProvisionedEntry>,
    pub pending_steer: Vec<ProvisionedEntry>,
    pub pending_follow_up: Vec<ProvisionedEntry>,
    pub pending_writes: Vec<ProvisionedEntry>,
    pub deferred: Option<DeferredHandle>,
    pub overflow_recovery_used: bool,
    pub newest_own: Option<NewestOwnState>,
    pub targets: LaneTargets,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LaneTargets {
    pub result: Option<bool>,
    pub summary: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaneState {
    pub lane: String,
    pub leaf_id: Option<String>,
    pub operation: Option<LaneOperationState>,
    pub pending_next_run: Vec<ProvisionedEntry>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaneReductionInput {
    pub lane: String,
    pub leaf_id: Option<String>,
    pub open_operations: Vec<OperationStartedRecord>,
    pub records: Vec<LaneRecord>,
    pub entries: Vec<Entry>,
    /// Entries appended by the open operation, oldest first.
    pub own_entries: Vec<Entry>,
    /// Bounded effective-state lookups at the operation anchor or idle leaf,
    /// oldest first.
    pub configuration_entries: Vec<Entry>,
    /// Harness option fallbacks used when no persisted value exists.
    pub defaults: EffectiveLaneConfiguration,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaneReductionResult {
    pub lane_state: LaneState,
    pub effective_configuration: EffectiveLaneConfiguration,
    pub terminal_failure: Option<TerminalFailureState>,
}

fn corrupt(reason: &str, message: String) -> RecordLogCorruption {
    RecordLogCorruption {
        reason: reason.to_string(),
        message,
    }
}

fn has_run_id(record: &LaneRecord) -> Option<&str> {
    record.run_id()
}

/// Payload equality ignoring storage-assigned fields (parentId/seq/timestamp).
fn matches_provisioned_entry(entry: &Entry, target: &ProvisionedEntry) -> bool {
    let mut entry = entry.clone();
    entry.set_parent_id(None);
    entry.set_seq(0.0);
    entry.set_timestamp(0.0);
    let mut target = target.clone();
    target.set_parent_id(None);
    target.set_seq(0.0);
    target.set_timestamp(0.0);
    entry == target
}

fn validate_exact_provisioned_entry(
    entries_by_id: &HashMap<String, Entry>,
    target: &ProvisionedEntry,
) -> Result<(), RecordLogCorruption> {
    if let Some(entry) = entries_by_id.get(target.id()) {
        if !matches_provisioned_entry(entry, target) {
            return Err(corrupt(
                "provisioned_entry_mismatch",
                format!(
                    "Provisioned entry {} exists with content different from its intent",
                    target.id()
                ),
            ));
        }
    }
    Ok(())
}

fn validate_result_entry(
    entries_by_id: &HashMap<String, Entry>,
    result_entry_id: &str,
    matches: impl Fn(&Entry) -> bool,
    description: &str,
) -> Result<(), RecordLogCorruption> {
    if let Some(entry) = entries_by_id.get(result_entry_id) {
        if !matches(entry) {
            return Err(corrupt(
                "provisioned_entry_mismatch",
                format!("Provisioned {description} entry {result_entry_id} exists with different content"),
            ));
        }
    }
    Ok(())
}

fn validate_attempt_reason(record: &StepAttemptRecord) -> Result<(), RecordLogCorruption> {
    let reason = record.compaction_reason.as_deref();
    if record.step == "compaction" {
        if !matches!(reason, Some("manual" | "threshold" | "overflow")) {
            return Err(corrupt(
                "invalid_compaction_reason",
                format!("Compaction attempt {} has no valid compaction reason", record.base.id),
            ));
        }
    } else if reason.is_some() {
        return Err(corrupt(
            "invalid_compaction_reason",
            format!("{} attempt {} has a compaction reason", record.step, record.base.id),
        ));
    }
    Ok(())
}

struct AttemptSeries {
    record: StepAttemptRecord,
}

fn validate_attempt_sequence(
    record: &StepAttemptRecord,
    previous: Option<&AttemptSeries>,
    entries_by_id: &HashMap<String, Entry>,
) -> Result<(), RecordLogCorruption> {
    let previous_record = previous.map(|p| &p.record);
    let previous_result = previous_record
        .and_then(|previous| entries_by_id.get(&previous.result_entry_id));
    let continues_series = previous_record.is_some_and(|previous| {
        previous.step == record.step
            && previous_result.is_none_or(|previous_result| previous_result.seq() >= record.base.seq)
    });
    let expected_attempt = if continues_series {
        previous_record.expect("checked above").attempt + 1.0
    } else {
        1.0
    };
    if record.attempt != expected_attempt {
        return Err(corrupt(
            "non_consecutive_attempt",
            format!(
                "{} attempt {} is {}; expected {}",
                record.step, record.base.id, record.attempt, expected_attempt
            ),
        ));
    }
    if !continues_series || record.step == "assistant" || previous_record.is_none() {
        return Ok(());
    }
    let previous = previous_record.expect("checked above");
    if record.result_entry_id != previous.result_entry_id {
        return Err(corrupt(
            "inconsistent_step",
            format!("{} attempts disagree on their result entry id", record.step),
        ));
    }
    if record.compaction_reason != previous.compaction_reason {
        return Err(corrupt(
            "inconsistent_step",
            format!("{} attempts disagree on their compaction reason", record.step),
        ));
    }
    Ok(())
}

fn validate_attempt_result(
    entries_by_id: &HashMap<String, Entry>,
    record: &StepAttemptRecord,
) -> Result<(), RecordLogCorruption> {
    match record.step.as_str() {
        "assistant" => validate_result_entry(
            entries_by_id,
            &record.result_entry_id,
            |entry| {
                matches!(entry, Entry::Message(message) if matches!(
                    &message.message,
                    AgentMessage::Llm(pi_ai::types::Message::Assistant(_))
                ))
            },
            "assistant result",
        ),
        "compaction" => validate_result_entry(
            entries_by_id,
            &record.result_entry_id,
            |entry| entry.type_name() == "compaction",
            "compaction result",
        ),
        _ => validate_result_entry(
            entries_by_id,
            &record.result_entry_id,
            |entry| entry.type_name() == "branch_summary",
            "branch-summary result",
        ),
    }
}

fn validate_tool_start(
    record: &ToolStartedRecord,
    entries_by_id: &HashMap<String, Entry>,
    invocations: &mut HashSet<String>,
) -> Result<(), RecordLogCorruption> {
    let invocation = format!("{}\u{0}{}", record.assistant_entry_id, record.tool_index);
    if !invocations.insert(invocation.clone()) {
        return Err(corrupt(
            "duplicate_tool_invocation",
            format!("Tool invocation {} is duplicated", invocation.replace('\u{0}', ":")),
        ));
    }

    let assistant_entry = entries_by_id.get(&record.assistant_entry_id);
    let Some(Entry::Message(assistant)) = assistant_entry else {
        return Err(corrupt(
            "tool_call_mismatch",
            format!("Tool start {} does not reference an assistant entry", record.base.id),
        ));
    };
    let AgentMessage::Llm(pi_ai::types::Message::Assistant(assistant_message)) = &assistant.message else {
        return Err(corrupt(
            "tool_call_mismatch",
            format!("Tool start {} does not reference an assistant entry", record.base.id),
        ));
    };
    let tool_calls: Vec<&AgentToolCall> = assistant_message
        .content
        .iter()
        .filter_map(|content| match content {
            pi_ai::types::Content::ToolCall(tool_call) => Some(tool_call),
            _ => None,
        })
        .collect();
    let tool_call = tool_calls.get(record.tool_index as usize);
    if tool_call.is_none_or(|tool_call| {
        tool_call.id != record.tool_call_id || tool_call.name != record.tool_name
    }) {
        return Err(corrupt(
            "tool_call_mismatch",
            format!("Tool start {} does not match its assistant tool-call ordinal", record.base.id),
        ));
    }

    validate_result_entry(
        entries_by_id,
        &record.result_entry_id,
        |entry| {
            matches!(entry, Entry::Message(message) if matches!(
                &message.message,
                AgentMessage::Llm(pi_ai::types::Message::ToolResult(tool_result))
                    if tool_result.tool_call_id == record.tool_call_id
                        && tool_result.tool_name == record.tool_name
            ))
        },
        "tool result",
    )
}

fn validate_deferred_handles(entries: &[Entry]) -> Result<(), RecordLogCorruption> {
    for entry in entries {
        if let Entry::Message(message) = entry {
            if let AgentMessage::Llm(pi_ai::types::Message::Assistant(assistant)) = &message.message {
                if assistant.stop_reason == StopReason::Deferred && assistant.deferred.is_none() {
                    return Err(corrupt(
                        "invalid_deferred_handle",
                        format!("Deferred assistant entry {} does not carry a handle", entry.id()),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_operation_result(
    entries_by_id: &HashMap<String, Entry>,
    record: &OperationStartedRecord,
) -> Result<(), RecordLogCorruption> {
    match &record.intent {
        RunIntent::Run { initial_messages, .. } => {
            for target in initial_messages {
                validate_exact_provisioned_entry(entries_by_id, target)?;
            }
        }
        RunIntent::Compaction { result_entry_id, .. } => validate_result_entry(
            entries_by_id,
            result_entry_id,
            |entry| entry.type_name() == "compaction",
            "manual compaction",
        )?,
        RunIntent::Navigation {
            summary_entry_id, ..
        } => {
            if let Some(summary_entry_id) = summary_entry_id {
                validate_result_entry(
                    entries_by_id,
                    summary_entry_id,
                    |entry| entry.type_name() == "branch_summary",
                    "navigation summary",
                )?;
            }
        }
    }
    Ok(())
}

/// Validates a bounded lane recovery slice without reading or mutating
/// session state.
pub fn validate_record_log(input: &RecordLogSlice) -> Result<(), RecordLogCorruption> {
    if input.open_operations.len() > 1 {
        return Err(corrupt(
            "multiple_open_operations",
            format!("Lane {} has at least two open operations", input.lane),
        ));
    }

    let entries_by_id: HashMap<String, Entry> =
        input.entries.iter().map(|entry| (entry.id().to_string(), entry.clone())).collect();
    validate_deferred_handles(&input.entries)?;
    let mut starts: HashMap<String, OperationStartedRecord> = HashMap::new();
    let mut finished_at: HashMap<String, f64> = HashMap::new();
    let mut aborted_at: HashMap<String, f64> = HashMap::new();
    let mut queue_enqueues: HashMap<String, QueueEnqueuedRecord> = HashMap::new();
    let mut latest_attempt: HashMap<String, AttemptSeries> = HashMap::new();
    let mut tool_invocations: HashSet<String> = HashSet::new();
    let mut records = input.records.clone();
    records.sort_by(|left, right| left.seq().total_cmp(&right.seq()));

    for record in &records {
        if record.type_name() == "operation_started" {
            let LaneRecord::OperationStarted(started) = record else {
                unreachable!()
            };
            validate_operation_result(&entries_by_id, started)?;
            starts.insert(started.base.id.clone(), started.clone());
            continue;
        }

        if let Some(run_id) = has_run_id(record) {
            if !starts.contains_key(run_id) {
                return Err(corrupt(
                    "unknown_operation",
                    format!("Record {} references unknown operation {run_id}", record.id()),
                ));
            }
            if let Some(finish_seq) = finished_at.get(run_id) {
                if record.seq() > *finish_seq {
                    return Err(corrupt(
                        "record_after_finish",
                        format!("Record {} follows the finish of operation {run_id}", record.id()),
                    ));
                }
            }
        }

        match record {
            LaneRecord::OperationFinished(finished) => {
                finished_at.insert(finished.run_id.clone(), finished.base.seq);
            }
            LaneRecord::AbortRequested(aborted) => {
                aborted_at.insert(aborted.run_id.clone(), aborted.base.seq);
            }
            LaneRecord::StepAttempt(attempt) => {
                validate_attempt_reason(attempt)?;
                validate_attempt_sequence(attempt, latest_attempt.get(&attempt.run_id), &entries_by_id)?;
                validate_attempt_result(&entries_by_id, attempt)?;
                latest_attempt.insert(attempt.run_id.clone(), AttemptSeries {
                    record: attempt.clone(),
                });
            }
            LaneRecord::ToolStarted(tool_started) => {
                validate_tool_start(tool_started, &entries_by_id, &mut tool_invocations)?;
            }
            LaneRecord::QueueEnqueued(enqueued) => {
                if enqueued.queue != "nextRun" {
                    if let Some(aborted_seq) = aborted_at.get(&enqueued.run_id.clone().unwrap_or_default()) {
                        let _ = aborted_seq;
                    }
                    if let Some(run_id) = &enqueued.run_id {
                        if let Some(aborted_seq) = aborted_at.get(run_id) {
                            if enqueued.base.seq > *aborted_seq {
                                return Err(corrupt(
                                    "queue_after_abort",
                                    format!(
                                        "{} item {} was enqueued after abort",
                                        enqueued.queue, enqueued.target.id()
                                    ),
                                ));
                            }
                        }
                    }
                }
                queue_enqueues.insert(enqueued.target.id().to_string(), enqueued.clone());
                validate_exact_provisioned_entry(&entries_by_id, &enqueued.target)?;
            }
            LaneRecord::QueueCancelled(cancelled) => {
                let enqueue = queue_enqueues.get(&cancelled.entry_id);
                let valid = enqueue.is_some_and(|enqueue| {
                    enqueue.base.seq < cancelled.base.seq
                        && enqueue.run_id == cancelled.run_id
                        && !entries_by_id.contains_key(&cancelled.entry_id)
                });
                if !valid {
                    return Err(corrupt(
                        "invalid_queue_cancellation",
                        format!("Queue cancellation {} has no pending matching enqueue", cancelled.base.id),
                    ));
                }
            }
            LaneRecord::WriteDeferred(deferred) => {
                validate_exact_provisioned_entry(&entries_by_id, &deferred.target)?;
            }
            LaneRecord::Usage(_) => {}
            LaneRecord::OperationStarted(_) => unreachable!("handled above"),
        }
    }
    Ok(())
}

fn by_sequence<T: Clone + HasSeq>(values: &[T]) -> Vec<T> {
    let mut indices: Vec<usize> = (0..values.len()).collect();
    indices.sort_by(|a, b| values[*a].seq().total_cmp(&values[*b].seq()));
    indices.into_iter().map(|i| values[i].clone()).collect()
}

trait HasSeq {
    fn seq(&self) -> f64;
}

impl HasSeq for Entry {
    fn seq(&self) -> f64 {
        Entry::seq(self)
    }
}

impl HasSeq for LaneRecord {
    fn seq(&self) -> f64 {
        LaneRecord::seq(self)
    }
}

fn derive_effective_configuration(input: &LaneReductionInput) -> EffectiveLaneConfiguration {
    let mut configuration = input.defaults.clone();
    let mut entries_by_id: HashMap<String, Entry> = HashMap::new();
    for entry in input.configuration_entries.iter().chain(input.own_entries.iter()) {
        entries_by_id.insert(entry.id().to_string(), entry.clone());
    }

    let mut ordered: Vec<Entry> = entries_by_id.values().cloned().collect();
    ordered.sort_by(|a, b| a.seq().total_cmp(&b.seq()));
    for entry in ordered {
        match &entry {
            Entry::ModelChange(model_change) => {
                configuration.model = (model_change.provider.clone(), model_change.model_id.clone());
            }
            Entry::ThinkingLevelChange(thinking_level) => {
                configuration.thinking_level = thinking_level.thinking_level.clone();
            }
            Entry::ActiveToolsChange(active_tools) => {
                configuration.active_tool_names = active_tools.active_tool_names.clone();
            }
            Entry::Message(message) => {
                if let AgentMessage::Llm(pi_ai::types::Message::Assistant(assistant)) = &message.message {
                    configuration.model = (assistant.provider.clone(), assistant.model.clone());
                }
            }
            _ => {}
        }
    }
    configuration
}

fn derive_newest_own(entry: Option<&Entry>) -> Option<NewestOwnState> {
    let entry = entry?;
    if entry.type_name() != "message" {
        return Some(NewestOwnState {
            entry_id: entry.id().to_string(),
            type_: entry.type_name().to_string(),
            role: None,
            stop_reason: None,
        });
    }
    let Entry::Message(message) = entry else {
        unreachable!()
    };
    match &message.message {
        AgentMessage::Llm(pi_ai::types::Message::Assistant(assistant)) => Some(NewestOwnState {
            entry_id: entry.id().to_string(),
            type_: entry.type_name().to_string(),
            role: Some("assistant".to_string()),
            stop_reason: Some(assistant.stop_reason),
        }),
        AgentMessage::Llm(pi_ai::types::Message::User(_)) => Some(NewestOwnState {
            entry_id: entry.id().to_string(),
            type_: entry.type_name().to_string(),
            role: Some("user".to_string()),
            stop_reason: None,
        }),
        AgentMessage::Llm(pi_ai::types::Message::ToolResult(_)) => Some(NewestOwnState {
            entry_id: entry.id().to_string(),
            type_: entry.type_name().to_string(),
            role: Some("toolResult".to_string()),
            stop_reason: None,
        }),
        AgentMessage::Custom(_) => None,
    }
}

fn derive_tool_batch(
    operation_id: &str,
    records: &[LaneRecord],
    own_entries: &[Entry],
    entries_by_id: &HashMap<String, Entry>,
    deferred_write_ids: &HashSet<String>,
) -> Option<ToolBatchState> {
    let assistant_entry = own_entries.iter().rev().find(|entry| {
        if let Entry::Message(message) = entry {
            if let AgentMessage::Llm(pi_ai::types::Message::Assistant(assistant)) = &message.message {
                return assistant
                    .content
                    .iter()
                    .any(|content| matches!(content, pi_ai::types::Content::ToolCall(_)));
            }
        }
        false
    });
    let assistant_entry = assistant_entry?;
    let Entry::Message(assistant) = assistant_entry else {
        return None;
    };
    let AgentMessage::Llm(pi_ai::types::Message::Assistant(assistant_message)) = &assistant.message else {
        return None;
    };

    let tool_calls: Vec<AgentToolCall> = assistant_message
        .content
        .iter()
        .filter_map(|content| match content {
            pi_ai::types::Content::ToolCall(tool_call) => Some(tool_call.clone()),
            _ => None,
        })
        .collect();
    let mut starts: HashMap<u64, ToolStartedRecord> = HashMap::new();
    for record in records {
        if let LaneRecord::ToolStarted(started) = record {
            if started.run_id == operation_id && started.assistant_entry_id == assistant_entry.id() {
                starts.insert(started.tool_index as u64, started.clone());
            }
        }
    }

    let calls: Vec<ToolBatchCallState> = tool_calls
        .iter()
        .enumerate()
        .map(|(tool_index, tool_call)| {
            let started = starts.get(&(tool_index as u64));
            let started_result = started
                .and_then(|started| entries_by_id.get(&started.result_entry_id));
            let blocked_result = own_entries.iter().find(|entry| {
                entry.seq() > assistant_entry.seq()
                    && !deferred_write_ids.contains(entry.id())
                    && matches!(entry, Entry::Message(message) if matches!(
                        &message.message,
                        AgentMessage::Llm(pi_ai::types::Message::ToolResult(tool_result))
                            if tool_result.tool_call_id == tool_call.id
                    ))
            });
            let result = started_result.or(blocked_result);
            let terminate = result.and_then(|entry| match entry {
                Entry::Message(message) => match &message.message {
                    AgentMessage::Llm(pi_ai::types::Message::ToolResult(tool_result)) => {
                        tool_result_details_terminate(tool_result)
                    }
                    _ => None,
                },
                _ => None,
            });
            ToolBatchCallState {
                tool_index: tool_index as f64,
                tool_call: tool_call.clone(),
                started: started.cloned(),
                result_exists: result.is_some(),
                terminate,
            }
        })
        .collect();

    Some(ToolBatchState {
        assistant_entry_id: assistant_entry.id().to_string(),
        truncated: assistant_message.stop_reason == StopReason::Length,
        unresolved: calls.iter().any(|call| !call.result_exists),
        calls,
    })
}

fn tool_result_details_terminate(_tool_result: &pi_ai::types::ToolResultMessage) -> Option<bool> {
    // The JS tool-result terminate flag lives on the result details in the
    // harness layer; the message model carries isError only. Default false.
    None
}

/// Purely reconstructs one lane's orchestration state from its bounded
/// recovery inputs.
pub fn reduce_lane_state(input: &LaneReductionInput) -> Result<LaneReductionResult, RecordLogCorruption> {
    validate_record_log(&RecordLogSlice {
        lane: input.lane.clone(),
        open_operations: input.open_operations.clone(),
        records: input.records.clone(),
        entries: input.entries.clone(),
    })?;

    let records = by_sequence(&input.records);
    let own_entries = by_sequence(&input.own_entries);
    let mut entries_by_id: HashMap<String, Entry> = HashMap::new();
    for entry in input.entries.iter().chain(own_entries.iter()) {
        entries_by_id.insert(entry.id().to_string(), entry.clone());
    }
    let cancelled_queue_ids: HashSet<String> = records
        .iter()
        .filter(|record| record.type_name() == "queue_cancelled")
        .map(|record| record.id().to_string())
        .collect();
    let pending_queue_records: Vec<QueueEnqueuedRecord> = records
        .iter()
        .filter_map(|record| match record {
            LaneRecord::QueueEnqueued(enqueued) => {
                if !entries_by_id.contains_key(enqueued.target.id())
                    && !cancelled_queue_ids.contains(enqueued.target.id())
                {
                    Some(enqueued.clone())
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    let started = input.open_operations.first();
    let captured_initial_message_ids: HashSet<String> = started
        .and_then(|started| match &started.intent {
            RunIntent::Run { initial_messages, .. } => {
                Some(initial_messages.iter().map(|target| target.id().to_string()).collect())
            }
            _ => None,
        })
        .unwrap_or_default();
    let pending_next_run: Vec<ProvisionedEntry> = pending_queue_records
        .iter()
        .filter(|record| record.queue == "nextRun" && !captured_initial_message_ids.contains(record.target.id()))
        .map(|record| record.target.clone())
        .collect();
    let effective_configuration = derive_effective_configuration(input);

    let Some(started) = started else {
        return Ok(LaneReductionResult {
            lane_state: LaneState {
                lane: input.lane.clone(),
                leaf_id: input.leaf_id.clone(),
                operation: None,
                pending_next_run,
            },
            effective_configuration,
            terminal_failure: None,
        });
    };

    let operation_records: Vec<LaneRecord> = records
        .iter()
        .filter(|record| match record {
            LaneRecord::OperationStarted(started_record) => started_record.base.id == started.base.id,
            other => other.run_id() == Some(&started.base.id),
        })
        .cloned()
        .collect();
    let aborting = operation_records
        .iter()
        .any(|record| record.type_name() == "abort_requested");
    let pending_steer: Vec<ProvisionedEntry> = if aborting {
        Vec::new()
    } else {
        pending_queue_records
            .iter()
            .filter(|record| record.queue == "steer" && record.run_id.as_deref() == Some(&started.base.id))
            .map(|record| record.target.clone())
            .collect()
    };
    let pending_follow_up: Vec<ProvisionedEntry> = if aborting {
        Vec::new()
    } else {
        pending_queue_records
            .iter()
            .filter(|record| record.queue == "followUp" && record.run_id.as_deref() == Some(&started.base.id))
            .map(|record| record.target.clone())
            .collect()
    };
    let pending_writes: Vec<ProvisionedEntry> = operation_records
        .iter()
        .filter_map(|record| match record {
            LaneRecord::WriteDeferred(deferred) => {
                if !entries_by_id.contains_key(deferred.target.id()) {
                    Some(deferred.target.clone())
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    let missing_initial_messages: Vec<ProvisionedEntry> = match &started.intent {
        RunIntent::Run { initial_messages, .. } => initial_messages
            .iter()
            .filter(|target| !entries_by_id.contains_key(target.id()))
            .cloned()
            .collect(),
        _ => Vec::new(),
    };

    let newest_attempt = operation_records
        .iter()
        .filter(|record| record.type_name() == "step_attempt")
        .last()
        .and_then(|record| match record {
            LaneRecord::StepAttempt(attempt) => Some(attempt),
            _ => None,
        });
    let step = newest_attempt
        .filter(|attempt| !entries_by_id.contains_key(&attempt.result_entry_id))
        .map(|attempt| LaneStepState {
            kind: attempt.step.clone(),
            attempts: attempt.attempt,
            result_entry_id: attempt.result_entry_id.clone(),
            compaction_reason: attempt.compaction_reason.clone(),
        });

    let mut consumed_input_ids: HashSet<String> = HashSet::new();
    if let RunIntent::Run { initial_messages, .. } = &started.intent {
        for target in initial_messages {
            consumed_input_ids.insert(target.id().to_string());
        }
    }
    for record in &operation_records {
        if let LaneRecord::QueueEnqueued(enqueued) = record {
            if enqueued.queue != "nextRun" {
                consumed_input_ids.insert(enqueued.target.id().to_string());
            }
        }
    }
    let mut newest_consumed_input_sequence = f64::NEG_INFINITY;
    for id in &consumed_input_ids {
        if let Some(entry) = entries_by_id.get(id) {
            if entry.type_name() == "message" {
                newest_consumed_input_sequence = newest_consumed_input_sequence.max(entry.seq());
            }
        }
    }
    let overflow_recovery_used = operation_records.iter().any(|record| {
        matches!(record, LaneRecord::StepAttempt(attempt)
            if attempt.step == "compaction"
                && attempt.compaction_reason.as_deref() == Some("overflow")
                && attempt.base.seq > newest_consumed_input_sequence)
    });

    let newest_own_entry = own_entries.last();
    let newest_own = derive_newest_own(newest_own_entry);
    let deferred = newest_own_entry.and_then(|entry| match entry {
        Entry::Message(message) => match &message.message {
            AgentMessage::Llm(pi_ai::types::Message::Assistant(assistant))
                if assistant.stop_reason == StopReason::Deferred =>
            {
                assistant.deferred.clone()
            }
            _ => None,
        },
        _ => None,
    });
    let mut targets = LaneTargets::default();
    match &started.intent {
        RunIntent::Compaction { result_entry_id, .. } => {
            targets.result = Some(entries_by_id.contains_key(result_entry_id));
        }
        RunIntent::Navigation {
            summary_entry_id, ..
        } => {
            if let Some(summary_entry_id) = summary_entry_id {
                targets.summary = Some(entries_by_id.contains_key(summary_entry_id));
            }
        }
        RunIntent::Run { .. } => {}
    }

    let deferred_write_ids: HashSet<String> = operation_records
        .iter()
        .filter(|record| record.type_name() == "write_deferred")
        .map(|record| match record {
            LaneRecord::WriteDeferred(deferred) => deferred.target.id().to_string(),
            _ => unreachable!(),
        })
        .collect();
    let mut terminal_failure: Option<TerminalFailureState> = None;
    if let Some(entry @ Entry::Message(message)) = newest_own_entry {
        if let AgentMessage::Llm(pi_ai::types::Message::Assistant(assistant)) = &message.message {
            if assistant.stop_reason == StopReason::Error && !deferred_write_ids.contains(entry.id()) {
                let produced_by_step = operation_records.iter().any(|record| {
                    matches!(record, LaneRecord::StepAttempt(attempt)
                        if attempt.result_entry_id == entry.id())
                });
                let previous_own_entry = own_entries.iter().nth_back(1);
                let produced_by_deferred_fetch = operation_records.iter().any(|record| {
                    matches!(record, LaneRecord::Usage(usage)
                        if usage.cause == "deferred_fetch"
                            && usage.entry_id.as_deref() == Some(entry.id()))
                }) || previous_own_entry.is_some_and(|previous| {
                    matches!(previous, Entry::Message(previous_message)
                        if matches!(&previous_message.message,
                            AgentMessage::Llm(pi_ai::types::Message::Assistant(previous_assistant))
                                if previous_assistant.stop_reason == StopReason::Deferred))
                });
                if produced_by_step || produced_by_deferred_fetch {
                    terminal_failure = Some(TerminalFailureState {
                        entry_id: entry.id().to_string(),
                        source: if produced_by_step { "step" } else { "deferred_fetch" }.to_string(),
                        message: assistant.clone(),
                    });
                }
            }
        }
    }

    Ok(LaneReductionResult {
        lane_state: LaneState {
            lane: input.lane.clone(),
            leaf_id: input.leaf_id.clone(),
            operation: Some(LaneOperationState {
                id: started.base.id.clone(),
                kind: started.intent_kind().to_string(),
                intent: started.intent.clone(),
                aborting,
                step,
                tool_batch: derive_tool_batch(
                    &started.base.id,
                    &operation_records,
                    &own_entries,
                    &entries_by_id,
                    &deferred_write_ids,
                ),
                missing_initial_messages,
                pending_steer,
                pending_follow_up,
                pending_writes,
                deferred,
                overflow_recovery_used,
                newest_own,
                targets,
            }),
            pending_next_run,
        },
        effective_configuration,
        terminal_failure,
    })
}

