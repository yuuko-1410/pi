//! Branch cache, port of
//! `packages/session-backends/sqlite-node/src/sqlite/storage/branch-entries.ts`
//! and `.../branch-cache.ts` (merged; tightly coupled).

use pi_agent_core::harness::session_types::SessionError;

use crate::database::{query_all, query_get, query_run, SqliteDatabase};
use crate::sql::{join_sql_fragments, SqlPart, SqlQuery};
use crate::storage::branch_tips::{delete_branch_tips, insert_branch_tip, read_branch_tip_branch_id, update_branch_tip};

#[derive(Clone, Debug, PartialEq)]
pub struct CachedBranch {
    pub branch_id: String,
    pub leaf_seq: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CachedBranchEntryRow {
    pub session_id: String,
    pub id: String,
    pub entry_seq: f64,
    pub parent_id: Option<String>,
    pub type_: String,
    pub timestamp: f64,
    pub payload: String,
}

#[derive(Clone, Debug, Default)]
pub struct CachedBranchQuery {
    pub type_: Option<String>,
    pub custom_type: Option<String>,
    pub stop_at_type: Option<String>,
    pub stop_at_id: Option<String>,
    pub cursor: Option<f64>,
    pub oldest_first: Option<bool>,
    pub limit: Option<f64>,
}

pub fn read_cached_branch(
    db: &dyn SqliteDatabase,
    session_id: &str,
    leaf_id: &str,
) -> Result<Option<CachedBranch>, String> {
    let row = query_get(
        db,
        &crate::sql! {
            "SELECT branch_id, entry_seq FROM branch_entries WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND entry_id = ",
            SqlPart::Value(leaf_id.into()),
            " ORDER BY branch_id LIMIT 1"
        },
    )?;
    Ok(row.map(|row| CachedBranch {
        branch_id: row.get_str("branch_id").unwrap_or("").to_string(),
        leaf_seq: row.get_f64("entry_seq").unwrap_or(0.0),
    }))
}

fn cached_branch_entry_from_row(row: &crate::database::SqliteRow) -> CachedBranchEntryRow {
    CachedBranchEntryRow {
        session_id: row.get_str("session_id").unwrap_or("").to_string(),
        id: row.get_str("id").unwrap_or("").to_string(),
        entry_seq: row.get_f64("entry_seq").unwrap_or(0.0),
        parent_id: row.get_str("parent_id").map(|value| value.to_string()),
        type_: row.get_str("type").unwrap_or("").to_string(),
        timestamp: row.get_f64("timestamp").unwrap_or(0.0),
        payload: row.get_str("payload").unwrap_or("").to_string(),
    }
}

pub fn query_cached_branch_rows(
    db: &dyn SqliteDatabase,
    session_id: &str,
    branch: &CachedBranch,
    query: &CachedBranchQuery,
) -> Result<Vec<CachedBranchEntryRow>, String> {
    let oldest_first = query.oldest_first == Some(true);
    let mut stop_predicates: Vec<SqlQuery> = Vec::new();
    if let Some(stop_at_type) = &query.stop_at_type {
        stop_predicates.push(SqlQuery::new("stop.entry_type = ?".to_string(), vec![stop_at_type.clone().into()]));
    }
    if let Some(stop_at_id) = &query.stop_at_id {
        stop_predicates.push(SqlQuery::new("stop.entry_id = ?".to_string(), vec![stop_at_id.clone().into()]));
    }

    let aggregate = if oldest_first { "MIN" } else { "MAX" };
    let boundary_comparison = if oldest_first { "<=" } else { ">=" };
    let cursor_comparison = if oldest_first { ">" } else { "<" };
    let direction = if oldest_first { "ASC" } else { "DESC" };

    let mut parts: Vec<SqlPart> = vec![
        "SELECT e.session_id, e.id, e.seq AS entry_seq, e.parent_id, e.type, e.timestamp, e.payload FROM branch_entries AS b JOIN entries AS e ON e.session_id = b.session_id AND e.id = b.entry_id WHERE b.session_id = ".into(),
        SqlPart::Value(session_id.into()),
        " AND b.branch_id = ".into(),
        SqlPart::Value(branch.branch_id.clone().into()),
        " AND b.entry_seq <= ".into(),
        SqlPart::Value(branch.leaf_seq.into()),
    ];
    if !stop_predicates.is_empty() {
        let boundary = SqlQuery::new(
            format!("SELECT {aggregate}(stop.entry_seq) FROM branch_entries AS stop WHERE stop.session_id = ? AND stop.branch_id = ? AND stop.entry_seq <= ? AND ({})", join_sql_fragments(&stop_predicates, " OR ").query_text),
            vec![
                session_id.into(),
                branch.branch_id.clone().into(),
                branch.leaf_seq.into(),
            ]
            .into_iter()
            .chain(join_sql_fragments(&stop_predicates, " OR ").params)
            .collect(),
        );
        let default = if oldest_first { branch.leaf_seq } else { 0.0 };
        parts.push(format!(" AND b.entry_seq {boundary_comparison} COALESCE((").into());
        parts.push(SqlPart::Query(boundary));
        parts.push(format!("), {default})").into());
    }
    if let Some(cursor) = query.cursor {
        parts.push(format!(" AND b.entry_seq {cursor_comparison} ").into());
        parts.push(SqlPart::Value(cursor.into()));
    }
    if let Some(type_) = &query.type_ {
        parts.push(" AND b.entry_type = ".into());
        parts.push(SqlPart::Value(type_.clone().into()));
    }
    if let Some(custom_type) = &query.custom_type {
        parts.push(" AND b.custom_type = ".into());
        parts.push(SqlPart::Value(custom_type.clone().into()));
    }
    parts.push(format!(" ORDER BY b.entry_seq {direction}").into());
    if let Some(limit) = query.limit {
        parts.push(" LIMIT ".into());
        parts.push(SqlPart::Value(limit.into()));
    }
    let built = crate::sql::build_sql_query(&parts);
    let rows = query_all(db, &built)?;
    Ok(rows.iter().map(cached_branch_entry_from_row).collect())
}

pub fn delete_branch_entries(db: &dyn SqliteDatabase, session_id: &str) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "DELETE FROM branch_entries WHERE session_id = ",
            SqlPart::Value(session_id.into())
        },
    )
    .map(|_| ())
}

