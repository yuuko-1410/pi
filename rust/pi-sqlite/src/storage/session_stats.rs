//! Session stats table access, port of
//! `packages/session-backends/sqlite-node/src/sqlite/storage/session-stats.ts`.

use pi_agent_core::harness::session_types::{SessionError, SessionStats};
use pi_ai::types::Usage;

use crate::database::{query_get, query_run, SqliteDatabase};
use crate::sql::SqlPart;

pub fn create_stats(
    db: &dyn SqliteDatabase,
    session_id: &str,
    message_count: f64,
) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "INSERT INTO session_stats (session_id, message_count, cached_tokens, uncached_tokens, total_tokens, cost_total) VALUES (",
            SqlPart::Value(session_id.into()),
            ", ",
            SqlPart::Value(message_count.into()),
            ", 0, 0, 0, 0)"
        },
    )
    .map(|_| ())
}

pub fn read_stats(db: &dyn SqliteDatabase, session_id: &str) -> Result<SessionStats, SessionError> {
    let row = query_get(
        db,
        &crate::sql! {
            "SELECT session_id, message_count, cached_tokens, uncached_tokens, total_tokens, cost_total FROM session_stats WHERE session_id = ",
            SqlPart::Value(session_id.into())
        },
    )
    .map_err(|message| SessionError::new("storage", format!("{message}")))?;
    match row {
        Some(row) => Ok(SessionStats {
            message_count: row.get_f64("message_count").unwrap_or(0.0),
            cached_tokens: row.get_f64("cached_tokens").unwrap_or(0.0),
            uncached_tokens: row.get_f64("uncached_tokens").unwrap_or(0.0),
            total_tokens: row.get_f64("total_tokens").unwrap_or(0.0),
            cost_total: row.get_f64("cost_total").unwrap_or(0.0),
        }),
        None => Err(SessionError::new(
            "storage",
            format!("Missing stats row for session {session_id}"),
        )),
    }
}

pub fn increment_message_count(db: &dyn SqliteDatabase, session_id: &str) -> Result<(), SessionError> {
    let result = query_run(
        db,
        &crate::sql! {
            "UPDATE session_stats SET message_count = message_count + 1 WHERE session_id = ",
            SqlPart::Value(session_id.into())
        },
    )
    .map_err(|message| SessionError::new("storage", format!("{message}")))?;
    if result.changes != 1 {
        return Err(SessionError::new(
            "storage",
            format!("Missing stats row for session {session_id}"),
        ));
    }
    Ok(())
}

pub fn add_usage_to_stats(
    db: &dyn SqliteDatabase,
    session_id: &str,
    usage: &Usage,
) -> Result<(), SessionError> {
    let result = query_run(
        db,
        &crate::sql! {
            "UPDATE session_stats SET cached_tokens = cached_tokens + ",
            SqlPart::Value(usage.cache_read.into()),
            ", uncached_tokens = uncached_tokens + ",
            SqlPart::Value((usage.input + usage.cache_write).into()),
            ", total_tokens = total_tokens + ",
            SqlPart::Value(usage.total_tokens.into()),
            ", cost_total = cost_total + ",
            SqlPart::Value(usage.cost.total.into()),
            " WHERE session_id = ",
            SqlPart::Value(session_id.into())
        },
    )
    .map_err(|message| SessionError::new("storage", format!("{message}")))?;
    if result.changes != 1 {
        return Err(SessionError::new(
            "storage",
            format!("Missing stats row for session {session_id}"),
        ));
    }
    Ok(())
}

pub fn delete_stats(db: &dyn SqliteDatabase, session_id: &str) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "DELETE FROM session_stats WHERE session_id = ",
            SqlPart::Value(session_id.into())
        },
    )
    .map(|_| ())
}
