//! SQLite session repository, port of
//! `packages/session-backends/sqlite-node/src/sqlite/repo.ts`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pi_agent_core::harness::session_types::{
    Entry, EntryBase, EntryOrder, EntryQuery, LanePointer, LaneRecord, LogItem, LogOptions,
    OperationStartedRecord, RecordQuery, SessionError, SessionMetadata, SessionStats,
};
use pi_protocol::cbor::Value;

use crate::database::{RusqliteDatabase, SqliteDatabase};
use crate::migrations::apply_migrations;
use crate::storage::branch_cache::{
    append_entry_to_branch_cache, query_cached_branch_rows, read_cached_branch, rebuild_branch_cache,
    CachedBranchQuery, CachedBranchEntryRow,
};
use crate::storage::branch_tips::delete_branch_tips;
use crate::storage::entries::{
    delete_entry_rows, id_exists_in_entries, insert_entry_row, read_entry_row, read_entry_rows, EntryRow,
    NewEntryRow, ReadEntryRowsOptions,
};
use crate::storage::facts::{append_fact, delete_fact_rows, read_fact_rows, read_latest_fact};
use crate::storage::lanes::{
    create_initial_lane, create_lane as insert_lane, delete_lane_rows, finish_lane_operation, move_lane,
    read_lane, read_lane_head, read_lane_move_rows, read_lanes, set_lane_leaf, start_lane_operation,
};
use crate::storage::records::{
    append_record_row, delete_record_rows, id_exists_in_records, read_open_operation_rows, read_record_rows,
    NewRecordRow, ReadRecordRowsOptions,
};
use crate::storage::session_sequences::{
    advance_sequence, create_sequence, delete_sequence, get_next_sequence,
};
use crate::storage::session_stats::{
    add_usage_to_stats, create_stats, delete_stats, increment_message_count, read_stats,
};
use crate::storage::sessions::{
    decode_session_metadata, delete_session_row, insert_session_row, read_session_row, read_session_rows,
    session_exists, NewSessionRow, SqliteSessionMetadata,
};
use crate::storage::writer_leases::{
    acquire_writer_lease, delete_writer_lease, release_writer_lease, renew_writer_lease, WriterLease,
};

#[derive(Clone, Debug)]
pub struct SqliteWriterLeaseOptions {
    pub ttl_ms: f64,
    pub heartbeat_interval_ms: f64,
}

#[derive(Clone, Debug)]
pub struct ResolvedWriterLeaseOptions {
    pub ttl_ms: f64,
    pub heartbeat_interval_ms: f64,
}

pub fn resolve_writer_lease_options(options: Option<&SqliteWriterLeaseOptions>) -> Result<ResolvedWriterLeaseOptions, String> {
    let ttl_ms = options.map(|options| options.ttl_ms).unwrap_or(30_000.0);
    let heartbeat_interval_ms = options
        .map(|options| options.heartbeat_interval_ms)
        .unwrap_or(10_000.0);
    if !ttl_ms.is_finite() || ttl_ms <= 0.0 {
        return Err("writerLease.ttlMs must be positive".to_string());
    }
    if !heartbeat_interval_ms.is_finite()
        || heartbeat_interval_ms <= 0.0
        || heartbeat_interval_ms >= ttl_ms
    {
        return Err("writerLease.heartbeatIntervalMs must be positive and less than ttlMs".to_string());
    }
    Ok(ResolvedWriterLeaseOptions {
        ttl_ms,
        heartbeat_interval_ms,
    })
}

pub struct SqliteSessionRepositoryOptions {
    pub database_path: String,
    pub writer_lease: Option<SqliteWriterLeaseOptions>,
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as f64
}

fn uuid_v7() -> String {
    pi_ai::utils::uuid::uuidv7()
}

// ---------------------------------------------------------------------------
// Entry payload serialization
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

fn bool(value: bool) -> Value {
    Value::Bool(value)
}

fn null() -> Value {
    Value::Null
}

/// Serialize an entry to its payload object (JS `entryPayload`: storage
/// fields stripped).
pub fn entry_payload(entry: &Entry) -> Value {
    match entry {
        Entry::Message(entry) => {
            let message = crate::util::agent_message_to_json(&entry.message);
            let mut entries = vec![kv("message", message)];
            if entry.terminate == Some(true) {
                entries.push(kv("terminate", bool(true)));
            }
            Value::Map(entries)
        }
        Entry::ModelChange(entry) => Value::Map(vec![
            kv("provider", str(&entry.provider)),
            kv("modelId", str(&entry.model_id)),
        ]),
        Entry::ThinkingLevelChange(entry) => Value::Map(vec![kv("thinkingLevel", str(&entry.thinking_level))]),
        Entry::ActiveToolsChange(entry) => Value::Map(vec![kv(
            "activeToolNames",
            Value::Array(entry.active_tool_names.iter().map(|name| str(name)).collect()),
        )]),
        Entry::Compaction(entry) => {
            let mut entries = vec![
                kv("summary", str(&entry.summary)),
                kv("retainedTail", crate::util::agent_messages_to_json(&entry.retained_tail)),
                kv("tokensBefore", num(entry.tokens_before)),
            ];
            if let Some(details) = &entry.details {
                entries.push(kv("details", details.clone()));
            }
            if let Some(usage) = &entry.usage {
                entries.push(kv("usage", crate::util::usage_to_json(usage)));
            }
            Value::Map(entries)
        }
        Entry::BranchSummary(entry) => {
            let mut entries = vec![kv("fromId", str(&entry.from_id)), kv("summary", str(&entry.summary))];
            if let Some(details) = &entry.details {
                entries.push(kv("details", details.clone()));
            }
            if let Some(usage) = &entry.usage {
                entries.push(kv("usage", crate::util::usage_to_json(usage)));
            }
            Value::Map(entries)
        }
        Entry::Custom(entry) => {
            let mut entries = vec![kv("customType", str(&entry.custom_type))];
            if let Some(data) = &entry.data {
                entries.push(kv("data", data.clone()));
            }
            Value::Map(entries)
        }
    }
}