pub fn insert_branch_entry(
    db: &dyn SqliteDatabase,
    session_id: &str,
    branch_id: &str,
    entry_id: &str,
    entry_seq: f64,
    entry_type: &str,
    custom_type: Option<&str>,
) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "INSERT INTO branch_entries (session_id, branch_id, entry_id, entry_seq, entry_type, custom_type) VALUES (",
            SqlPart::Value(session_id.into()),
            ", ",
            SqlPart::Value(branch_id.into()),
            ", ",
            SqlPart::Value(entry_id.into()),
            ", ",
            SqlPart::Value(entry_seq.into()),
            ", ",
            SqlPart::Value(entry_type.into()),
            ", ",
            SqlPart::Value(custom_type.into()),
            ")"
        },
    )
    .map(|_| ())
}

fn custom_type_from_payload(row: &CachedBranchEntryRow) -> Result<Option<String>, SessionError> {
    if row.type_ != "custom" {
        return Ok(None);
    }
    let parsed: pi_protocol::cbor::Value = pi_ai::utils::json::parse_json_with_repair(&row.payload)
        .map_err(|_| SessionError::new("invalid_entry", format!("Invalid SQLite session entry {}: failed to decode entry {}", row.id, row.id)))?;
    match parsed {
        pi_protocol::cbor::Value::Map(entries) => {
            let custom_type = entries.iter().find(|(key, _)| key == "customType").map(|(_, value)| value);
            match custom_type {
                Some(pi_protocol::cbor::Value::String(custom_type)) => Ok(Some(custom_type.clone())),
                _ => Err(SessionError::new(
                    "invalid_entry",
                    format!("Invalid SQLite session entry {}: failed to decode entry {}", row.id, row.id),
                )),
            }
        }
        _ => Err(SessionError::new(
            "invalid_entry",
            format!("Invalid SQLite session entry {}: failed to decode entry {}", row.id, row.id),
        )),
    }
}

