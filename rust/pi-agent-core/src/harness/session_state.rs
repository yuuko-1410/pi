//! Session state machine, port of `packages/agent/src/harness/session/state.ts`.

use std::collections::{HashMap, HashSet};

use crate::harness::session_types::{
    BranchBounds, Entry, EntryOrder, EntryQuery, ForkOptions, LanePointer, LaneRecord, LogItem,
    LogOptions, OperationStartedRecord, RecordQuery, SessionError, SessionStats,
};

pub type SessionMutation = MutKind;

#[derive(Clone, Debug)]
pub enum MutKind {
    Entry { lane: Option<String>, entry: Entry },
    Record { record: LaneRecord },
    Lane { seq: f64, lane: String, leaf_id: Option<String> },
    NameFact { seq: f64, name: Option<String> },
    LabelFact { seq: f64, target_id: String, label: Option<String> },
}

impl SessionMutation {
    fn seq(&self) -> f64 {
        match self {
            MutKind::Entry { entry, .. } => entry.seq(),
            MutKind::Record { record } => record.seq(),
            MutKind::Lane { seq, .. } | MutKind::NameFact { seq, .. } | MutKind::LabelFact { seq, .. } => *seq,
        }
    }
}

fn invalid_mutation(message: &str) -> SessionError {
    SessionError::new("invalid_entry", format!("Invalid session mutation: {message}"))
}

pub fn assert_valid_limit(limit: Option<f64>) -> Result<(), SessionError> {
    if let Some(limit) = limit {
        if limit.fract() != 0.0 || limit <= 0.0 {
            return Err(SessionError::new(
                "invalid_query",
                "limit must be a positive integer",
            ));
        }
    }
    Ok(())
}

