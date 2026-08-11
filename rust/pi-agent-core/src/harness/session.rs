//! Session and in-memory storage, ports of
//! `packages/agent/src/harness/session/{session,memory}.ts`.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::harness::session_state::{assert_valid_cursor, assert_valid_limit, SessionState};
use crate::harness::session_types::{
    BranchBounds, Entry, EntryOrder, EntryQuery, ForkOptions, LanePointer, LaneRecord, LogItem,
    LogOptions, OperationStartedRecord, ProvisionedEntry, RecordQuery, SessionCreateOptions,
    SessionError, SessionMetadata, SessionRepo, SessionStats, SessionStorage, SessionTree,
};
use crate::types::AgentMessage;

/// JSON-serializability validation is trivially satisfied by Rust values
/// (no cycles, non-finite numbers, or non-plain objects are representable in
/// the value tree). Kept as a named function for call-site parity.
pub fn assert_json_serializable(_value: &impl Sized) {}

fn validate_metadata(metadata: &SessionMetadata) -> Result<(), SessionError> {
    if metadata.id.is_empty() {
        return Err(SessionError::new("invalid_payload", "Durable payload contains an empty id"));
    }
    Ok(())
}

/// Session over a storage backend, port of `Session`.
pub struct Session<S: SessionStorage> {
    storage: S,
}

impl<S: SessionStorage> Session<S> {
    pub fn new(storage: S) -> Self {
        Self { storage }
    }

    pub fn get_metadata(&self) -> Result<SessionMetadata, SessionError> {
        self.storage.get_metadata()
    }

    pub fn view(&self) -> &Self {
        self
    }

    pub fn get_leaf_id(&self) -> Result<Option<String>, SessionError> {
        self.get_leaf_id_for_lane("main")
    }

    fn get_leaf_id_for_lane(&self, lane: &str) -> Result<Option<String>, SessionError> {
        let pointer = self
            .storage
            .get_lanes()?
            .into_iter()
            .find(|candidate| candidate.lane == lane);
        match pointer {
            Some(pointer) => Ok(pointer.leaf_id),
            None => Err(SessionError::new("invalid_lane", format!("Lane not found: {lane}"))),
        }
    }

    pub fn get_entry(&self, id: &str) -> Result<Option<Entry>, SessionError> {
        self.storage.get_entry(id)
    }

    pub fn get_stats(&self) -> Result<SessionStats, SessionError> {
        self.storage.get_stats()
    }

    pub fn get_name(&self) -> Result<Option<String>, SessionError> {
        self.storage.get_name()
    }

    pub fn set_name(&self, name: Option<&str>) -> Result<(), SessionError> {
        self.storage.set_name(name)
    }

    pub fn get_label(&self, target_id: &str) -> Result<Option<String>, SessionError> {
        self.storage.get_label(target_id)
    }

    pub fn set_label(&self, target_id: &str, label: Option<&str>) -> Result<(), SessionError> {
        self.storage.set_label(target_id, label)
    }

    pub fn find_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>, SessionError> {
        assert_valid_limit(query.limit)?;
        assert_valid_cursor(query.cursor.as_ref().map(|c| c.after_seq))?;
        self.storage.find_entries(query)
    }

    pub fn find_entry(&self, query: &EntryQuery) -> Result<Option<Entry>, SessionError> {
        let mut query = query.clone();
        query.limit = Some(1.0);
        Ok(self.storage.find_entries(&query)?.into_iter().next())
    }