pub fn insert_branch_entries_for_path(
    db: &dyn SqliteDatabase,
    session_id: &str,
    branch_id: &str,
    leaf_id: &str,
) -> Result<(), SessionError> {
    let mut path: Vec<CachedBranchEntryRow> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut entry_id: Option<String> = Some(leaf_id.to_string());

    while let Some(current) = entry_id {
        if seen.contains(&current) {
            return Err(SessionError::new("invalid_entry", format!("Entry parent cycle at {current}")));
        }
        seen.insert(current.clone());
        let row = query_get(
            db,
            &crate::sql! {
                "SELECT id, seq, parent_id, type, payload FROM entries WHERE session_id = ",
                SqlPart::Value(session_id.into()),
                " AND id = ",
                SqlPart::Value(current.clone().into())
            },
        )
        .map_err(|message| SessionError::new("storage", message))?;
        let Some(row) = row else {
            return Err(SessionError::new("invalid_entry", format!("Entry {current} not found")));
        };
        let row = CachedBranchEntryRow {
            session_id: session_id.to_string(),
            id: row.get_str("id").unwrap_or("").to_string(),
            entry_seq: row.get_f64("seq").unwrap_or(0.0),
            parent_id: row.get_str("parent_id").map(|value| value.to_string()),
            type_: row.get_str("type").unwrap_or("").to_string(),
            timestamp: 0.0,
            payload: row.get_str("payload").unwrap_or("").to_string(),
        };
        entry_id = row.parent_id.clone();
        path.push(row);
    }

    for row in path.iter().rev() {
        let custom_type = custom_type_from_payload(row)?;
        insert_branch_entry(
            db,
            session_id,
            branch_id,
            &row.id,
            row.entry_seq,
            &row.type_,
            custom_type.as_deref(),
        )
        .map_err(|message| SessionError::new("storage", message))?;
    }
    Ok(())
}

pub fn read_branch_containing_entry(
    db: &dyn SqliteDatabase,
    session_id: &str,
    entry_id: &str,
) -> Result<Option<CachedBranch>, String> {
    let row = query_get(
        db,
        &crate::sql! {
            "SELECT b.branch_id, b.entry_seq FROM branch_entries AS b WHERE b.session_id = ",
            SqlPart::Value(session_id.into()),
            " AND b.entry_id = ",
            SqlPart::Value(entry_id.into()),
            " ORDER BY b.branch_id LIMIT 1"
        },
    )?;
    Ok(row.map(|row| CachedBranch {
        branch_id: row.get_str("branch_id").unwrap_or("").to_string(),
        leaf_seq: row.get_f64("entry_seq").unwrap_or(0.0),
    }))
}

pub fn copy_branch_entries_through_seq(
    db: &dyn SqliteDatabase,
    session_id: &str,
    target_branch_id: &str,
    source_branch_id: &str,
    through_seq: f64,
) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "INSERT INTO branch_entries (session_id, branch_id, entry_id, entry_seq, entry_type, custom_type) SELECT session_id, ",
            SqlPart::Value(target_branch_id.into()),
            ", entry_id, entry_seq, entry_type, custom_type FROM branch_entries WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND branch_id = ",
            SqlPart::Value(source_branch_id.into()),
            " AND entry_seq <= ",
            SqlPart::Value(through_seq.into())
        },
    )
    .map(|_| ())
}

// ---------------------------------------------------------------------------
// branch-cache.ts
// ---------------------------------------------------------------------------

pub fn delete_branch_cache(db: &dyn SqliteDatabase, session_id: &str) -> Result<(), String> {
    delete_branch_tips(db, session_id)?;
    delete_branch_entries(db, session_id)
}

pub fn rebuild_branch_cache(db: &dyn SqliteDatabase, session_id: &str) -> Result<(), String> {
    let tips = query_all(
        db,
        &crate::sql! {
            "SELECT leaf.id FROM entries AS leaf WHERE leaf.session_id = ",
            SqlPart::Value(session_id.into()),
            " AND NOT EXISTS (SELECT 1 FROM entries AS child WHERE child.session_id = leaf.session_id AND child.parent_id = leaf.id) ORDER BY leaf.seq"
        },
    )?;
    delete_branch_cache(db, session_id)?;
    for tip in &tips {
        if let Some(id) = tip.get_str("id") {
            build_cached_branch(db, session_id, id)
                .map_err(|error| error.message)?;
        }
    }
    Ok(())
}

