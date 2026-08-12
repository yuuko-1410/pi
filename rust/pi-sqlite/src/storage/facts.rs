//! Facts table access, port of
//! `packages/session-backends/sqlite-node/src/sqlite/storage/facts.ts`.

use crate::database::{query_all, query_get, query_run, query_exec, SqliteDatabase};
use crate::sql::SqlPart;

#[derive(Clone, Debug, PartialEq)]
pub struct FactRow {
    pub session_id: String,
    pub seq: f64,
    pub kind: String,
    pub key: Option<String>,
    pub value: Option<String>,
}

fn fact_row_from_row(row: &crate::database::SqliteRow) -> FactRow {
    FactRow {
        session_id: row.get_str("session_id").unwrap_or("").to_string(),
        seq: row.get_f64("seq").unwrap_or(0.0),
        kind: row.get_str("kind").unwrap_or("").to_string(),
        key: row.get_str("key").map(|value| value.to_string()),
        value: row.get_str("value").map(|value| value.to_string()),
    }
}

pub fn append_fact(
    db: &dyn SqliteDatabase,
    session_id: &str,
    seq: f64,
    kind: &str,
    key: Option<&str>,
    value: Option<&str>,
) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "INSERT INTO facts (session_id, seq, kind, key, value) VALUES (",
            SqlPart::Value(session_id.into()),
            ", ",
            SqlPart::Value(seq.into()),
            ", ",
            SqlPart::Value(kind.into()),
            ", ",
            SqlPart::Value(key.into()),
            ", ",
            SqlPart::Value(value.into()),
            ")"
        },
    )
    .map(|_| ())
}

pub fn read_latest_fact(
    db: &dyn SqliteDatabase,
    session_id: &str,
    kind: &str,
    key: Option<&str>,
) -> Result<Option<FactRow>, String> {
    let row = query_get(
        db,
        &crate::sql! {
            "SELECT session_id, seq, kind, key, value FROM facts INDEXED BY idx_facts_session_kind_key_seq WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND kind = ",
            SqlPart::Value(kind.into()),
            " AND key IS ",
            SqlPart::Value(key.into()),
            " ORDER BY seq DESC LIMIT 1"
        },
    )?;
    Ok(row.map(|row| fact_row_from_row(&row)))
}

pub fn read_latest_label_facts(
    db: &dyn SqliteDatabase,
    session_id: &str,
) -> Result<Vec<(String, String)>, String> {
    let rows = query_all(
        db,
        &crate::sql! {
            "SELECT f.key, f.value FROM facts AS f INDEXED BY idx_facts_session_kind_key_seq WHERE f.session_id = ",
            SqlPart::Value(session_id.into()),
            " AND f.kind = 'label' AND f.value IS NOT NULL AND f.seq = (SELECT MAX(candidate.seq) FROM facts AS candidate INDEXED BY idx_facts_session_kind_key_seq WHERE candidate.session_id = f.session_id AND candidate.kind = f.kind AND candidate.key IS f.key) ORDER BY f.key"
        },
    )?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            let key = row.get_str("key")?.to_string();
            let value = row.get_str("value")?.to_string();
            Some((key, value))
        })
        .collect())
}

pub fn read_fact_rows(
    db: &dyn SqliteDatabase,
    session_id: &str,
    after_seq: Option<f64>,
    limit: Option<f64>,
) -> Result<Vec<FactRow>, String> {
    let mut parts: Vec<SqlPart> = vec![
        "SELECT session_id, seq, kind, key, value FROM facts WHERE session_id = ".into(),
        SqlPart::Value(session_id.into()),
    ];
    if let Some(after_seq) = after_seq {
        parts.push(" AND seq > ".into());
        parts.push(SqlPart::Value(after_seq.into()));
    }
    parts.push(" ORDER BY seq".into());
    if let Some(limit) = limit {
        parts.push(" LIMIT ".into());
        parts.push(SqlPart::Value(limit.into()));
    }
    let query = crate::sql::build_sql_query(&parts);
    let rows = query_all(db, &query)?;
    Ok(rows.iter().map(fact_row_from_row).collect())
}

pub fn delete_fact_rows(db: &dyn SqliteDatabase, session_id: &str) -> Result<(), String> {
    query_exec(
        db,
        &crate::sql! {
            "DELETE FROM facts WHERE session_id = ",
            SqlPart::Value(session_id.into())
        },
    )
}