    pub fn find_entries_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &BranchBounds,
    ) -> Result<Vec<Entry>, SessionError> {
        assert_valid_limit(query.limit)?;
        assert_valid_cursor(query.cursor.as_ref().map(|c| c.after_seq))?;
        let start = match bounds.start.clone() {
            Some(start) => start,
            None => match self.get_leaf_id_for_lane("main")? {
                Some(start) => start,
                None => return Ok(Vec::new()),
            },
        };
        self.storage.find_entries_on_branch(query, bounds, &start)
    }

    pub fn find_entry_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &BranchBounds,
    ) -> Result<Option<Entry>, SessionError> {
        let mut query = query.clone();
        query.limit = Some(1.0);
        Ok(self.find_entries_on_branch(&query, bounds)?.into_iter().next())
    }

    pub fn append_message(&self, message: AgentMessage) -> Result<String, SessionError> {
        self.append_message_to_lane("main", message)
    }

    pub fn append_custom_entry(&self, custom_type: &str, data: Option<pi_ai::types::JsonValue>) -> Result<String, SessionError> {
        self.append_custom_entry_to_lane("main", custom_type, data)
    }

    pub fn get_lanes(&self) -> Result<Vec<LanePointer>, SessionError> {
        self.storage.get_lanes()
    }

    pub fn create_lane(&self, lane: &str, at: Option<&str>) -> Result<(), SessionError> {
        self.storage.create_lane(lane, at)
    }

    pub fn move_lane(&self, lane: &str, to: Option<&str>) -> Result<(), SessionError> {
        self.storage.move_lane(lane, to)
    }

    pub fn append_entry(&self, entry: ProvisionedEntry, lane: &str) -> Result<Entry, SessionError> {
        self.commit_entry(entry, lane)
    }

    pub fn append_record(&self, record: LaneRecord) -> Result<LaneRecord, SessionError> {
        self.commit_record(record)
    }

    pub fn find_records(&self, query: &RecordQuery) -> Result<Vec<LaneRecord>, SessionError> {
        assert_valid_limit(query.limit)?;
        assert_valid_cursor(query.after_seq)?;
        if query.operation_kind.is_some() && query.type_.as_deref() != Some("operation_started") {
            return Err(SessionError::new(
                "invalid_query",
                "operationKind requires type \"operation_started\"",
            ));
        }
        self.storage.find_records(query)
    }

    pub fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<f64>,
    ) -> Result<Vec<OperationStartedRecord>, SessionError> {
        assert_valid_limit(limit)?;
        self.storage.find_open_operations(lane, limit)
    }

    pub fn get_log(&self, options: &LogOptions) -> Result<Vec<LogItem>, SessionError> {
        assert_valid_limit(options.limit)?;
        assert_valid_cursor(options.after_seq)?;
        self.storage.get_log(options)
    }

    fn append_message_to_lane(&self, lane: &str, message: AgentMessage) -> Result<String, SessionError> {
        let entry = self.commit_entry(
            Entry::Message(crate::harness::session_types::MessageEntry {
                base: crate::harness::session_types::EntryBase {
                    type_: "message".to_string(),
                    id: uuid(),
                    seq: 0.0,
                    parent_id: None,
                    timestamp: 0.0,
                },
                message,
                terminate: None,
            }),
            lane,
        )?;
        Ok(entry.id().to_string())
    }

    fn append_custom_entry_to_lane(
        &self,
        lane: &str,
        custom_type: &str,
        data: Option<pi_ai::types::JsonValue>,
    ) -> Result<String, SessionError> {
        let entry = self.commit_entry(
            Entry::Custom(crate::harness::session_types::CustomEntry {
                base: crate::harness::session_types::EntryBase {
                    type_: "custom".to_string(),
                    id: uuid(),
                    seq: 0.0,
                    parent_id: None,
                    timestamp: 0.0,
                },
                custom_type: custom_type.to_string(),
                data,
            }),
            lane,
        )?;
        Ok(entry.id().to_string())
    }

    fn commit_entry(&self, entry: Entry, lane: &str) -> Result<Entry, SessionError> {
        self.storage.append_entry(entry, lane)
    }

    fn commit_record(&self, record: LaneRecord) -> Result<LaneRecord, SessionError> {
        self.storage.append_record(record)
    }
}

impl<S: SessionStorage> SessionTree for Session<S> {
    fn get_leaf_id(&self) -> Result<Option<String>, SessionError> {
        Session::get_leaf_id(self)
    }
    fn get_entry(&self, id: &str) -> Result<Option<Entry>, SessionError> {
        Session::get_entry(self, id)
    }
    fn get_stats(&self) -> Result<SessionStats, SessionError> {
        Session::get_stats(self)
    }
    fn get_name(&self) -> Result<Option<String>, SessionError> {
        Session::get_name(self)
    }
    fn set_name(&self, name: Option<&str>) -> Result<(), SessionError> {
        Session::set_name(self, name)
    }
    fn get_label(&self, target_id: &str) -> Result<Option<String>, SessionError> {
        Session::get_label(self, target_id)
    }
    fn set_label(&self, target_id: &str, label: Option<&str>) -> Result<(), SessionError> {
        Session::set_label(self, target_id, label)
    }
    fn find_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>, SessionError> {
        Session::find_entries(self, query)
    }
    fn find_entry(&self, query: &EntryQuery) -> Result<Option<Entry>, SessionError> {
        Session::find_entry(self, query)
    }
    fn find_entries_on_branch(&self, query: &EntryQuery, bounds: &BranchBounds) -> Result<Vec<Entry>, SessionError> {
        Session::find_entries_on_branch(self, query, bounds)
    }
    fn find_entry_on_branch(&self, query: &EntryQuery, bounds: &BranchBounds) -> Result<Option<Entry>, SessionError> {
        Session::find_entry_on_branch(self, query, bounds)
    }
    fn append_message(&self, message: AgentMessage) -> Result<String, SessionError> {
        Session::append_message(self, message)
    }
    fn append_custom_entry(&self, custom_type: &str, data: Option<pi_ai::types::JsonValue>) -> Result<String, SessionError> {
        Session::append_custom_entry(self, custom_type, data)
    }
}

