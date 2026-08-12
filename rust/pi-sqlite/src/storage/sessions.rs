//! Sessions table access, port of
//! `packages/session-backends/sqlite-node/src/sqlite/storage/sessions.ts`.

use pi_agent_core::harness::session_types::SessionError;
use pi_protocol::cbor::Value;

use crate::database::{query_all, query_get, query_run, SqliteDatabase};
use crate::sql::SqlPart;

#[derive(Clone, Debug, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub created_at: f64,
    pub metadata: Option<String>,
    pub cwd: String,
    pub parent_session_id: Option<String>,
    pub has_session_name: bool,
    pub session_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewSessionRow {
    pub id: String,
    pub created_at: f64,
    pub cwd: String,
    pub parent_session_id: Option<String>,
    pub metadata: Option<Value>,
}

/// SQLite session metadata with name projection.
#[derive(Clone, Debug, PartialEq)]
pub struct SqliteSessionMetadata {
    pub id: String,
    pub created_at: f64,
    pub name: Option<String>,
    pub cwd: String,
    pub path: String,
    pub parent_session_id: Option<String>,
    pub metadata: Option<Value>,
}

fn parse_metadata(metadata: Option<&str>, session_id: &str) -> Result<Option<Value>, SessionError> {
    let Some(metadata) = metadata else { return Ok(None) };
    let parsed: Value = pi_ai::utils::json::parse_json_with_repair(metadata)
        .map_err(|_| SessionError::new("storage", format!("Invalid SQLite session {session_id}: metadata is not valid JSON")))?;
    if !matches!(parsed, Value::Map(_)) {
        return Err(SessionError::new(
            "storage",
            format!("Invalid SQLite session {session_id}: metadata must be an object"),
        ));
    }
    Ok(Some(parsed))
}

pub fn session_exists(db: &dyn SqliteDatabase, session_id: &str) -> Result<bool, String> {
    let row = query_get(
        db,
        &crate::sql! {
            "SELECT 1 AS found FROM sessions WHERE id = ",
            SqlPart::Value(session_id.into())
        },
    )?;
    Ok(row.is_some())
}

fn serialize_metadata(metadata: Option<&Value>) -> Result<Option<String>, SessionError> {
    let Some(metadata) = metadata else { return Ok(None) };
    if !matches!(metadata, Value::Map(_)) {
        return Err(SessionError::new(
            "invalid_payload",
            "SQLite session metadata must be an object",
        ));
    }
    Ok(Some(pi_ai::utils::json::json_stringify(metadata)))
}

pub fn insert_session_row(db: &dyn SqliteDatabase, session: &NewSessionRow) -> Result<(), SessionError> {
    let metadata = serialize_metadata(session.metadata.as_ref())?;
    query_run(
        db,
        &crate::sql! {
            "INSERT INTO sessions (id, created_at, metadata, cwd, parent_session_id) VALUES (",
            SqlPart::Value(session.id.clone().into()),
            ", ",
            SqlPart::Value(session.created_at.into()),
            ", ",
            SqlPart::Value(metadata.clone().into()),
            ", ",
            SqlPart::Value(session.cwd.clone().into()),
            ", ",
            SqlPart::Value(session.parent_session_id.clone().into()),
            ")"
        },
    )
    .map_err(|message| SessionError::new("storage", message))?;
    Ok(())
}

const SESSION_SELECT: &str = "SELECT s.id, s.created_at, s.metadata, s.cwd, s.parent_session_id, name_fact.seq IS NOT NULL AS has_session_name, name_fact.value AS session_name FROM sessions AS s LEFT JOIN facts AS name_fact ON name_fact.session_id = s.id AND name_fact.kind = 'name' AND name_fact.key IS NULL AND name_fact.seq = (SELECT MAX(f.seq) FROM facts AS f WHERE f.session_id = s.id AND f.kind = 'name' AND f.key IS NULL)";

fn session_row_from_row(row: &crate::database::SqliteRow) -> SessionRow {
    SessionRow {
        id: row.get_str("id").unwrap_or("").to_string(),
        created_at: row.get_f64("created_at").unwrap_or(0.0),
        metadata: row.get_str("metadata").map(|value| value.to_string()),
        cwd: row.get_str("cwd").unwrap_or("").to_string(),
        parent_session_id: row.get_str("parent_session_id").map(|value| value.to_string()),
        has_session_name: row.get_i64("has_session_name") == Some(1),
        session_name: row.get_str("session_name").map(|value| value.to_string()),
    }
}

pub fn read_session_row(db: &dyn SqliteDatabase, session_id: &str) -> Result<Option<SessionRow>, String> {
    let row = query_get(
        db,
        &crate::sql! {
            SESSION_SELECT,
            " WHERE s.id = ",
            SqlPart::Value(session_id.into())
        },
    )?;
    Ok(row.map(|row| session_row_from_row(&row)))
}

pub fn read_session_rows(
    db: &dyn SqliteDatabase,
    cwd: Option<&str>,
) -> Result<Vec<SessionRow>, String> {
    let mut parts: Vec<SqlPart> = vec![SESSION_SELECT.into()];
    if let Some(cwd) = cwd {
        parts.push(" WHERE s.cwd = ".into());
        parts.push(SqlPart::Value(cwd.into()));
    }
    parts.push(" ORDER BY s.created_at DESC".into());
    let query = crate::sql::build_sql_query(&parts);
    let rows = query_all(db, &query)?;
    Ok(rows.iter().map(session_row_from_row).collect())
}

pub fn delete_session_row(db: &dyn SqliteDatabase, session_id: &str) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "DELETE FROM sessions WHERE id = ",
            SqlPart::Value(session_id.into())
        },
    )
    .map(|_| ())
}

fn parse_session_name(value: Option<&str>, session_id: &str) -> Result<Option<String>, SessionError> {
    let Some(value) = value else { return Ok(None) };
    let parsed: Value = pi_ai::utils::json::parse_json_with_repair(value)
        .map_err(|_| SessionError::new("storage", format!("Invalid SQLite session {session_id}: name is not valid JSON")))?;
    match parsed {
        Value::String(name) => Ok(Some(name)),
        _ => Err(SessionError::new(
            "storage",
            format!("Invalid SQLite session {session_id}: name must be a string"),
        )),
    }
}

/// Decode a session row into metadata (JS `decodeSessionMetadata`).
pub fn decode_session_metadata(row: &SessionRow, path: &str) -> Result<SqliteSessionMetadata, SessionError> {
    let metadata = parse_metadata(row.metadata.as_deref(), &row.id)?;
    let name = if row.has_session_name {
        parse_session_name(row.session_name.as_deref(), &row.id)?
    } else {
        None
    };
    Ok(SqliteSessionMetadata {
        id: row.id.clone(),
        created_at: row.created_at,
        name,
        cwd: row.cwd.clone(),
        path: path.to_string(),
        parent_session_id: row.parent_session_id.clone(),
        metadata,
    })
}
