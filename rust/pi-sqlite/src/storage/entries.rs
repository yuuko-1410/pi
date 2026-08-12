//! Entries table access, port of
//! `packages/session-backends/sqlite-node/src/sqlite/storage/entries.ts`.

use crate::database::{query_all, query_get, query_run, SqliteDatabase};
use crate::sql::SqlPart;

#[derive(Clone, Debug, PartialEq)]
pub struct EntryRow {
    pub session_id: String,
    pub seq: f64,
    pub id: String,
    pub parent_id: Option<String>,
    pub type_: String,
    pub timestamp: f64,
    pub payload: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewEntryRow {
    pub seq: f64,
    pub id: String,
    pub parent_id: Option<String>,
    pub type_: String,
    pub timestamp: f64,
    pub payload: String,
}

pub fn insert_entry_row(
    db: &dyn SqliteDatabase,
    session_id: &str,
    entry: &NewEntryRow,
) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "INSERT INTO entries (session_id, id, seq, parent_id, type, timestamp, payload) VALUES (",
            SqlPart::Value(session_id.into()),
            ", ",
            SqlPart::Value(entry.id.clone().into()),
            ", ",
            SqlPart::Value(entry.seq.into()),
            ", ",
            SqlPart::Value(entry.parent_id.clone().into()),
            ", ",
            SqlPart::Value(entry.type_.clone().into()),
            ", ",
            SqlPart::Value(entry.timestamp.into()),
            ", ",
            SqlPart::Value(entry.payload.clone().into()),
            ")"
        },
    )
    .map(|_| ())
}

pub fn read_entry_row(
    db: &dyn SqliteDatabase,
    session_id: &str,
    entry_id: &str,
) -> Result<Option<EntryRow>, String> {
    let row = query_get(
        db,
        &crate::sql! {
            "SELECT session_id, seq, id, parent_id, type, timestamp, payload FROM entries WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND id = ",
            SqlPart::Value(entry_id.into())
        },
    )?;
    Ok(row.map(|row| entry_row_from_row(&row)))
}

fn entry_row_from_row(row: &crate::database::SqliteRow) -> EntryRow {
    EntryRow {
        session_id: row.get_str("session_id").unwrap_or("").to_string(),
        seq: row.get_f64("seq").unwrap_or(0.0),
        id: row.get_str("id").unwrap_or("").to_string(),
        parent_id: row.get_str("parent_id").map(|value| value.to_string()),
        type_: row.get_str("type").unwrap_or("").to_string(),
        timestamp: row.get_f64("timestamp").unwrap_or(0.0),
        payload: row.get_str("payload").unwrap_or("").to_string(),
    }
}

#[derive(Clone, Debug, Default)]
pub struct ReadEntryRowsOptions {
    pub after_seq: Option<f64>,
    pub cursor: Option<f64>,
    pub type_: Option<String>,
    pub oldest_first: Option<bool>,
    pub limit: Option<f64>,
}

pub fn read_entry_rows(
    db: &dyn SqliteDatabase,
    session_id: &str,
    options: &ReadEntryRowsOptions,
) -> Result<Vec<EntryRow>, String> {
    let oldest_first = options.oldest_first == Some(true);
    let mut where_parts: Vec<SqlPart> = vec!["session_id = ".into(), SqlPart::Value(session_id.into())];
    if let Some(after_seq) = options.after_seq {
        where_parts.push(" AND seq > ".into());
        where_parts.push(SqlPart::Value(after_seq.into()));
    }
    if let Some(cursor) = options.cursor {
        where_parts.push(if oldest_first {
            " AND seq > ".into()
        } else {
            " AND seq < ".into()
        });
        where_parts.push(SqlPart::Value(cursor.into()));
    }
    if let Some(type_) = &options.type_ {
        where_parts.push(" AND type = ".into());
        where_parts.push(SqlPart::Value(type_.clone().into()));
    }
    let direction: SqlPart = if oldest_first { "ASC".into() } else { "DESC".into() };
    let mut parts: Vec<SqlPart> = vec![
        "SELECT session_id, seq, id, parent_id, type, timestamp, payload FROM entries WHERE ".into(),
    ];
    parts.extend(where_parts);
    parts.push(" ORDER BY seq ".into());
    parts.push(direction);
    if let Some(limit) = options.limit {
        parts.push(" LIMIT ".into());
        parts.push(SqlPart::Value(limit.into()));
    }
    let query = crate::sql::build_sql_query(&parts);
    let rows = query_all(db, &query)?;
    Ok(rows.iter().map(entry_row_from_row).collect())
}

pub fn id_exists_in_entries(db: &dyn SqliteDatabase, session_id: &str, id: &str) -> Result<bool, String> {
    let row = query_get(
        db,
        &crate::sql! {
            "SELECT 1 AS found FROM entries WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND id = ",
            SqlPart::Value(id.into()),
            " LIMIT 1"
        },
    )?;
    Ok(row.is_some())
}

pub fn delete_entry_rows(db: &dyn SqliteDatabase, session_id: &str) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "DELETE FROM entries WHERE session_id = ",
            SqlPart::Value(session_id.into())
        },
    )
    .map(|_| ())
}