pub fn build_cached_branch(
    db: &dyn SqliteDatabase,
    session_id: &str,
    leaf_id: &str,
) -> Result<(), SessionError> {
    db.exec("SAVEPOINT build_branch_cache")
        .map_err(|message| SessionError::new("storage", message))?;
    let result = (|| -> Result<(), SessionError> {
        let branch_id = uuid_v7();
        insert_branch_entries_for_path(db, session_id, &branch_id, leaf_id)?;
        insert_branch_tip(db, session_id, leaf_id, &branch_id)
            .map_err(|message| SessionError::new("storage", message))?;
        db.exec("RELEASE SAVEPOINT build_branch_cache")
            .map_err(|message| SessionError::new("storage", message))?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = db.exec("ROLLBACK TO SAVEPOINT build_branch_cache");
        let _ = db.exec("RELEASE SAVEPOINT build_branch_cache");
        return Err(SessionError::new(
            "storage",
            format!("Failed to build SQLite branch cache at entry {leaf_id}: {error}"),
        ));
    }
    Ok(())
}

fn extend_branch(
    db: &dyn SqliteDatabase,
    session_id: &str,
    branch_id: &str,
    parent_id: &str,
    entry_id: &str,
    entry_seq: f64,
    entry_type: &str,
    custom_type: Option<&str>,
) -> Result<(), SessionError> {
    insert_branch_entry(db, session_id, branch_id, entry_id, entry_seq, entry_type, custom_type)
        .map_err(|message| SessionError::new("storage", message))?;
    if !update_branch_tip(db, session_id, branch_id, parent_id, entry_id)
        .map_err(|message| SessionError::new("storage", message))?
    {
        return Err(SessionError::new(
            "invalid_entry",
            format!("Branch tip {parent_id} changed during append"),
        ));
    }
    Ok(())
}

pub fn append_entry_to_branch_cache(
    db: &dyn SqliteDatabase,
    session_id: &str,
    entry_id: &str,
    entry_seq: f64,
    entry_type: &str,
    custom_type: Option<&str>,
    parent_id: Option<&str>,
) -> Result<(), SessionError> {
    let Some(parent_id) = parent_id else {
        let branch_id = uuid_v7();
        insert_branch_entry(db, session_id, &branch_id, entry_id, entry_seq, entry_type, custom_type)
            .map_err(|message| SessionError::new("storage", message))?;
        insert_branch_tip(db, session_id, entry_id, &branch_id)
            .map_err(|message| SessionError::new("storage", message))?;
        return Ok(());
    };

    let tip_branch_id = read_branch_tip_branch_id(db, session_id, parent_id)
        .map_err(|message| SessionError::new("storage", message))?;
    if let Some(branch_id) = tip_branch_id {
        return extend_branch(db, session_id, &branch_id, parent_id, entry_id, entry_seq, entry_type, custom_type);
    }

    let source = read_branch_containing_entry(db, session_id, parent_id)
        .map_err(|message| SessionError::new("storage", message))?;
    let Some(source) = source else {
        return Err(SessionError::new(
            "invalid_entry",
            format!("Branch cache has no branch containing parent entry {parent_id}"),
        ));
    };

    let branch_id = uuid_v7();
    copy_branch_entries_through_seq(db, session_id, &branch_id, &source.branch_id, source.leaf_seq)
        .map_err(|message| SessionError::new("storage", message))?;
    insert_branch_entry(db, session_id, &branch_id, entry_id, entry_seq, entry_type, custom_type)
        .map_err(|message| SessionError::new("storage", message))?;
    insert_branch_tip(db, session_id, entry_id, &branch_id)
        .map_err(|message| SessionError::new("storage", message))?;
    Ok(())
}

fn uuid_v7() -> String {
    pi_ai::utils::uuid::uuidv7()
}
