//! Lanes table access, port of
//! `packages/session-backends/sqlite-node/src/sqlite/storage/lanes.ts`.

use pi_agent_core::harness::session_types::SessionError;

use crate::database::{query_all, query_get, query_run, SqliteDatabase};
use crate::sql::SqlPart;

#[derive(Clone, Debug, PartialEq)]
pub struct LaneRow {
    pub session_id: String,
    pub lane: String,
    pub leaf_id: Option<String>,
    pub open_operation_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LaneMoveRow {
    pub session_id: String,
    pub seq: f64,
    pub lane: String,
    pub leaf_id: Option<String>,
}

pub fn create_initial_lane(
    db: &dyn SqliteDatabase,
    session_id: &str,
    lane: &str,
    leaf_id: Option<&str>,
) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "INSERT INTO lanes (session_id, lane, leaf_id, open_operation_id) VALUES (",
            SqlPart::Value(session_id.into()),
            ", ",
            SqlPart::Value(lane.into()),
            ", ",
            SqlPart::Value(leaf_id.into()),
            ", NULL)"
        },
    )
    .map(|_| ())
}

fn lane_row_from_row(row: &crate::database::SqliteRow) -> LaneRow {
    LaneRow {
        session_id: row.get_str("session_id").unwrap_or("").to_string(),
        lane: row.get_str("lane").unwrap_or("").to_string(),
        leaf_id: row.get_str("leaf_id").map(|value| value.to_string()),
        open_operation_id: row.get_str("open_operation_id").map(|value| value.to_string()),
    }
}

/// Read all lanes, validating leaf references (JS `readLanes`).
pub fn read_lanes(db: &dyn SqliteDatabase, session_id: &str) -> Result<Vec<LaneRow>, SessionError> {
    let rows = query_all(
        db,
        &crate::sql! {
            "SELECT l.session_id, l.lane, l.leaf_id, l.open_operation_id, (l.leaf_id IS NULL OR EXISTS (SELECT 1 FROM entries AS e WHERE e.session_id = l.session_id AND e.id = l.leaf_id)) AS leaf_exists FROM lanes AS l WHERE l.session_id = ",
            SqlPart::Value(session_id.into()),
            " ORDER BY l.lane"
        },
    )
    .map_err(|message| SessionError::new("storage", format!("{message}")))?;
    let mut result = Vec::new();
    for row in &rows {
        if row.get_i64("leaf_exists") == Some(0) {
            return Err(SessionError::new(
                "storage",
                format!("Lane {} points at missing entry {:?}", row.get_str("lane").unwrap_or(""), row.get_str("leaf_id")),
            ));
        }
        result.push(lane_row_from_row(row));
    }
    Ok(result)
}

pub fn read_lane(
    db: &dyn SqliteDatabase,
    session_id: &str,
    lane: &str,
) -> Result<Option<LaneRow>, String> {
    let row = query_get(
        db,
        &crate::sql! {
            "SELECT session_id, lane, leaf_id, open_operation_id FROM lanes WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND lane = ",
            SqlPart::Value(lane.into())
        },
    )?;
    Ok(row.map(|row| lane_row_from_row(&row)))
}

/// Read the lane head with leaf validation (JS `readLaneHead`).
pub fn read_lane_head(
    db: &dyn SqliteDatabase,
    session_id: &str,
    lane: &str,
) -> Result<Option<String>, SessionError> {
    let row = query_get(
        db,
        &crate::sql! {
            "SELECT l.leaf_id, (l.leaf_id IS NULL OR EXISTS (SELECT 1 FROM entries AS e WHERE e.session_id = l.session_id AND e.id = l.leaf_id)) AS leaf_exists FROM lanes AS l WHERE l.session_id = ",
            SqlPart::Value(session_id.into()),
            " AND l.lane = ",
            SqlPart::Value(lane.into())
        },
    )
    .map_err(|message| SessionError::new("storage", format!("{message}")))?;
    let Some(row) = row else {
        return Err(SessionError::new("invalid_lane", format!("Lane not found: {lane}")));
    };
    if row.get_i64("leaf_exists") == Some(0) {
        return Err(SessionError::new(
            "storage",
            format!("Entry {:?} not found", row.get_str("leaf_id")),
        ));
    }
    Ok(row.get_str("leaf_id").map(|value| value.to_string()))
}

pub fn create_lane(
    db: &dyn SqliteDatabase,
    session_id: &str,
    seq: f64,
    lane: &str,
    leaf_id: Option<&str>,
) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "INSERT INTO lanes (session_id, lane, leaf_id, open_operation_id) VALUES (",
            SqlPart::Value(session_id.into()),
            ", ",
            SqlPart::Value(lane.into()),
            ", ",
            SqlPart::Value(leaf_id.into()),
            ", NULL)"
        },
    )?;
    append_lane_move(db, session_id, seq, lane, leaf_id)
}

