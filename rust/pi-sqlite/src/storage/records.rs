//! Records table access, port of
//! `packages/session-backends/sqlite-node/src/sqlite/storage/records.ts`.

use pi_agent_core::harness::session_types::SessionError;

use crate::database::{query_all, query_get, query_run, query_exec, SqliteDatabase};
use crate::sql::{join_sql_fragments, SqlPart, SqlQuery};

#[derive(Clone, Debug, PartialEq)]
pub struct RecordRow {
    pub session_id: String,
    pub seq: f64,
    pub id: String,
    pub lane: String,
    pub run_id: Option<String>,
    pub type_: String,
    pub op_kind: Option<String>,
    pub timestamp: f64,
    pub payload: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewRecordRow {
    pub seq: f64,
    pub id: String,
    pub lane: String,
    pub run_id: Option<String>,
    pub type_: String,
    pub op_kind: Option<String>,
    pub timestamp: f64,
    pub payload: String,
}

fn record_row_from_row(row: &crate::database::SqliteRow) -> RecordRow {
    RecordRow {
        session_id: row.get_str("session_id").unwrap_or("").to_string(),
        seq: row.get_f64("seq").unwrap_or(0.0),
        id: row.get_str("id").unwrap_or("").to_string(),
        lane: row.get_str("lane").unwrap_or("").to_string(),
        run_id: row.get_str("run_id").map(|value| value.to_string()),
        type_: row.get_str("type").unwrap_or("").to_string(),
        op_kind: row.get_str("op_kind").map(|value| value.to_string()),
        timestamp: row.get_f64("timestamp").unwrap_or(0.0),
        payload: row.get_str("payload").unwrap_or("").to_string(),
    }
}

pub fn append_record_row(
    db: &dyn SqliteDatabase,
    session_id: &str,
    record: &NewRecordRow,
) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "INSERT INTO records (session_id, seq, id, lane, run_id, type, op_kind, timestamp, payload) VALUES (",
            SqlPart::Value(session_id.into()),
            ", ",
            SqlPart::Value(record.seq.into()),
            ", ",
            SqlPart::Value(record.id.clone().into()),
            ", ",
            SqlPart::Value(record.lane.clone().into()),
            ", ",
            SqlPart::Value(record.run_id.clone().into()),
            ", ",
            SqlPart::Value(record.type_.clone().into()),
            ", ",
            SqlPart::Value(record.op_kind.clone().into()),
            ", ",
            SqlPart::Value(record.timestamp.into()),
            ", ",
            SqlPart::Value(record.payload.clone().into()),
            ")"
        },
    )
    .map(|_| ())
}

pub fn id_exists_in_records(db: &dyn SqliteDatabase, session_id: &str, id: &str) -> Result<bool, String> {
    let row = query_get(
        db,
        &crate::sql! {
            "SELECT 1 AS found FROM records WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND id = ",
            SqlPart::Value(id.into()),
            " LIMIT 1"
        },
    )?;
    Ok(row.is_some())
}

pub fn delete_record_rows(db: &dyn SqliteDatabase, session_id: &str) -> Result<(), String> {
    query_exec(
        db,
        &crate::sql! {
            "DELETE FROM records WHERE session_id = ",
            SqlPart::Value(session_id.into())
        },
    )
}

#[derive(Clone, Debug, Default)]
pub struct ReadRecordRowsOptions {
    pub lane: Option<String>,
    pub type_: Option<String>,
    pub run_id: Option<String>,
    pub operation_kind: Option<String>,
    pub after_seq: Option<f64>,
    pub oldest_first: Option<bool>,
    pub limit: Option<f64>,
}

pub fn read_record_rows(
    db: &dyn SqliteDatabase,
    session_id: &str,
    query: &ReadRecordRowsOptions,
) -> Result<Vec<RecordRow>, String> {
    let mut predicates: Vec<SqlQuery> = vec![SqlQuery::new(
        "session_id = ?".to_string(),
        vec![session_id.into()],
    )];
    if let Some(lane) = &query.lane {
        predicates.push(SqlQuery::new("lane = ?".to_string(), vec![lane.clone().into()]));
    }
    if let Some(type_) = &query.type_ {
        predicates.push(SqlQuery::new("type = ?".to_string(), vec![type_.clone().into()]));
    }
    if let Some(run_id) = &query.run_id {
        predicates.push(SqlQuery::new("run_id = ?".to_string(), vec![run_id.clone().into()]));
    }
    if let Some(operation_kind) = &query.operation_kind {
        predicates.push(SqlQuery::new(
            "op_kind = ?".to_string(),
            vec![operation_kind.clone().into()],
        ));
    }
    if let Some(after_seq) = query.after_seq {
        predicates.push(SqlQuery::new("seq > ?".to_string(), vec![after_seq.into()]));
    }
    let direction: &str = if query.oldest_first == Some(true) { "ASC" } else { "DESC" };
    let mut parts: Vec<SqlPart> = vec![
        "SELECT session_id, seq, id, lane, run_id, type, op_kind, timestamp, payload FROM records WHERE ".into(),
        SqlPart::Query(join_sql_fragments(&predicates, " AND ")),
        format!(" ORDER BY seq {direction}").into(),
    ];
    if let Some(limit) = query.limit {
        parts.push(" LIMIT ".into());
        parts.push(SqlPart::Value(limit.into()));
    }
    let built = crate::sql::build_sql_query(&parts);
    let rows = query_all(db, &built)?;
    Ok(rows.iter().map(record_row_from_row).collect())
}

/// Read the open operation record for a lane (JS `readOpenOperationRows`).
pub fn read_open_operation_rows(
    db: &dyn SqliteDatabase,
    session_id: &str,
    lane: &str,
) -> Result<Vec<RecordRow>, SessionError> {
    let lane_row = query_get(
        db,
        &crate::sql! {
            "SELECT open_operation_id FROM lanes WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND lane = ",
            SqlPart::Value(lane.into())
        },
    )
    .map_err(|message| SessionError::new("storage", format!("{message}")))?;
    let Some(lane_row) = lane_row else { return Ok(vec![]) };
    let Some(open_operation_id) = lane_row.get_str("open_operation_id").map(|value| value.to_string()) else {
        return Ok(vec![]);
    };

    let record = query_get(
        db,
        &crate::sql! {
            "SELECT session_id, seq, id, lane, run_id, type, op_kind, timestamp, payload FROM records WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND id = ",
            SqlPart::Value(open_operation_id.clone().into())
        },
    )
    .map_err(|message| SessionError::new("storage", format!("{message}")))?;
    let Some(record) = record else {
        return Err(SessionError::new(
            "storage",
            format!("Lane {lane} points at missing open operation {open_operation_id}"),
        ));
    };
    let row = record_row_from_row(&record);
    if row.lane != lane || row.type_ != "operation_started" {
        return Err(SessionError::new(
            "storage",
            format!("Lane {lane} points at invalid open operation {open_operation_id}"),
        ));
    }
    Ok(vec![row])
}
