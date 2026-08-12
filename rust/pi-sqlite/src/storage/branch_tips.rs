//! Branch tips table access, port of
//! `packages/session-backends/sqlite-node/src/sqlite/storage/branch-tips.ts`.

use crate::database::{query_all, query_get, query_run, query_exec, SqliteDatabase};
use crate::sql::SqlPart;

pub fn read_branch_tip_ids(db: &dyn SqliteDatabase, session_id: &str) -> Result<Vec<String>, String> {
    let rows = query_all(
        db,
        &crate::sql! {
            "SELECT tip_id FROM branch_tips WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " ORDER BY tip_id"
        },
    )?;
    Ok(rows
        .iter()
        .filter_map(|row| row.get_str("tip_id").map(|value| value.to_string()))
        .collect())
}

pub fn read_branch_tip_branch_id(
    db: &dyn SqliteDatabase,
    session_id: &str,
    tip_id: &str,
) -> Result<Option<String>, String> {
    let row = query_get(
        db,
        &crate::sql! {
            "SELECT branch_id FROM branch_tips WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND tip_id = ",
            SqlPart::Value(tip_id.into())
        },
    )?;
    Ok(row.and_then(|row| row.get_str("branch_id").map(|value| value.to_string())))
}

pub fn insert_branch_tip(
    db: &dyn SqliteDatabase,
    session_id: &str,
    tip_id: &str,
    branch_id: &str,
) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "INSERT INTO branch_tips (session_id, tip_id, branch_id) VALUES (",
            SqlPart::Value(session_id.into()),
            ", ",
            SqlPart::Value(tip_id.into()),
            ", ",
            SqlPart::Value(branch_id.into()),
            ")"
        },
    )
    .map(|_| ())
}

/// Update the tip of a branch; returns whether the old tip matched (JS
/// `updateBranchTip`).
pub fn update_branch_tip(
    db: &dyn SqliteDatabase,
    session_id: &str,
    branch_id: &str,
    old_tip_id: &str,
    new_tip_id: &str,
) -> Result<bool, String> {
    let result = query_run(
        db,
        &crate::sql! {
            "UPDATE branch_tips SET tip_id = ",
            SqlPart::Value(new_tip_id.into()),
            " WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND branch_id = ",
            SqlPart::Value(branch_id.into()),
            " AND tip_id = ",
            SqlPart::Value(old_tip_id.into())
        },
    )?;
    Ok(result.changes == 1)
}

pub fn delete_branch_tips(db: &dyn SqliteDatabase, session_id: &str) -> Result<(), String> {
    query_exec(
        db,
        &crate::sql! {
            "DELETE FROM branch_tips WHERE session_id = ",
            SqlPart::Value(session_id.into())
        },
    )
}