pub fn move_lane(
    db: &dyn SqliteDatabase,
    session_id: &str,
    seq: f64,
    lane: &str,
    leaf_id: Option<&str>,
) -> Result<(), SessionError> {
    let result = query_run(
        db,
        &crate::sql! {
            "UPDATE lanes SET leaf_id = ",
            SqlPart::Value(leaf_id.into()),
            " WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND lane = ",
            SqlPart::Value(lane.into())
        },
    )
    .map_err(|message| SessionError::new("storage", format!("{message}")))?;
    if result.changes != 1 {
        return Err(SessionError::new("invalid_lane", format!("Lane not found: {lane}")));
    }
    append_lane_move(db, session_id, seq, lane, leaf_id)
        .map_err(|message| SessionError::new("storage", message))
}

pub fn set_lane_leaf(
    db: &dyn SqliteDatabase,
    session_id: &str,
    lane: &str,
    leaf_id: Option<&str>,
) -> Result<(), SessionError> {
    let result = query_run(
        db,
        &crate::sql! {
            "UPDATE lanes SET leaf_id = ",
            SqlPart::Value(leaf_id.into()),
            " WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND lane = ",
            SqlPart::Value(lane.into())
        },
    )
    .map_err(|message| SessionError::new("storage", format!("{message}")))?;
    if result.changes != 1 {
        return Err(SessionError::new("invalid_lane", format!("Lane not found: {lane}")));
    }
    Ok(())
}

pub fn start_lane_operation(
    db: &dyn SqliteDatabase,
    session_id: &str,
    lane: &str,
    run_id: &str,
) -> Result<(), SessionError> {
    let result = query_run(
        db,
        &crate::sql! {
            "UPDATE lanes SET open_operation_id = ",
            SqlPart::Value(run_id.into()),
            " WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND lane = ",
            SqlPart::Value(lane.into()),
            " AND open_operation_id IS NULL"
        },
    )
    .map_err(|message| SessionError::new("storage", format!("{message}")))?;
    if result.changes == 1 {
        return Ok(());
    }
    let current = read_lane(db, session_id, lane).map_err(|message| SessionError::new("storage", message))?;
    match current {
        None => Err(SessionError::new("invalid_lane", format!("Lane not found: {lane}"))),
        Some(current) => Err(SessionError::new(
            "storage",
            format!("Lane {lane} already has an open operation {:?}", current.open_operation_id),
        )),
    }
}

pub fn finish_lane_operation(
    db: &dyn SqliteDatabase,
    session_id: &str,
    lane: &str,
    run_id: &str,
) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "UPDATE lanes SET open_operation_id = NULL WHERE session_id = ",
            SqlPart::Value(session_id.into()),
            " AND lane = ",
            SqlPart::Value(lane.into()),
            " AND open_operation_id = ",
            SqlPart::Value(run_id.into())
        },
    )
    .map(|_| ())
}

pub fn read_lane_move_rows(
    db: &dyn SqliteDatabase,
    session_id: &str,
    after_seq: Option<f64>,
    limit: Option<f64>,
) -> Result<Vec<LaneMoveRow>, String> {
    let mut parts: Vec<SqlPart> = vec![
        "SELECT session_id, seq, lane, leaf_id FROM lane_moves WHERE session_id = ".into(),
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
    Ok(rows
        .iter()
        .map(|row| LaneMoveRow {
            session_id: row.get_str("session_id").unwrap_or("").to_string(),
            seq: row.get_f64("seq").unwrap_or(0.0),
            lane: row.get_str("lane").unwrap_or("").to_string(),
            leaf_id: row.get_str("leaf_id").map(|value| value.to_string()),
        })
        .collect())
}

pub fn delete_lane_rows(db: &dyn SqliteDatabase, session_id: &str) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "DELETE FROM lane_moves WHERE session_id = ",
            SqlPart::Value(session_id.into())
        },
    )?;
    query_run(
        db,
        &crate::sql! {
            "DELETE FROM lanes WHERE session_id = ",
            SqlPart::Value(session_id.into())
        },
    )
    .map(|_| ())
}

fn append_lane_move(
    db: &dyn SqliteDatabase,
    session_id: &str,
    seq: f64,
    lane: &str,
    leaf_id: Option<&str>,
) -> Result<(), String> {
    query_run(
        db,
        &crate::sql! {
            "INSERT INTO lane_moves (session_id, seq, lane, leaf_id) VALUES (",
            SqlPart::Value(session_id.into()),
            ", ",
            SqlPart::Value(seq.into()),
            ", ",
            SqlPart::Value(lane.into()),
            ", ",
            SqlPart::Value(leaf_id.into()),
            ")"
        },
    )
    .map(|_| ())
}