fn get_str_value<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a str> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_str())
}

fn get_number(entries: &[(String, Value)], key: &str) -> Option<f64> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_number())
}

fn get_bool(entries: &[(String, Value)], key: &str) -> Option<bool> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_bool())
}

fn get_map<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a [(String, Value)]> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_map())
}

fn get_array<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a [Value]> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_array())
}

/// Decode an entry row into an Entry (JS `decodeEntry`).
pub fn decode_entry(row: &EntryRow) -> Result<Entry, SessionError> {
    let payload: Value = pi_ai::utils::json::parse_json_with_repair(&row.payload)
        .map_err(|_| invalid_entry(row))?;
    let entries = match payload {
        Value::Map(entries) => entries,
        _ => return Err(invalid_entry(row)),
    };
    let base = EntryBase {
        type_: row.type_.clone(),
        id: row.id.clone(),
        seq: row.seq,
        parent_id: row.parent_id.clone(),
        timestamp: row.timestamp,
    };
    let result = match row.type_.as_str() {
        "message" => {
            let message = get_map(&entries, "message").ok_or_else(|| invalid_entry(row))?;
            Entry::Message(pi_agent_core::harness::session_types::MessageEntry {
                base,
                message: crate::util::json_to_agent_message(Value::Map(message.to_vec())),
                terminate: get_bool(&entries, "terminate"),
            })
        }
        "model_change" => {
            let provider = get_str_value(&entries, "provider").ok_or_else(|| invalid_entry(row))?;
            let model_id = get_str_value(&entries, "modelId").ok_or_else(|| invalid_entry(row))?;
            Entry::ModelChange(pi_agent_core::harness::session_types::ModelChangeEntry {
                base,
                provider: provider.to_string(),
                model_id: model_id.to_string(),
            })
        }
        "thinking_level_change" => {
            let thinking_level = get_str_value(&entries, "thinkingLevel").ok_or_else(|| invalid_entry(row))?;
            Entry::ThinkingLevelChange(pi_agent_core::harness::session_types::ThinkingLevelEntry {
                base,
                thinking_level: thinking_level.to_string(),
            })
        }
        "active_tools_change" => {
            let names = get_array(&entries, "activeToolNames").ok_or_else(|| invalid_entry(row))?;
            let mut active_tool_names = Vec::new();
            for name in names {
                match name {
                    Value::String(name) => active_tool_names.push(name.clone()),
                    _ => return Err(invalid_entry(row)),
                }
            }
            Entry::ActiveToolsChange(pi_agent_core::harness::session_types::ActiveToolsEntry {
                base,
                active_tool_names,
            })
        }
        "compaction" => {
            let summary = get_str_value(&entries, "summary").ok_or_else(|| invalid_entry(row))?;
            let retained_tail = get_array(&entries, "retainedTail").ok_or_else(|| invalid_entry(row))?;
            let tokens_before = get_number(&entries, "tokensBefore").ok_or_else(|| invalid_entry(row))?;
            Entry::Compaction(pi_agent_core::harness::session_types::CompactionEntry {
                base,
                summary: summary.to_string(),
                retained_tail: retained_tail
                    .iter()
                    .map(|value| crate::util::json_to_agent_message(value.clone()))
                    .collect(),
                tokens_before,
                details: get_map(&entries, "details").map(|map| Value::Map(map.to_vec())),
                usage: get_map(&entries, "usage").map(crate::util::json_to_usage),
            })
        }
        "branch_summary" => {
            let from_id = get_str_value(&entries, "fromId").ok_or_else(|| invalid_entry(row))?;
            let summary = get_str_value(&entries, "summary").ok_or_else(|| invalid_entry(row))?;
            Entry::BranchSummary(pi_agent_core::harness::session_types::BranchSummaryEntry {
                base,
                from_id: from_id.to_string(),
                summary: summary.to_string(),
                details: get_map(&entries, "details").map(|map| Value::Map(map.to_vec())),
                usage: get_map(&entries, "usage").map(crate::util::json_to_usage),
            })
        }
        "custom" => {
            let custom_type = get_str_value(&entries, "customType").ok_or_else(|| invalid_entry(row))?;
            Entry::Custom(pi_agent_core::harness::session_types::CustomEntry {
                base,
                custom_type: custom_type.to_string(),
                data: get_map(&entries, "data").map(|map| Value::Map(map.to_vec())),
            })
        }
        _ => return Err(invalid_entry(row)),
    };
    Ok(result)
}

fn invalid_entry(row: &EntryRow) -> SessionError {
    SessionError::new(
        "invalid_entry",
        format!("Invalid SQLite session entry {}: failed to decode entry {}", row.id, row.id),
    )
}

fn record_run_id(record: &LaneRecord) -> Option<String> {
    match record {
        LaneRecord::OperationStarted(record) => Some(record.base.id.clone()),
        LaneRecord::AbortRequested(record) => Some(record.run_id.clone()),
        LaneRecord::OperationFinished(record) => Some(record.run_id.clone()),
        LaneRecord::StepAttempt(record) => Some(record.run_id.clone()),
        LaneRecord::ToolStarted(record) => Some(record.run_id.clone()),
        LaneRecord::QueueEnqueued(record) => record.run_id.clone(),
        LaneRecord::QueueCancelled(record) => record.run_id.clone(),
        LaneRecord::WriteDeferred(record) => Some(record.run_id.clone()),
        LaneRecord::Usage(record) => record.run_id.clone(),
    }
}

fn record_op_kind(record: &LaneRecord) -> Option<String> {
    match record {
        LaneRecord::OperationStarted(record) => Some(match &record.intent {
            pi_agent_core::harness::session_types::RunIntent::Run { .. } => "run".to_string(),
            pi_agent_core::harness::session_types::RunIntent::Compaction { .. } => "compaction".to_string(),
            pi_agent_core::harness::session_types::RunIntent::Navigation { .. } => "navigation".to_string(),
        }),
        _ => None,
    }
}

