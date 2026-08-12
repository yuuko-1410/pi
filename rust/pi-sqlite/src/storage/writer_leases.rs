//! Writer lease table access, port of
//! `packages/session-backends/sqlite-node/src/sqlite/storage/writer-leases.ts`.

use crate::database::{query_get, query_run, SqliteDatabase};
use crate::sql::SqlPart;

#[derive(Clone, Debug, PartialEq)]
pub struct WriterLease {
    pub owner_id: String,
    pub fence: f64,
    pub expires_at_ms: f64,
}

/// Claim or steal the writer lease; returns None when the current lease is
/// unexpired (JS `acquireWriterLease`).
pub fn acquire_writer_lease(
    db: &dyn SqliteDatabase,
    session_id: &str,
    owner_id: &str,
    now: f64,
    expires_at_ms: f64,
) -> Result<Option<WriterLease>, String> {
    let row = query_get(
        db,
        &crate::sql! {
            "INSERT INTO writer_leases (session_id, owner_id, fence, expires_at_ms) VALUES (",
            SqlPart::Value(session_id.into()),
            ", ",
            SqlPart::Value(owner_id.into()),
            ", 1, ",
            SqlPart::Value(expires_at_ms.into()),
            ") ON CONFLICT(session_id) DO UPDATE SET owner_id = excluded.owner_id, fence = writer_leases.fence + 1, expires_at_ms = excluded.expires_at_ms WHERE writer_leases.expires_at_ms <= ",
            SqlPart::Value(now.into()),
            " RETURNING owner_id, fence, expires_at_ms"
        },
    )?;
    Ok(row.map(|row| WriterLease {
        owner_id: row.get_str("owner_id").unwrap_or("").to_string(),
        fence: row.get_f64("fence").unwrap_or(0.0),
        expires_at_ms: row.get_f64("expires_at_ms").unwrap_or(0.0),
    }))
}

/// Renew the lease if it is still owned with the same fence and unexpired;
/// mutates the lease on success (JS `renewWriterLease`).
pub fn renew_writer_lease(
    db: &dyn SqliteDatabase,
    session_id: &str,
    lease: &mut WriterLease,
    now: f64,
    expires_at_ms: f64,
) -> Result<bool, String> {
    let result = query_run(
        db,
        &crate::sql! {
            "UPDATE writer_leases SET expires_at_ms = ",
            SqlPart::Value(expires_at_ms.into()),
            " WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND owner_id = ",
            SqlPart::Value(lease.owner_id.clone().into()),
            " AND fence = ",
            SqlPart::Value(lease.fence.into()),
            " AND expires_at_ms > ",
            SqlPart::Value(now.into())
        },
    )?;
    if result.changes == 1 {
        lease.expires_at_ms = expires_at_ms;
    }
    Ok(result.changes == 1)
}

pub fn release_writer_lease(
    db: &dyn SqliteDatabase,
    session_id: &str,
    lease: &WriterLease,
) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "DELETE FROM writer_leases WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND owner_id = ",
            SqlPart::Value(lease.owner_id.clone().into()),
            " AND fence = ",
            SqlPart::Value(lease.fence.into())
        },
    )
    .map(|_| ())
}

pub fn delete_writer_lease(db: &dyn SqliteDatabase, session_id: &str) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "DELETE FROM writer_leases WHERE session_id = ",
            SqlPart::Value(session_id.into())
        },
    )
    .map(|_| ())
}