/// In-memory storage backend, port of `InMemorySessionStorage`.
pub struct InMemorySessionStorage {
    metadata: SessionMetadata,
    state: Mutex<SessionState>,
}

impl InMemorySessionStorage {
    pub fn new(metadata: SessionMetadata) -> Result<Self, SessionError> {
        validate_metadata(&metadata)?;
        Ok(Self {
            metadata,
            state: Mutex::new(SessionState::new()),
        })
    }

    pub fn fork(
        &self,
        metadata: SessionMetadata,
        options: &ForkOptions,
    ) -> Result<InMemorySessionStorage, SessionError> {
        let storage = InMemorySessionStorage::new(metadata)?;
        let mutations = {
            let state = self.state.lock().unwrap();
            state.create_fork_mutations(options)?
        };
        for mutation in mutations {
            storage.state.lock().unwrap().apply_mutation(mutation)?;
        }
        Ok(storage)
    }
}

impl SessionStorage for InMemorySessionStorage {
    fn get_metadata(&self) -> Result<SessionMetadata, SessionError> {
        Ok(self.metadata.clone())
    }

    fn get_lanes(&self) -> Result<Vec<LanePointer>, SessionError> {
        Ok(self.state.lock().unwrap().get_lanes())
    }

    fn create_lane(&self, lane: &str, at: Option<&str>) -> Result<(), SessionError> {
        let mut state = self.state.lock().unwrap();
        state.validate_new_lane(lane)?;
        state.validate_target(at)?;
        let seq = state.next_sequence();
        state.apply_mutation(crate::harness::session_state::MutKind::Lane {
            seq,
            lane: lane.to_string(),
            leaf_id: at.map(|s| s.to_string()),
        })
    }

    fn move_lane(&self, lane: &str, to: Option<&str>) -> Result<(), SessionError> {
        let mut state = self.state.lock().unwrap();
        state.require_lane(lane)?;
        state.validate_target(to)?;
        let seq = state.next_sequence();
        state.apply_mutation(crate::harness::session_state::MutKind::Lane {
            seq,
            lane: lane.to_string(),
            leaf_id: to.map(|s| s.to_string()),
        })
    }

    fn append_entry(&self, mut entry: Entry, lane: &str) -> Result<Entry, SessionError> {
        let mut state = self.state.lock().unwrap();
        let parent_id = state.require_lane(lane)?;
        state.validate_unused_id(entry.id())?;
        entry.set_parent_id(parent_id);
        state.apply_mutation(crate::harness::session_state::MutKind::Entry {
            lane: Some(lane.to_string()),
            entry: entry.clone(),
        })?;
        Ok(entry)
    }

    fn append_record(&self, mut record: LaneRecord) -> Result<LaneRecord, SessionError> {
        let mut state = self.state.lock().unwrap();
        state.require_lane(record.lane())?;
        state.validate_unused_id(record.id())?;
        let current_open = state.find_open_operations(record.lane(), Some(1.0))?;
        if record.type_name() == "operation_started" {
            if let Some(open) = current_open.first() {
                return Err(SessionError::new(
                    "storage",
                    format!(
                        "Lane {} already has an open operation {}",
                        record.lane(),
                        open.base.id
                    ),
                ));
            }
        }
        let seq = state.next_sequence();
        let timestamp = now_ms();
        record.set_seq_timestamp(seq, timestamp);
        state.apply_mutation(crate::harness::session_state::MutKind::Record {
            record: record.clone(),
        })?;
        Ok(record)
    }

    fn get_entry(&self, id: &str) -> Result<Option<Entry>, SessionError> {
        Ok(self.state.lock().unwrap().get_entry(id).cloned())
    }

    fn find_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>, SessionError> {
        self.state.lock().unwrap().find_entries(query)
    }