fn record_type(record: &LaneRecord) -> &'static str {
    match record {
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

fn record_lane(record: &LaneRecord) -> &str {
    match record {
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

fn record_id(record: &LaneRecord) -> &str {
    match record {
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

/// Serialize a record to its payload JSON (JS stores the full record minus
/// storage-assigned seq/timestamp; the Rust version stores the full record
/// including base and re-applies seq/timestamp on decode).
pub fn record_to_payload(record: &LaneRecord) -> String {
    // The record is stored as its serialized JSON; seq/timestamp are
    // reapplied on read (decode_record overrides them), matching the JS
    // behavior where storage assigns them.
    crate::util::json_stringify(&crate::util::lane_record_to_json(record))
}

pub fn decode_record(seq: f64, timestamp: f64, payload: &str) -> Result<LaneRecord, SessionError> {
    let value: Value = pi_ai::utils::json::parse_json_with_repair(payload)
        .map_err(|_| SessionError::new("storage", format!("Invalid SQLite session record at sequence {seq}: failed to decode payload")))?;
    let mut record = crate::util::json_to_lane_record(value)
        .map_err(|message| SessionError::new("storage", format!("Invalid SQLite session record at sequence {seq}: {message}")))?;
    set_record_seq_timestamp(&mut record, seq, timestamp);
    Ok(record)
}

fn set_record_seq_timestamp(record: &mut LaneRecord, seq: f64, timestamp: f64) {
    let base = match record {
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

// ---------------------------------------------------------------------------
// Storage (writer-lease guarded, serialized writes)
// ---------------------------------------------------------------------------

pub struct SqliteSessionStorage {
    db: Arc<RusqliteDatabase>,
    session_id: String,
    metadata: Mutex<SqliteSessionMetadata>,
    lease: Mutex<WriterLease>,
    lease_options: ResolvedWriterLeaseOptions,
    write_lock: Mutex<()>,
    lease_error: Mutex<Option<SessionError>>,
    closing: AtomicBool,
    heartbeat_stop: Arc<AtomicBool>,
}

impl SqliteSessionStorage {
    fn new(
        db: Arc<RusqliteDatabase>,
        metadata: SqliteSessionMetadata,
        lease: WriterLease,
        lease_options: ResolvedWriterLeaseOptions,
    ) -> Arc<Self> {
        let storage = Arc::new(Self {
            db,
            session_id: metadata.id.clone(),
            metadata: Mutex::new(metadata),
            lease: Mutex::new(lease),
            lease_options,
            write_lock: Mutex::new(()),
            lease_error: Mutex::new(None),
            closing: AtomicBool::new(false),
            heartbeat_stop: Arc::new(AtomicBool::new(false)),
        });
        let heartbeat = storage.clone();
        std::thread::spawn(move || {
            heartbeat.heartbeat_loop();
        });
        storage
    }

    fn heartbeat_loop(&self) {
        while !self.heartbeat_stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(self.lease_options.heartbeat_interval_ms as u64));
            if self.heartbeat_stop.load(Ordering::SeqCst) {
                return;
            }
            let _guard = self.write_lock.lock().unwrap();
            if self.closing.load(Ordering::SeqCst) || self.lease_error.lock().unwrap().is_some() {
                return;
            }
            let mut lease = self.lease.lock().unwrap();
            let now = now_ms();
            let renewed = renew_writer_lease(
                self.db.as_ref(),
                &self.session_id,
                &mut lease,
                now,
                now + self.lease_options.ttl_ms,
            );
            if !renewed.unwrap_or(false) {
                *self.lease_error.lock().unwrap() = Some(lost_writer_error(&self.session_id));
                return;
            }
        }
    }

    /// Release the writer lease (JS `release`).
    pub fn release(&self) -> Result<(), SessionError> {
        self.closing.store(true, Ordering::SeqCst);
        self.heartbeat_stop.store(true, Ordering::SeqCst);
        let _guard = self.write_lock.lock().unwrap();
        let lease = self.lease.lock().unwrap().clone();
        release_writer_lease(self.db.as_ref(), &self.session_id, &lease)
            .map_err(|message| SessionError::new("storage", message))
    }

    fn enqueue_write<T>(
        &self,
        operation: impl FnOnce(&dyn SqliteDatabase) -> Result<T, SessionError>,
    ) -> Result<T, SessionError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(SessionError::new(
                "storage",
                format!("SQLite session {} is closed", self.session_id),
            ));
        }
        let _guard = self.write_lock.lock().unwrap();
        if let Some(error) = self.lease_error.lock().unwrap().clone() {
            return Err(error);
        }
        let mut lease = self.lease.lock().unwrap();
        let now = now_ms();
        let renewed = renew_writer_lease(
            self.db.as_ref(),
            &self.session_id,
            &mut lease,
            now,
            now + self.lease_options.ttl_ms,
        )
        .map_err(|message| SessionError::new("storage", message))?;
        if !renewed {
            let error = lost_writer_error(&self.session_id);
            *self.lease_error.lock().unwrap() = Some(error.clone());
            return Err(error);
        }
        let db: &dyn SqliteDatabase = self.db.as_ref();
        operation(db)
    }
}

fn active_writer_error(session_id: &str) -> SessionError {
    SessionError::new(
        "storage",
        format!("SQLite session {session_id} already has an active writer"),
    )
}

fn lost_writer_error(session_id: &str) -> SessionError {
    SessionError::new("storage", format!("SQLite session {session_id} writer lease was lost"))
}

impl pi_agent_core::harness::session_types::SessionStorage for SqliteSessionStorage {
    fn get_metadata(&self) -> Result<SessionMetadata, SessionError> {
        let row = read_session_row(self.db.as_ref(), &self.session_id)
            .map_err(|message| SessionError::new("storage", message))?
            .ok_or_else(|| SessionError::new("not_found", format!("Session not found: {}", self.session_id)))?;
        let decoded = decode_session_metadata(&row, "").map_err(|error| error)?;
        Ok(SessionMetadata {
            id: decoded.id,
            created_at: decoded.created_at,
            parent_session_id: decoded.parent_session_id,
        })
    }

    fn get_lanes(&self) -> Result<Vec<LanePointer>, SessionError> {
        let rows = read_lanes(self.db.as_ref(), &self.session_id)?;
        Ok(rows
            .iter()
            .map(|row| LanePointer {
                lane: row.lane.clone(),
                leaf_id: row.leaf_id.clone(),
            })
            .collect())
    }

    fn create_lane(&self, lane: &str, at: Option<&str>) -> Result<(), SessionError> {
        self.enqueue_write(|db| {
            if read_lane(db, &self.session_id, lane).map_err(|message| SessionError::new("storage", message))?.is_some()
            {
                return Err(SessionError::new("already_exists", format!("Lane already exists: {lane}")));
            }
            if let Some(at) = at {
                if read_entry_row(db, &self.session_id, at)
                    .map_err(|message| SessionError::new("storage", message))?
                    .is_none()
                {
                    return Err(SessionError::new("not_found", format!("Entry not found: {at}")));
                }
            }
            let seq = get_next_sequence(db, &self.session_id).map_err(|error| error)?;
            insert_lane(db, &self.session_id, seq, lane, at).map_err(|message| SessionError::new("storage", message))?;
            advance_sequence(db, &self.session_id, seq).map_err(|message| SessionError::new("storage", message))?;
            Ok(())
        })
    }

    fn move_lane(&self, lane: &str, to: Option<&str>) -> Result<(), SessionError> {
        self.enqueue_write(|db| {
            if read_lane(db, &self.session_id, lane)
                .map_err(|message| SessionError::new("storage", message))?
                .is_none()
            {
                return Err(SessionError::new("invalid_lane", format!("Lane not found: {lane}")));
            }
            if let Some(to) = to {
                if read_entry_row(db, &self.session_id, to)
                    .map_err(|message| SessionError::new("storage", message))?
                    .is_none()
                {
                    return Err(SessionError::new("not_found", format!("Entry not found: {to}")));
                }
            }
            let seq = get_next_sequence(db, &self.session_id).map_err(|error| error)?;
            move_lane(db, &self.session_id, seq, lane, to).map_err(|error| error)?;
            advance_sequence(db, &self.session_id, seq).map_err(|message| SessionError::new("storage", message))?;
            Ok(())
        })
    }

    fn append_entry(&self, entry: Entry, lane: &str) -> Result<Entry, SessionError> {
        self.enqueue_write(|db| {
            let parent_id = read_lane_head(db, &self.session_id, lane)?;
            if id_exists_in_entries(db, &self.session_id, entry.id()).map_err(|message| SessionError::new("storage", message))?
                || id_exists_in_records(db, &self.session_id, entry.id()).map_err(|message| SessionError::new("storage", message))?
            {
                return Err(SessionError::new("already_exists", format!("ID already exists: {}", entry.id())));
            }
            let seq = get_next_sequence(db, &self.session_id).map_err(|error| error)?;
            let timestamp = now_ms();
            let committed = entry.with_storage_fields(parent_id.clone(), seq, timestamp);
            insert_entry_row(
                db,
                &self.session_id,
                &NewEntryRow {
                    seq,
                    id: committed.id().to_string(),
                    parent_id: parent_id.clone(),
                    type_: committed.type_name().to_string(),
                    timestamp,
                    payload: crate::util::json_stringify(&entry_payload(&committed)),
                },
            )
            .map_err(|message| SessionError::new("storage", message))?;
            set_lane_leaf(db, &self.session_id, lane, Some(committed.id())).map_err(|error| error)?;
            append_entry_to_branch_cache(
                db,
                &self.session_id,
                committed.id(),
                seq,
                committed.type_name(),
                custom_type_of(&committed).as_deref(),
                parent_id.as_deref().or(None),
            )
            .map_err(|error| error)?;
            if committed.type_name() == "message" {
                increment_message_count(db, &self.session_id).map_err(|error| error)?;
            }
            advance_sequence(db, &self.session_id, seq).map_err(|message| SessionError::new("storage", message))?;
            Ok(committed)
        })
    }

    fn append_record(&self, record: LaneRecord) -> Result<LaneRecord, SessionError> {
        self.enqueue_write(|db| {
            let lane = record_lane(&record).to_string();
            if read_lane(db, &self.session_id, &lane)
                .map_err(|message| SessionError::new("storage", message))?
                .is_none()
            {
                return Err(SessionError::new("invalid_lane", format!("Lane not found: {lane}")));
            }
            let id = record_id(&record).to_string();
            if id_exists_in_entries(db, &self.session_id, &id).map_err(|message| SessionError::new("storage", message))?
                || id_exists_in_records(db, &self.session_id, &id).map_err(|message| SessionError::new("storage", message))?
            {
                return Err(SessionError::new("already_exists", format!("ID already exists: {id}")));
            }
            let seq = get_next_sequence(db, &self.session_id).map_err(|error| error)?;
            let timestamp = now_ms();
            let mut committed = record.clone();
            set_record_seq_timestamp(&mut committed, seq, timestamp);
            if matches!(record, LaneRecord::OperationStarted(_)) {
                start_lane_operation(db, &self.session_id, &lane, &id).map_err(|error| error)?;
            }
            append_record_row(
                db,
                &self.session_id,
                &NewRecordRow {
                    seq,
                    id: id.clone(),
                    lane: lane.clone(),
                    run_id: record_run_id(&record),
                    type_: record_type(&record).to_string(),
                    op_kind: record_op_kind(&record),
                    timestamp,
                    payload: record_to_payload(&record),
                },
            )
            .map_err(|message| SessionError::new("storage", message))?;
            if let LaneRecord::OperationFinished(record) = &record {
                finish_lane_operation(db, &self.session_id, &lane, &record.run_id)
                    .map_err(|message| SessionError::new("storage", message))?;
            }
            if let LaneRecord::Usage(record) = &record {
                add_usage_to_stats(db, &self.session_id, &record.usage).map_err(|error| error)?;
            }
            advance_sequence(db, &self.session_id, seq).map_err(|message| SessionError::new("storage", message))?;
            Ok(committed)
        })
    }

    fn get_entry(&self, id: &str) -> Result<Option<Entry>, SessionError> {
        let row = read_entry_row(self.db.as_ref(), &self.session_id, id)
            .map_err(|message| SessionError::new("storage", message))?;
        row.map(|row| decode_entry(&row)).transpose()
    }

    fn find_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>, SessionError> {
        let sql_type = query.type_.clone().or_else(|| {
            if query.custom_type.is_some() {
                Some("custom".to_string())
            } else {
                None
            }
        });
        let sql_limit = if query.custom_type.is_some() { None } else { query.limit };
        let rows = read_entry_rows(
            self.db.as_ref(),
            &self.session_id,
            &ReadEntryRowsOptions {
                after_seq: None,
                cursor: query.cursor.as_ref().map(|cursor| cursor.after_seq),
                type_: sql_type,
                oldest_first: Some(query.order == Some(EntryOrder::OldestFirst)),
                limit: sql_limit,
            },
        )
        .map_err(|message| SessionError::new("storage", message))?;
        let mut entries: Vec<Entry> = rows
            .iter()
            .map(decode_entry)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|entry| matches_entry_query(entry, query))
            .collect();
        if let Some(limit) = query.limit {
            entries.truncate(limit as usize);
        }
        Ok(entries)
    }

    fn find_entries_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &pi_agent_core::harness::session_types::BranchBounds,
        start: &str,
    ) -> Result<Vec<Entry>, SessionError> {
        let cached = read_cached_branch(self.db.as_ref(), &self.session_id, start)
            .map_err(|message| SessionError::new("storage", message))?;
        let Some(cached) = cached else {
            let exists = read_entry_row(self.db.as_ref(), &self.session_id, start)
                .map_err(|message| SessionError::new("storage", message))?
                .is_some();
            if !exists {
                return Err(SessionError::new("not_found", format!("Entry not found: {start}")));
            }
            return Err(SessionError::new(
                "invalid_entry",
                format!("Branch cache missing entry {start}"),
            ));
        };
        let rows = query_cached_branch_rows(
            self.db.as_ref(),
            &self.session_id,
            &cached,
            &CachedBranchQuery {
                type_: query.type_.clone(),
                custom_type: query.custom_type.clone(),
                stop_at_type: bounds.stop_at_type.clone(),
                stop_at_id: bounds.stop_at_id.clone(),
                cursor: query.cursor.as_ref().map(|cursor| cursor.after_seq),
                oldest_first: Some(query.order == Some(EntryOrder::OldestFirst)),
                limit: query.limit,
            },
        )
        .map_err(|message| SessionError::new("storage", message))?;
        validate_cached_branch_rows(&rows, query, &bounds)?;
        let mut entries: Vec<Entry> = rows
            .iter()
            .map(cached_row_to_entry_row)
            .map(|row| decode_entry(&row))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|entry| matches_entry_query(entry, query))
            .collect();
        if let Some(limit) = query.limit {
            entries.truncate(limit as usize);
        }
        Ok(entries)
    }

    fn find_records(&self, query: &RecordQuery) -> Result<Vec<LaneRecord>, SessionError> {
        let rows = read_record_rows(
            self.db.as_ref(),
            &self.session_id,
            &ReadRecordRowsOptions {
                lane: query.lane.clone(),
                type_: query.type_.clone(),
                run_id: query.run_id.clone(),
                operation_kind: query.operation_kind.clone(),
                after_seq: query.after_seq,
                oldest_first: Some(query.order == Some(EntryOrder::OldestFirst)),
                limit: query.limit,
            },
        )
        .map_err(|message| SessionError::new("storage", message))?;
        rows.iter()
            .map(|row| decode_record(row.seq, row.timestamp, &row.payload))
            .collect()
    }

    fn find_open_operations(&self, lane: &str, _limit: Option<f64>) -> Result<Vec<OperationStartedRecord>, SessionError> {
        let rows = read_open_operation_rows(self.db.as_ref(), &self.session_id, lane).map_err(|error| error)?;
        rows.iter()
            .map(|row| {
                let record = decode_record(row.seq, row.timestamp, &row.payload)?;
                match record {
                    LaneRecord::OperationStarted(record) => Ok(record),
                    _ => Err(SessionError::new("storage", "Expected operation_started record")),
                }
            })
            .collect()
    }

    fn get_log(&self, options: &LogOptions) -> Result<Vec<LogItem>, SessionError> {
        let after_seq = options.after_seq.unwrap_or(0.0);
        let limit = options.limit;
        let entry_rows = read_entry_rows(
            self.db.as_ref(),
            &self.session_id,
            &ReadEntryRowsOptions {
                after_seq: Some(after_seq),
                cursor: None,
                type_: None,
                oldest_first: Some(true),
                limit,
            },
        )
        .map_err(|message| SessionError::new("storage", message))?;
        let record_rows = read_record_rows(
            self.db.as_ref(),
            &self.session_id,
            &ReadRecordRowsOptions {
                after_seq: Some(after_seq),
                oldest_first: Some(true),
                limit,
                ..ReadRecordRowsOptions::default()
            },
        )
        .map_err(|message| SessionError::new("storage", message))?;
        let lane_rows = read_lane_move_rows(self.db.as_ref(), &self.session_id, Some(after_seq), limit)
            .map_err(|message| SessionError::new("storage", message))?;
        let fact_rows = read_fact_rows(self.db.as_ref(), &self.session_id, Some(after_seq), limit)
            .map_err(|message| SessionError::new("storage", message))?;

        struct LogRow {
            seq: f64,
            decode: Box<dyn FnOnce() -> LogItem>,
        }
        let mut log_rows: Vec<LogRow> = Vec::new();
        for row in &entry_rows {
            let row = row.clone();
            log_rows.push(LogRow {
                seq: row.seq,
                decode: Box::new(move || LogItem::Entry {
                    seq: row.seq,
                    entry: decode_entry(&row).expect("entry decode"),
                }),
            });
        }
        for row in &record_rows {
            let row = row.clone();
            log_rows.push(LogRow {
                seq: row.seq,
                decode: Box::new(move || LogItem::Record {
                    seq: row.seq,
                    record: decode_record(row.seq, row.timestamp, &row.payload).expect("record decode"),
                }),
            });
        }
        for row in &lane_rows {
            let row = row.clone();
            log_rows.push(LogRow {
                seq: row.seq,
                decode: Box::new(move || LogItem::Lane {
                    seq: row.seq,
                    lane: row.lane.clone(),
                    leaf_id: row.leaf_id.clone(),
                }),
            });
        }
        for row in &fact_rows {
            let row = row.clone();
            log_rows.push(LogRow {
                seq: row.seq,
                decode: Box::new(move || {
                    if row.kind == "name" {
                        let name = row.value.as_deref().map(|value| {
                            match pi_ai::utils::json::parse_json_with_repair(value) {
                                Ok(Value::String(name)) => name,
                                _ => String::new(),
                            }
                        });
                        LogItem::NameFact { seq: row.seq, name }
                    } else {
                        let label = row.value.as_deref().map(|value| {
                            match pi_ai::utils::json::parse_json_with_repair(value) {
                                Ok(Value::String(label)) => label,
                                _ => String::new(),
                            }
                        });
                        LogItem::LabelFact {
                            seq: row.seq,
                            target_id: row.key.clone().unwrap_or_default(),
                            label,
                        }
                    }
                }),
            });
        }
        log_rows.sort_by(|left, right| left.seq.partial_cmp(&right.seq).unwrap_or(std::cmp::Ordering::Equal));
        if let Some(limit) = options.limit {
            log_rows.truncate(limit as usize);
        }
        Ok(log_rows.into_iter().map(|row| (row.decode)()).collect())
    }

    fn get_name(&self) -> Result<Option<String>, SessionError> {
        let row = read_latest_fact(self.db.as_ref(), &self.session_id, "name", None)
            .map_err(|message| SessionError::new("storage", message))?;
        match row.and_then(|row| row.value) {
            None => Ok(None),
            Some(value) => match pi_ai::utils::json::parse_json_with_repair(&value) {
                Ok(Value::String(name)) => Ok(Some(name)),
                _ => Ok(None),
            },
        }
    }

    fn set_name(&self, name: Option<&str>) -> Result<(), SessionError> {
        self.enqueue_write(|db| {
            let seq = get_next_sequence(db, &self.session_id).map_err(|error| error)?;
            append_fact(
                db,
                &self.session_id,
                seq,
                "name",
                None,
                name.map(|name| crate::util::json_stringify(&Value::String(name.to_string()))).as_deref(),
            )
            .map_err(|message| SessionError::new("storage", message))?;
            advance_sequence(db, &self.session_id, seq).map_err(|message| SessionError::new("storage", message))?;
            Ok(())
        })
    }

    fn get_label(&self, id: &str) -> Result<Option<String>, SessionError> {
        let row = read_latest_fact(self.db.as_ref(), &self.session_id, "label", Some(id))
            .map_err(|message| SessionError::new("storage", message))?;
        match row.and_then(|row| row.value) {
            None => Ok(None),
            Some(value) => match pi_ai::utils::json::parse_json_with_repair(&value) {
                Ok(Value::String(label)) => Ok(Some(label)),
                _ => Ok(None),
            },
        }
    }

    fn set_label(&self, id: &str, label: Option<&str>) -> Result<(), SessionError> {
        self.enqueue_write(|db| {
            if read_entry_row(db, &self.session_id, id)
                .map_err(|message| SessionError::new("storage", message))?
                .is_none()
            {
                return Err(SessionError::new("not_found", format!("Entry not found: {id}")));
            }
            let seq = get_next_sequence(db, &self.session_id).map_err(|error| error)?;
            append_fact(
                db,
                &self.session_id,
                seq,
                "label",
                Some(id),
                label.map(|label| crate::util::json_stringify(&Value::String(label.to_string()))).as_deref(),
            )
            .map_err(|message| SessionError::new("storage", message))?;
            advance_sequence(db, &self.session_id, seq).map_err(|message| SessionError::new("storage", message))?;
            Ok(())
        })
    }

    fn get_stats(&self) -> Result<SessionStats, SessionError> {
        read_stats(self.db.as_ref(), &self.session_id).map_err(|error| error)
    }
}

