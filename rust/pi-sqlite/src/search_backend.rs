//! FTS5 session search, port of
//! `packages/session-backends/sqlite-node/src/sqlite/search-backend.ts`.

use crate::database::{query_all, query_get, RusqliteDatabase, SqliteDatabase};
use crate::migrations::apply_migrations;
use crate::sql::SqlPart;
use crate::storage::sessions::{decode_session_metadata, SessionRow};

fn get_parent_path(path: &str) -> String {
    let normalized = path.trim_end_matches(['/', '\\']);
    let last_slash = normalized
        .rfind('/')
        .map(|index| index as isize)
        .or_else(|| normalized.rfind('\\').map(|index| index as isize));
    match last_slash {
        None => ".".to_string(),
        Some(0) => normalized[..1].to_string(),
        Some(index) => normalized[..index as usize].to_string(),
    }
}

fn configure_sqlite_database(db: &dyn SqliteDatabase) -> Result<(), String> {
    db.exec("PRAGMA journal_mode=WAL")?;
    db.exec("PRAGMA synchronous=FULL")?;
    db.exec("PRAGMA busy_timeout=5000")?;
    Ok(())
}

fn table_exists(db: &dyn SqliteDatabase, name: &str) -> Result<bool, String> {
    let row = query_get(
        db,
        &crate::sql! {
            "SELECT 1 AS found FROM sqlite_master WHERE type = 'table' AND name = ",
            SqlPart::Value(name.into()),
            " LIMIT 1"
        },
    )?;
    Ok(row.is_some())
}

fn ensure_search_schema(db: &dyn SqliteDatabase) -> Result<(), String> {
    let fts_exists = table_exists(db, "session_search_fts")?;
    db.exec(
        "CREATE VIRTUAL TABLE IF NOT EXISTS session_search_fts USING fts5(
            payload,
            content = 'entries',
            content_rowid = 'rowid',
            tokenize = 'trigram remove_diacritics 1'
        );
        CREATE TRIGGER IF NOT EXISTS session_search_fts_ai AFTER INSERT ON entries BEGIN
            INSERT INTO session_search_fts(rowid, payload) VALUES (new.rowid, new.payload);
        END;
        CREATE TRIGGER IF NOT EXISTS session_search_fts_ad AFTER DELETE ON entries BEGIN
            INSERT INTO session_search_fts(session_search_fts, rowid, payload) VALUES('delete', old.rowid, old.payload);
        END;
        CREATE TRIGGER IF NOT EXISTS session_search_fts_au AFTER UPDATE OF payload ON entries BEGIN
            INSERT INTO session_search_fts(session_search_fts, rowid, payload) VALUES('delete', old.rowid, old.payload);
            INSERT INTO session_search_fts(rowid, payload) VALUES (new.rowid, new.payload);
        END;",
    )?;
    if !fts_exists {
        db.exec("INSERT INTO session_search_fts(session_search_fts) VALUES('rebuild')")?;
    }
    Ok(())
}

/// FTS search hit.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSearchHit {
    pub metadata: crate::storage::sessions::SqliteSessionMetadata,
    pub entry_id: String,
    pub timestamp: f64,
    pub score: f64,
}

pub struct SqliteSessionSearch {
    database_path: String,
}

impl SqliteSessionSearch {
    pub fn new(database_path: &str) -> Self {
        Self {
            database_path: database_path.to_string(),
        }
    }

    fn open_database(&self) -> Result<RusqliteDatabase, String> {
        let directory = get_parent_path(&self.database_path);
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("Failed to create SQLite search directory {directory}: {error}"))?;
        let db = crate::database::FileDatabaseFactory::open_file(&self.database_path)?;
        configure_sqlite_database(&db)?;
        apply_migrations(&db)?;
        ensure_search_schema(&db)?;
        Ok(db)
    }

    /// Search entries; empty text returns no hits.
    pub fn search(&self, text: &str, cwd: Option<&str>) -> Result<Vec<SessionSearchHit>, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(vec![]);
        }
        let db = self.open_database()?;
        let quoted = format!("\"{}\"", trimmed.replace('"', "\"\""));
        let rows = query_all(
            &db,
            &crate::sql! {
                "SELECT s.id, s.created_at, s.metadata, s.cwd, s.parent_session_id, name_fact.seq IS NOT NULL AS has_session_name, name_fact.value AS session_name, se.id AS entry_id, se.timestamp, bm25(session_search_fts) AS score FROM session_search_fts JOIN entries AS se ON se.rowid = session_search_fts.rowid JOIN sessions AS s ON s.id = se.session_id LEFT JOIN facts AS name_fact ON name_fact.session_id = s.id AND name_fact.kind = 'name' AND name_fact.key IS NULL AND name_fact.seq = (SELECT MAX(f.seq) FROM facts AS f WHERE f.session_id = s.id AND f.kind = 'name' AND f.key IS NULL) WHERE session_search_fts MATCH ",
                SqlPart::Value(quoted.clone().into()),
                " AND (",
                SqlPart::Value(cwd.into()),
                " IS NULL OR s.cwd = ",
                SqlPart::Value(cwd.into()),
                ") ORDER BY score"
            },
        )?;
        Ok(rows
            .iter()
            .map(|row| {
                let session_row = SessionRow {
                    id: row.get_str("id").unwrap_or("").to_string(),
                    created_at: row.get_f64("created_at").unwrap_or(0.0),
                    metadata: row.get_str("metadata").map(|value| value.to_string()),
                    cwd: row.get_str("cwd").unwrap_or("").to_string(),
                    parent_session_id: row.get_str("parent_session_id").map(|value| value.to_string()),
                    has_session_name: row.get_i64("has_session_name") == Some(1),
                    session_name: row.get_str("session_name").map(|value| value.to_string()),
                };
                SessionSearchHit {
                    metadata: decode_session_metadata(&session_row, &self.database_path).unwrap_or_else(|_| {
                        crate::storage::sessions::SqliteSessionMetadata {
                            id: session_row.id.clone(),
                            created_at: session_row.created_at,
                            name: None,
                            cwd: session_row.cwd.clone(),
                            path: self.database_path.clone(),
                            parent_session_id: session_row.parent_session_id.clone(),
                            metadata: None,
                        }
                    }),
                    entry_id: row.get_str("entry_id").unwrap_or("").to_string(),
                    timestamp: row.get_f64("timestamp").unwrap_or(0.0),
                    score: row.get_f64("score").unwrap_or(0.0),
                }
            })
            .collect())
    }
}

/// Test helper: expose ensure_search_schema for seeding file databases.
pub fn ensure_search_schema_for_test(db: &dyn SqliteDatabase) -> Result<(), String> {
    ensure_search_schema(db)
}

pub fn create_sqlite_session_search(database_path: &str) -> SqliteSessionSearch {
    SqliteSessionSearch::new(database_path)
}