    fn find_entries_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &BranchBounds,
        start: &str,
    ) -> Result<Vec<Entry>, SessionError> {
        self.state.lock().unwrap().find_entries_on_branch(query, bounds, start)
    }

    fn find_records(&self, query: &RecordQuery) -> Result<Vec<LaneRecord>, SessionError> {
        self.state.lock().unwrap().find_records(query)
    }

    fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<f64>,
    ) -> Result<Vec<OperationStartedRecord>, SessionError> {
        self.state.lock().unwrap().find_open_operations(lane, limit)
    }

    fn get_log(&self, options: &LogOptions) -> Result<Vec<LogItem>, SessionError> {
        self.state.lock().unwrap().get_log(options)
    }

    fn get_name(&self) -> Result<Option<String>, SessionError> {
        Ok(self.state.lock().unwrap().get_name().map(|s| s.to_string()))
    }

    fn set_name(&self, name: Option<&str>) -> Result<(), SessionError> {
        let mut state = self.state.lock().unwrap();
        let seq = state.next_sequence();
        state.apply_mutation(crate::harness::session_state::MutKind::NameFact {
            seq,
            name: name.map(|s| s.to_string()),
        })
    }

    fn get_label(&self, id: &str) -> Result<Option<String>, SessionError> {
        Ok(self.state.lock().unwrap().get_label(id).map(|s| s.to_string()))
    }

    fn set_label(&self, id: &str, label: Option<&str>) -> Result<(), SessionError> {
        let mut state = self.state.lock().unwrap();
        state.validate_target(Some(id))?;
        let seq = state.next_sequence();
        state.apply_mutation(crate::harness::session_state::MutKind::LabelFact {
            seq,
            target_id: id.to_string(),
            label: label.map(|s| s.to_string()),
        })
    }

    fn get_stats(&self) -> Result<SessionStats, SessionError> {
        Ok(self.state.lock().unwrap().get_stats().clone())
    }
}

/// In-memory repository, port of `InMemorySessionRepo`.
pub struct InMemorySessionRepo {
    sessions: Mutex<HashMap<String, InMemorySessionStorage>>,
}

impl InMemorySessionRepo {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for InMemorySessionRepo {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRepo for InMemorySessionRepo {
    fn create(&mut self, options: &SessionCreateOptions) -> Result<(), SessionError> {
        let id = options.id.clone().unwrap_or_else(uuid);
        let mut sessions = self.sessions.lock().unwrap();
        if sessions.contains_key(&id) {
            return Err(SessionError::new("already_exists", format!("Session already exists: {id}")));
        }
        let storage = InMemorySessionStorage::new(SessionMetadata {
            id,
            created_at: now_ms(),
            parent_session_id: options.parent_session_id.clone(),
        })?;
        sessions.insert(storage.metadata.id.clone(), storage);
        Ok(())
    }

    fn open(&self, metadata: &SessionMetadata) -> Result<(), SessionError> {
        let sessions = self.sessions.lock().unwrap();
        if !sessions.contains_key(&metadata.id) {
            return Err(SessionError::new("not_found", format!("Session not found: {}", metadata.id)));
        }
        Ok(())
    }

    fn list(&self) -> Result<Vec<SessionMetadata>, SessionError> {
        let sessions = self.sessions.lock().unwrap();
        Ok(sessions.values().map(|s| s.metadata.clone()).collect())
    }

    fn delete(&mut self, metadata: &SessionMetadata) -> Result<(), SessionError> {
        self.sessions.lock().unwrap().remove(&metadata.id);
        Ok(())
    }

    fn fork(
        &mut self,
        source: &SessionMetadata,
        options: &ForkOptions,
        create: &SessionCreateOptions,
    ) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().unwrap();
        let source_storage = sessions.get(&source.id).ok_or_else(|| {
            SessionError::new("not_found", format!("Session not found: {}", source.id))
        })?;
        let id = create.id.clone().unwrap_or_else(uuid);
        if sessions.contains_key(&id) {
            return Err(SessionError::new("already_exists", format!("Session already exists: {id}")));
        }
        let storage = source_storage.fork(
            SessionMetadata {
                id,
                created_at: now_ms(),
                parent_session_id: create
                    .parent_session_id
                    .clone()
                    .or_else(|| Some(source.id.clone())),
            },
            options,
        )?;
        sessions.insert(storage.metadata.id.clone(), storage);
        Ok(())
    }
}

fn uuid() -> String {
    pi_ai::utils::uuid::uuidv7()
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as f64
}

#[allow(dead_code)]
fn _order_default() -> EntryOrder {
    EntryOrder::NewestFirst
}