fn custom_type_of(entry: &Entry) -> Option<String> {
    match entry {
        Entry::Custom(entry) => Some(entry.custom_type.clone()),
        _ => None,
    }
}

fn cached_row_to_entry_row(row: &CachedBranchEntryRow) -> EntryRow {
    EntryRow {
        session_id: row.session_id.clone(),
        seq: row.entry_seq,
        id: row.id.clone(),
        parent_id: row.parent_id.clone(),
        type_: row.type_.clone(),
        timestamp: row.timestamp,
        payload: row.payload.clone(),
    }
}

fn matches_entry_query(entry: &Entry, query: &EntryQuery) -> bool {
    if let Some(type_) = &query.type_ {
        if entry.type_name() != type_ {
            return false;
        }
    }
    if let Some(custom_type) = &query.custom_type {
        match entry {
            Entry::Custom(entry) if &entry.custom_type == custom_type => {}
            _ => return false,
        }
    }
    if let Some(cursor) = &query.cursor {
        let seq = entry.seq();
        if query.order == Some(EntryOrder::OldestFirst) {
            if seq <= cursor.after_seq {
                return false;
            }
        } else if seq >= cursor.after_seq {
            return false;
        }
    }
    true
}

fn validate_cached_branch_rows(
    rows: &[CachedBranchEntryRow],
    query: &EntryQuery,
    _bounds: &pi_agent_core::harness::session_types::BranchBounds,
) -> Result<(), SessionError> {
    if rows.is_empty() || query.type_.is_some() || query.custom_type.is_some() {
        return Ok(());
    }
    let mut path: Vec<&CachedBranchEntryRow> = rows.iter().collect();
    path.sort_by(|left, right| left.entry_seq.partial_cmp(&right.entry_seq).unwrap_or(std::cmp::Ordering::Equal));
    let should_include_root = query.cursor.is_none() && (query.order == Some(EntryOrder::OldestFirst) || query.limit.is_none());
    if should_include_root {
        if let Some(first) = path.first() {
            if first.parent_id.is_some() {
                return Err(SessionError::new(
                    "invalid_entry",
                    format!("Entry {:?} not found", first.parent_id),
                ));
            }
        }
    }
    for index in 1..path.len() {
        let previous = path[index - 1];
        let current = path[index];
        if current.parent_id.as_deref() != Some(previous.id.as_str()) {
            return Err(SessionError::new(
                "invalid_entry",
                format!("Entry {:?} not found", current.parent_id),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Repository
// ---------------------------------------------------------------------------

pub struct SqliteSessionRepository {
    database_path: String,
    db: Mutex<Option<Arc<RusqliteDatabase>>>,
    active_storages: Mutex<Vec<Arc<SqliteSessionStorage>>>,
    operation_lock: Mutex<()>,
    lease_options: ResolvedWriterLeaseOptions,
}

impl SqliteSessionRepository {
    pub fn new(options: SqliteSessionRepositoryOptions) -> Result<Arc<Self>, String> {
        let lease_options = resolve_writer_lease_options(options.writer_lease.as_ref())?;
        Ok(Arc::new(Self {
            database_path: options.database_path,
            db: Mutex::new(None),
            active_storages: Mutex::new(Vec::new()),
            operation_lock: Mutex::new(()),
            lease_options,
        }))
    }

    fn get_database(&self) -> Result<Arc<RusqliteDatabase>, SessionError> {
        let mut slot = self.db.lock().unwrap();
        if let Some(db) = &*slot {
            return Ok(db.clone());
        }
        let directory = get_parent_path(&self.database_path);
        std::fs::create_dir_all(&directory)
            .map_err(|error| SessionError::new("storage", format!("Failed to create SQLite sessions directory {}: {error}", self.database_path)))?;
        let db = crate::database::FileDatabaseFactory::open_file(&self.database_path)
            .map_err(|message| SessionError::new("storage", message))?;
        configure_sqlite_database(&db)?;
        apply_migrations(&db).map_err(|message| SessionError::new("storage", message))?;
        let db = Arc::new(db);
        *slot = Some(db.clone());
        Ok(db)
    }

    fn claim_storage(
        &self,
        db: &Arc<RusqliteDatabase>,
        metadata: &SqliteSessionMetadata,
    ) -> Result<Arc<SqliteSessionStorage>, SessionError> {
        let active = self
            .active_storages
            .lock()
            .unwrap()
            .iter()
            .find(|storage| storage.session_id == metadata.id)
            .cloned();
        if let Some(active) = active {
            let _ = read_lanes(db.as_ref(), &metadata.id)?;
            return Ok(active);
        }
        let db_ref: &dyn SqliteDatabase = db.as_ref();
        require_session_row(db_ref, &metadata.id)?;
        let lease = claim_writer_lease(db_ref, &metadata.id, &self.lease_options)?;
        let row = require_session_row(db_ref, &metadata.id)?;
        let _ = read_lanes(db_ref, &metadata.id)?;
        let decoded = decode_session_metadata(&row, &self.database_path).map_err(|error| error)?;
        let storage = SqliteSessionStorage::new(db.clone(), decoded, lease, self.lease_options.clone());
        self.active_storages.lock().unwrap().push(storage.clone());
        Ok(storage)
    }

    /// Create a session and return its storage (JS `create`).
    pub fn create_session(
        &self,
        id: Option<&str>,
        cwd: &str,
        parent_session_id: Option<&str>,
        metadata: Option<Value>,
    ) -> Result<Arc<SqliteSessionStorage>, SessionError> {
        let _guard = self.operation_lock.lock().unwrap();
        let db = self.get_database()?;
        let id = id.map(|id| id.to_string()).unwrap_or_else(uuid_v7);
        if session_exists(db.as_ref(), &id).map_err(|message| SessionError::new("storage", message))? {
            return Err(SessionError::new("already_exists", format!("Session already exists: {id}")));
        }
        let created_at = now_ms();
        let lease = {
            insert_session_row(
                db.as_ref(),
                &NewSessionRow {
                    id: id.clone(),
                    created_at,
                    cwd: cwd.to_string(),
                    parent_session_id: parent_session_id.map(|value| value.to_string()),
                    metadata,
                },
            )
            .map_err(|error| error)?;
            create_sequence(db.as_ref(), &id, 1.0).map_err(|message| SessionError::new("storage", message))?;
            create_stats(db.as_ref(), &id, 0.0).map_err(|message| SessionError::new("storage", message))?;
            create_initial_lane(db.as_ref(), &id, "main", None).map_err(|message| SessionError::new("storage", message))?;
            claim_writer_lease(db.as_ref(), &id, &self.lease_options)?
        };
        let row = require_session_row(db.as_ref(), &id)?;
        let decoded = decode_session_metadata(&row, &self.database_path).map_err(|error| error)?;
        let storage = SqliteSessionStorage::new(db, decoded, lease, self.lease_options.clone());
        self.active_storages.lock().unwrap().push(storage.clone());
        Ok(storage)
    }

    /// Open an existing session (JS `open`).
    pub fn open_session(&self, metadata: &SessionMetadata) -> Result<Arc<SqliteSessionStorage>, SessionError> {
        let _guard = self.operation_lock.lock().unwrap();
        let db = self.get_database()?;
        let row = require_session_row(db.as_ref(), &metadata.id)?;
        let decoded = decode_session_metadata(&row, &self.database_path).map_err(|error| error)?;
        self.claim_storage(&db, &decoded)
    }

    /// List session metadata (JS `list`).
    pub fn list_sessions(&self, cwd: Option<&str>) -> Result<Vec<SqliteSessionMetadata>, SessionError> {
        let _guard = self.operation_lock.lock().unwrap();
        if !std::path::Path::new(&self.database_path).exists() {
            return Ok(vec![]);
        }
        let db = self.get_database()?;
        let rows = read_session_rows(db.as_ref(), cwd).map_err(|message| SessionError::new("storage", message))?;
        rows.iter()
            .map(|row| decode_session_metadata(row, &self.database_path))
            .collect()
    }

    /// Delete a session (JS `delete`).
    pub fn delete_session(&self, metadata: &SessionMetadata) -> Result<(), SessionError> {
        let _guard = self.operation_lock.lock().unwrap();
        self.release_storages_for_session(&metadata.id)?;
        let db = self.get_database()?;
        let db_ref: &dyn SqliteDatabase = db.as_ref();
        if !session_exists(db_ref, &metadata.id).map_err(|message| SessionError::new("storage", message))? {
            delete_writer_lease(db_ref, &metadata.id).map_err(|message| SessionError::new("storage", message))?;
            return Ok(());
        }
        let lease = claim_writer_lease(db_ref, &metadata.id, &self.lease_options)?;
        delete_branch_tips(db_ref, &metadata.id).map_err(|message| SessionError::new("storage", message))?;
        crate::storage::branch_cache::delete_branch_entries(db_ref, &metadata.id)
            .map_err(|message| SessionError::new("storage", message))?;
        delete_fact_rows(db_ref, &metadata.id).map_err(|message| SessionError::new("storage", message))?;
        delete_lane_rows(db_ref, &metadata.id).map_err(|message| SessionError::new("storage", message))?;
        delete_record_rows(db_ref, &metadata.id).map_err(|message| SessionError::new("storage", message))?;
        delete_entry_rows(db_ref, &metadata.id).map_err(|message| SessionError::new("storage", message))?;
        delete_writer_lease(db_ref, &metadata.id).map_err(|message| SessionError::new("storage", message))?;
        delete_stats(db_ref, &metadata.id).map_err(|message| SessionError::new("storage", message))?;
        delete_sequence(db_ref, &metadata.id).map_err(|message| SessionError::new("storage", message))?;
        delete_session_row(db_ref, &metadata.id).map_err(|message| SessionError::new("storage", message))?;
        let _ = release_writer_lease(db_ref, &metadata.id, &lease);
        Ok(())
    }

    /// Repair the branch cache (JS `repairBranchCache`).
    pub fn repair_branch_cache(&self, metadata: &SessionMetadata) -> Result<(), SessionError> {
        let _guard = self.operation_lock.lock().unwrap();
        self.release_storages_for_session(&metadata.id)?;
        let db = self.get_database()?;
        let db_ref: &dyn SqliteDatabase = db.as_ref();
        require_session_row(db_ref, &metadata.id)?;
        let lease = claim_writer_lease(db_ref, &metadata.id, &self.lease_options)?;
        let result = rebuild_branch_cache(db_ref, &metadata.id);
        let _ = release_writer_lease(db_ref, &metadata.id, &lease);
        result.map_err(|message| SessionError::new("storage", message))
    }

    /// Close the repository (JS `close`).
    pub fn close(&self) -> Result<(), SessionError> {
        let storages: Vec<Arc<SqliteSessionStorage>> = self.active_storages.lock().unwrap().clone();
        for storage in storages {
            storage.release()?;
        }
        self.active_storages.lock().unwrap().clear();
        *self.db.lock().unwrap() = None;
        Ok(())
    }

    fn release_storages_for_session(&self, session_id: &str) -> Result<(), SessionError> {
        let storages: Vec<Arc<SqliteSessionStorage>> = self
            .active_storages
            .lock()
            .unwrap()
            .iter()
            .filter(|storage| storage.session_id == session_id)
            .cloned()
            .collect();
        for storage in storages {
            storage.release()?;
        }
        self.active_storages
            .lock()
            .unwrap()
            .retain(|storage| storage.session_id != session_id);
        Ok(())
    }
}

fn require_session_row(db: &dyn SqliteDatabase, session_id: &str) -> Result<crate::storage::sessions::SessionRow, SessionError> {
    read_session_row(db, session_id)
        .map_err(|message| SessionError::new("storage", message))?
        .ok_or_else(|| SessionError::new("not_found", format!("Session not found: {session_id}")))
}

fn claim_writer_lease(
    db: &dyn SqliteDatabase,
    session_id: &str,
    options: &ResolvedWriterLeaseOptions,
) -> Result<WriterLease, SessionError> {
    let now = now_ms();
    let lease = acquire_writer_lease(db, session_id, &uuid_v7(), now, now + options.ttl_ms)
        .map_err(|message| SessionError::new("storage", message))?;
    lease.ok_or_else(|| active_writer_error(session_id))
}

fn configure_sqlite_database(db: &dyn SqliteDatabase) -> Result<(), SessionError> {
    db.exec("PRAGMA journal_mode=WAL")
        .map_err(|message| SessionError::new("storage", message))?;
    db.exec("PRAGMA synchronous=FULL")
        .map_err(|message| SessionError::new("storage", message))?;
    db.exec("PRAGMA busy_timeout=5000")
        .map_err(|message| SessionError::new("storage", message))?;
    Ok(())
}

fn get_parent_path(path: &str) -> String {
    let normalized = path.trim_end_matches(['/', '\\']);
    let last_slash = normalized
        .rfind('/')
        .map(|index| index as isize)
        .or_else(|| normalized.rfind('\\').map(|index| index as isize));
    match last_slash {
        None => ".".to_string(),
        Some(0) => normalized[..1].to_string(),
        Some(index) => normalized[..index as usize].to_string(),
    }
}

/// Test accessor extension.
pub trait SessionStorageExt {
    fn session_id_for_test(&self) -> String;
}

impl SessionStorageExt for SqliteSessionStorage {
    fn session_id_for_test(&self) -> String {
        self.session_id.clone()
    }
}

/// Session trait implementation for the sqlite storage.
pub type SqliteSession = pi_agent_core::harness::session::Session<SqliteSessionStorage>;

// Re-export entry helpers used by the crate.
pub use pi_agent_core::harness::session_types::SessionTree;