pub fn assert_valid_cursor(after_seq: Option<f64>) -> Result<(), SessionError> {
    if let Some(after_seq) = after_seq {
        if after_seq.fract() != 0.0 || after_seq < 0.0 {
            return Err(SessionError::new(
                "invalid_query",
                "cursor sequence must be a non-negative integer",
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct SessionState {
    sequence: f64,
    used_ids: HashSet<String>,
    entries: Vec<Entry>,
    entries_by_id: HashMap<String, Entry>,
    records: Vec<LaneRecord>,
    open_operations_by_lane: HashMap<String, HashMap<String, OperationStartedRecord>>,
    lanes: HashMap<String, Option<String>>,
    log: Vec<LogItem>,
    stats: SessionStats,
    name: Option<String>,
    labels: HashMap<String, String>,
}

impl SessionState {
    pub fn new() -> Self {
        let mut lanes = HashMap::new();
        lanes.insert("main".to_string(), None);
        Self {
            lanes,
            ..Self::default()
        }
    }

    pub fn next_sequence(&self) -> f64 {
        self.sequence + 1.0
    }

    pub fn get_lanes(&self) -> Vec<LanePointer> {
        self.lanes
            .iter()
            .map(|(lane, leaf_id)| LanePointer {
                lane: lane.clone(),
                leaf_id: leaf_id.clone(),
            })
            .collect()
    }

    pub fn require_lane(&self, lane: &str) -> Result<Option<String>, SessionError> {
        match self.lanes.get(lane) {
            Some(leaf_id) => Ok(leaf_id.clone()),
            None => Err(SessionError::new("invalid_lane", format!("Lane not found: {lane}"))),
        }
    }

    pub fn validate_new_lane(&self, lane: &str) -> Result<(), SessionError> {
        if self.lanes.contains_key(lane) {
            return Err(SessionError::new("already_exists", format!("Lane already exists: {lane}")));
        }
        Ok(())
    }

    pub fn validate_target(&self, target_id: Option<&str>) -> Result<(), SessionError> {
        if let Some(target_id) = target_id {
            if !self.entries_by_id.contains_key(target_id) {
                return Err(SessionError::new("not_found", format!("Entry not found: {target_id}")));
            }
        }
        Ok(())
    }

    pub fn validate_unused_id(&self, id: &str) -> Result<(), SessionError> {
        if self.used_ids.contains(id) {
            return Err(SessionError::new("already_exists", format!("Session id already exists: {id}")));
        }
        Ok(())
    }

    pub fn apply_mutation(&mut self, mutation: SessionMutation) -> Result<(), SessionError> {
        let seq = mutation.seq();
        if seq != self.sequence + 1.0 {
            return Err(invalid_mutation(&format!("has non-consecutive seq {seq}")));
        }

        match mutation {
            MutKind::Entry { lane, entry } => {
                if self.used_ids.contains(entry.id()) {
                    return Err(invalid_mutation(&format!("contains duplicate id {}", entry.id())));
                }
                if let Some(lane_name) = &lane {
                    let leaf_id = self.lanes.get(lane_name).ok_or_else(|| {
                        invalid_mutation(&format!("references missing lane {lane_name}"))
                    })?;
                    if entry.parent_id().as_deref() != leaf_id.as_deref() {
                        return Err(invalid_mutation("does not chain to the lane leaf"));
                    }
                }
                if entry.parent_id().is_some()
                    && !self
                        .entries_by_id
                        .contains_key(entry.parent_id().expect("checked above"))
                {
                    return Err(invalid_mutation(&format!(
                        "references missing parent {}",
                        entry.parent_id().expect("checked above")
                    )));
                }
                self.sequence = seq;
                self.used_ids.insert(entry.id().to_string());
                if entry.type_name() == "message" {
                    self.stats.message_count += 1.0;
                }
                let log_entry = entry.clone();
                self.log.push(LogItem::Entry {
                    seq,
                    entry: log_entry,
                });
                let entry_id = entry.id().to_string();
                if let Some(lane_name) = &lane {
                    self.lanes.insert(lane_name.clone(), Some(entry_id.clone()));
                }
                self.entries.push(entry.clone());
                self.entries_by_id.insert(entry_id, entry);
            }
            MutKind::Record { record } => {
                if !self.lanes.contains_key(record.lane()) {
                    return Err(invalid_mutation(&format!("references missing lane {}", record.lane())));
                }
                if self.used_ids.contains(record.id()) {
                    return Err(invalid_mutation(&format!("contains duplicate id {}", record.id())));
                }
                self.sequence = seq;
                self.used_ids.insert(record.id().to_string());
                match &record {
                    LaneRecord::OperationStarted(started) => {
                        let open = self
                            .open_operations_by_lane
                            .entry(started.base.lane.clone())
                            .or_default();
                        open.insert(started.base.id.clone(), started.clone());
                    }
                    LaneRecord::OperationFinished(finished) => {
                        if let Some(open) = self.open_operations_by_lane.get_mut(&finished.base.lane) {
                            open.remove(&finished.run_id);
                        }
                    }
                    LaneRecord::Usage(usage) => {
                        self.stats.cached_tokens += usage.usage.cache_read;
                        self.stats.uncached_tokens += usage.usage.input + usage.usage.cache_write;
                        self.stats.total_tokens += usage.usage.total_tokens;
                        self.stats.cost_total += usage.usage.cost.total;
                    }
                    _ => {}
                }
                let log_record = record.clone();
                self.log.push(LogItem::Record {
                    seq,
                    record: log_record,
                });
                self.records.push(record);
            }
            MutKind::Lane {
                seq,
                lane,
                leaf_id,
            } => {
                if let Some(leaf_id) = &leaf_id {
                    if !self.entries_by_id.contains_key(leaf_id) {
                        return Err(invalid_mutation(&format!("references missing lane target {leaf_id}")));
                    }
                }
                self.sequence = seq;
                let log_leaf = leaf_id.clone();
                self.lanes.insert(lane.clone(), leaf_id);
                self.log.push(LogItem::Lane {
                    seq,
                    lane,
                    leaf_id: log_leaf,
                });
            }
            MutKind::NameFact { seq, name } => {
                self.sequence = seq;
                self.name = name.clone();
                self.log.push(LogItem::NameFact { seq, name });
            }
            MutKind::LabelFact {
                seq,
                target_id,
                label,
            } => {
                if !self.entries_by_id.contains_key(&target_id) {
                    return Err(invalid_mutation(&format!("references missing label target {target_id}")));
                }
                self.sequence = seq;
                match &label {
                    Some(label) => {
                        self.labels.insert(target_id.clone(), label.clone());
                    }
                    None => {
                        self.labels.remove(&target_id);
                    }
                }
                self.log.push(LogItem::LabelFact {
                    seq,
                    target_id,
                    label,
                });
            }
        }
        Ok(())
    }

    pub fn get_entry(&self, id: &str) -> Option<&Entry> {
        self.entries_by_id.get(id)
    }

    pub fn find_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>, SessionError> {
        assert_valid_limit(query.limit)?;
        assert_valid_cursor(query.cursor.as_ref().map(|c| c.after_seq))?;
        let mut results = Vec::new();
        for entry in order_entries(&self.entries, query.order.as_ref()) {
            if !matches_entry_query(&entry, query) {
                continue;
            }
            results.push(entry);
            if let Some(limit) = query.limit {
                if results.len() as f64 == limit {
                    break;
                }
            }
        }
        Ok(results)
    }

    pub fn find_entries_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &BranchBounds,
        start: &str,
    ) -> Result<Vec<Entry>, SessionError> {
        assert_valid_limit(query.limit)?;
        assert_valid_cursor(query.cursor.as_ref().map(|c| c.after_seq))?;
        let mut results = Vec::new();
        let path = self.walk_to_root(Some(start), Some(bounds))?;
        if query.order.as_ref() == Some(&EntryOrder::OldestFirst) {
            for entry in path.into_iter().rev() {
                let reached_bound = bounds
                    .stop_at_id
                    .as_deref()
                    .is_some_and(|id| entry.id() == id)
                    || bounds
                        .stop_at_type
                        .as_deref()
                        .is_some_and(|type_| entry.type_name() == type_);
                if matches_entry_query(&entry, query) {
                    results.push(entry);
                }
                if reached_bound {
                    break;
                }
                if let Some(limit) = query.limit {
                    if results.len() as f64 == limit {
                        break;
                    }
                }
            }
        } else {
            for entry in path {
                if matches_entry_query(&entry, query) {
                    results.push(entry);
                }
                if let Some(limit) = query.limit {
                    if results.len() as f64 == limit {
                        break;
                    }
                }
            }
        }
        Ok(results)
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
        let mut results = Vec::new();
        for record in order_records(&self.records, query.order.as_ref()) {
            if !matches_record_query(&record, query) {
                continue;
            }
            results.push(record);
            if let Some(limit) = query.limit {
                if results.len() as f64 == limit {
                    break;
                }
            }
        }
        Ok(results)
    }

    pub fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<f64>,
    ) -> Result<Vec<OperationStartedRecord>, SessionError> {
        assert_valid_limit(limit)?;
        let open = self.open_operations_by_lane.get(lane);
        let mut operations: Vec<OperationStartedRecord> = match open {
            Some(open) => open.values().cloned().collect(),
            None => Vec::new(),
        };
        operations.reverse(); // newest first (insertion order is seq order)
        match limit {
            Some(limit) => Ok(operations.into_iter().take(limit as usize).collect()),
            None => Ok(operations),
        }
    }

    pub fn get_log(&self, options: &LogOptions) -> Result<Vec<LogItem>, SessionError> {
        assert_valid_limit(options.limit)?;
        assert_valid_cursor(options.after_seq)?;
        let mut results = Vec::new();
        for item in &self.log {
            let seq = match item {
                LogItem::Entry { seq, .. }
                | LogItem::Record { seq, .. }
                | LogItem::Lane { seq, .. }
                | LogItem::NameFact { seq, .. }
                | LogItem::LabelFact { seq, .. } => *seq,
            };
            if options.after_seq.is_some_and(|after| seq <= after) {
                continue;
            }
            results.push(item.clone());
            if let Some(limit) = options.limit {
                if results.len() as f64 == limit {
                    break;
                }
            }
        }
        Ok(results)
    }

    pub fn get_name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn get_label(&self, id: &str) -> Option<&str> {
        self.labels.get(id).map(String::as_str)
    }

    pub fn get_stats(&self) -> &SessionStats {
        &self.stats
    }

    /// Mutations that reproduce this state in a fork.
    pub fn create_fork_mutations(&self, options: &ForkOptions) -> Result<Vec<SessionMutation>, SessionError> {
        let copied_entries: Vec<Entry>;
        let fork_lanes: Vec<LanePointer>;
        if matches!(options, ForkOptions::Tree) {
            copied_entries = self.find_entries(&EntryQuery {
                order: Some(EntryOrder::OldestFirst),
                ..EntryQuery::default()
            })?;
            fork_lanes = self.get_lanes();
        } else {
            let ForkOptions::Branch { entry_id, position } = options else {
                unreachable!()
            };
            let selected_entry_id = match entry_id {
                Some(id) => Some(id.clone()),
                None => self.require_lane("main")?,
            };
            let mut target_id: Option<String> = None;
            if let Some(selected_entry_id) = &selected_entry_id {
                let entry = self.entries_by_id.get(selected_entry_id);
                let Some(entry) = entry else {
                    return Err(SessionError::new(
                        "invalid_fork_target",
                        format!("Fork target is not a message entry: {selected_entry_id}"),
                    ));
                };
                if entry.type_name() != "message" {
                    return Err(SessionError::new(
                        "invalid_fork_target",
                        format!("Fork target is not a message entry: {selected_entry_id}"),
                    ));
                }
                let position = match position {
                    Some(position) => position.clone(),
                    None => {
                        if entry_id.is_none() {
                            "at".to_string()
                        } else {
                            "before".to_string()
                        }
                    }
                };
                target_id = if position == "at" {
                    Some(entry.id().to_string())
                } else {
                    entry.parent_id().map(|id| id.to_string())
                };
            }
            copied_entries = match &target_id {
                None => Vec::new(),
                Some(target_id) => self.find_entries_on_branch(
                    &EntryQuery {
                        order: Some(EntryOrder::OldestFirst),
                        ..EntryQuery::default()
                    },
                    &BranchBounds::default(),
                    target_id,
                )?,
            };
            fork_lanes = vec![LanePointer {
                lane: "main".to_string(),
                leaf_id: target_id,
            }];
        }

        let mut mutations: Vec<SessionMutation> = Vec::new();
        let mut sequence = 1.0f64;
        for mut source_entry in copied_entries {
            source_entry.set_parent_id(None);
            source_entry.set_seq(sequence);
            mutations.push(MutKind::Entry {
                lane: None,
                entry: source_entry,
            });
            sequence += 1.0;
        }
        for pointer in fork_lanes {
            mutations.push(MutKind::Lane {
                seq: sequence,
                lane: pointer.lane,
                leaf_id: pointer.leaf_id,
            });
            sequence += 1.0;
        }
        if let Some(name) = &self.name {
            mutations.push(MutKind::NameFact {
                seq: sequence,
                name: Some(name.clone()),
            });
            sequence += 1.0;
        }
        for entry in self.entries.clone() {
            if let Some(label) = self.labels.get(entry.id()) {
                mutations.push(MutKind::LabelFact {
                    seq: sequence,
                    target_id: entry.id().to_string(),
                    label: Some(label.clone()),
                });
                sequence += 1.0;
            }
        }
        Ok(mutations)
    }

    fn walk_to_root(
        &self,
        start: Option<&str>,
        bounds: Option<&BranchBounds>,
    ) -> Result<Vec<Entry>, SessionError> {
        let Some(start) = start else {
            return Ok(Vec::new());
        };
        let mut visited = HashSet::new();
        let mut current = self
            .entries_by_id
            .get(start)
            .ok_or_else(|| SessionError::new("not_found", format!("Entry not found: {start}")))?;
        let mut result = Vec::new();
        loop {
            if visited.contains(current.id()) {
                return Err(SessionError::new(
                    "invalid_entry",
                    format!("Session branch contains a cycle at {}", current.id()),
                ));
            }
            visited.insert(current.id().to_string());
            result.push(current.clone());
            if current.id() == bounds.and_then(|b| b.stop_at_id.as_deref()).unwrap_or("")
                || bounds
                    .and_then(|b| b.stop_at_type.as_deref())
                    .is_some_and(|type_| current.type_name() == type_)
                || current.parent_id().is_none()
            {
                break;
            }
            let parent_id = current.parent_id().expect("checked above").to_string();
            current = self.entries_by_id.get(&parent_id).ok_or_else(|| {
                SessionError::new("invalid_entry", format!("Entry not found: {parent_id}"))
            })?;
        }
        Ok(result)
    }
}

fn order_entries(entries: &[Entry], order: Option<&EntryOrder>) -> Vec<Entry> {
    let mut indices: Vec<usize> = (0..entries.len()).collect();
    if order != Some(&EntryOrder::OldestFirst) {
        indices.reverse();
    }
    indices.into_iter().map(|i| entries[i].clone()).collect()
}

fn order_records(records: &[LaneRecord], order: Option<&EntryOrder>) -> Vec<LaneRecord> {
    let mut indices: Vec<usize> = (0..records.len()).collect();
    if order != Some(&EntryOrder::OldestFirst) {
        indices.reverse();
    }
    indices.into_iter().map(|i| records[i].clone()).collect()
}

fn matches_entry_query(entry: &Entry, query: &EntryQuery) -> bool {
    let type_matches = query
        .type_
        .as_deref()
        .is_none_or(|type_| entry.type_name() == type_);
    let custom_matches = query.custom_type.as_deref().is_none_or(|custom_type| {
        entry.type_name() == "custom" && entry.custom_type() == Some(custom_type)
    });
    let cursor_matches = match &query.cursor {
        None => true,
        Some(cursor) => {
            if query.order.as_ref() == Some(&EntryOrder::OldestFirst) {
                entry.seq() > cursor.after_seq
            } else {
                entry.seq() < cursor.after_seq
            }
        }
    };
    type_matches && custom_matches && cursor_matches
}

fn matches_record_query(record: &LaneRecord, query: &RecordQuery) -> bool {
    let lane_matches = query.lane.as_deref().is_none_or(|lane| record.lane() == lane);
    let type_matches = query.type_.as_deref().is_none_or(|type_| record.type_name() == type_);
    let run_matches = query.run_id.as_deref().is_none_or(|run_id| match record {
        LaneRecord::OperationStarted(started) => started.base.id == run_id,
        other => other.run_id() == Some(run_id),
    });
    let kind_matches = query.operation_kind.as_deref().is_none_or(|kind| {
        matches!(
            record,
            LaneRecord::OperationStarted(started) if started.intent_kind() == kind
        )
    });
    let after_matches = query
        .after_seq
        .is_none_or(|after_seq| record.seq() > after_seq);
    lane_matches && type_matches && run_matches && kind_matches && after_matches
}
