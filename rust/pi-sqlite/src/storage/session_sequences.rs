//! Session sequence table access, port of
//! `packages/session-backends/sqlite-node/src/sqlite/storage/session-sequences.ts`.

use pi_agent_core::harness::session_types::SessionError;

use crate::database::{query_get, query_run, SqliteDatabase};
use crate::sql::SqlPart;

pub fn create_sequence(
    db: &dyn SqliteDatabase,
    session_id: &str,
    next_seq: f64,
) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "INSERT INTO session_sequences (session_id, next_seq) VALUES (",
            SqlPart::Value(session_id.into()),
            ", ",
            SqlPart::Value(next_seq.into()),
            ")"
        },
    )
    .map(|_| ())
}

pub fn get_next_sequence(db: &dyn SqliteDatabase, session_id: &str) -> Result<f64, SessionError> {
    let row = query_get(
        db,
        &crate::sql! {
            "SELECT next_seq FROM session_sequences WHERE session_id = ",
            SqlPart::Value(session_id.into())
        },
    )
    .map_err(|message| SessionError::new("storage", format!("{message}")))?;
    match row {
        Some(row) => Ok(row.get_f64("next_seq").unwrap_or(0.0)),
        None => Err(SessionError::new(
            "storage",
            format!("Missing sequence row for session {session_id}"),
        )),
    }
}

pub fn set_next_sequence(
    db: &dyn SqliteDatabase,
    session_id: &str,
    next_seq: f64,
) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "UPDATE session_sequences SET next_seq = ",
            SqlPart::Value(next_seq.into()),
            " WHERE session_id = ",
            SqlPart::Value(session_id.into())
        },
    )
    .map(|_| ())
}

pub fn advance_sequence(db: &dyn SqliteDatabase, session_id: &str, seq: f64) -> Result<(), String> {
    set_next_sequence(db, session_id, seq + 1.0)
}

pub fn delete_sequence(db: &dyn SqliteDatabase, session_id: &str) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "DELETE FROM session_sequences WHERE session_id = ",
            SqlPart::Value(session_id.into())
        },
    )
    .map(|_| ())
}
