//! SQLite database binding, port of
//! `packages/session-backends/sqlite-node/src/sqlite/types.ts`.
//!
//! The JS `SqliteDatabase` interface abstracts better-sqlite3 so tests can
//! swap backends; the Rust version is a thin wrapper over rusqlite
//! (bundled SQLite). Statements expose run/get/all with JS parameter
//! semantics (SqlValue).

use std::sync::{Arc, Mutex};

use rusqlite::types::ValueRef;
use rusqlite::{params_from_iter, Connection, OpenFlags};

use crate::sql::SqlQuery;

/// Result of a prepared statement execution.
#[derive(Clone, Debug, PartialEq)]
pub struct SqliteRunResult {
    pub changes: usize,
    pub last_insert_rowid: Option<i64>,
}

/// A row of column-name → value pairs (JS `TRow extends object`).
#[derive(Clone, Debug, PartialEq)]
pub struct SqliteRow {
    pub columns: Vec<(String, SqliteValue)>,
}

impl SqliteRow {
    pub fn get_str(&self, column: &str) -> Option<&str> {
        self.columns
            .iter()
            .find(|(name, _)| name == column)
            .and_then(|(_, value)| match value {
                SqliteValue::Text(text) => Some(text.as_str()),
                _ => None,
            })
    }
    pub fn get_f64(&self, column: &str) -> Option<f64> {
        self.columns
            .iter()
            .find(|(name, _)| name == column)
            .and_then(|(_, value)| match value {
                SqliteValue::Int(value) => Some(*value as f64),
                SqliteValue::Float(value) => Some(*value),
                _ => None,
            })
    }
    pub fn get_i64(&self, column: &str) -> Option<i64> {
        self.columns
            .iter()
            .find(|(name, _)| name == column)
            .and_then(|(_, value)| match value {
                SqliteValue::Int(value) => Some(*value),
                SqliteValue::Float(value) => Some(*value as i64),
                _ => None,
            })
    }
}

/// A SQLite column value (JS `unknown` rows).
#[derive(Clone, Debug, PartialEq)]
pub enum SqliteValue {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Blob(Vec<u8>),
}

/// SQLite database capability (JS `SqliteDatabase`). Statements are
/// prepared per call with an LRU cache inside rusqlite, matching the JS
/// interface's observable behavior.
pub trait SqliteDatabase: Send + Sync {
    fn exec(&self, sql_text: &str) -> Result<(), String>;
    fn run(&self, sql_text: &str, params: &[crate::sql::SqlValue]) -> Result<SqliteRunResult, String>;
    fn get(&self, sql_text: &str, params: &[crate::sql::SqlValue]) -> Result<Option<SqliteRow>, String>;
    fn all(&self, sql_text: &str, params: &[crate::sql::SqlValue]) -> Result<Vec<SqliteRow>, String>;
    fn transaction(&self, callback: Box<dyn FnOnce(&dyn SqliteDatabase) + Send>);
    fn close(&self);
}

/// Factory for opening databases (JS `SqliteDatabaseFactory`).
pub trait SqliteDatabaseFactory: Send + Sync {
    fn open(&self, path: &str) -> Result<Arc<RusqliteDatabase>, String>;
}

fn to_rusqlite_value(value: &crate::sql::SqlValue) -> rusqlite::types::Value {
    match value {
        crate::sql::SqlValue::Null => rusqlite::types::Value::Null,
        crate::sql::SqlValue::Int(value) => rusqlite::types::Value::Integer(*value),
        crate::sql::SqlValue::Float(value) => rusqlite::types::Value::Real(*value),
        crate::sql::SqlValue::Text(text) => rusqlite::types::Value::Text(text.clone()),
    }
}

fn row_to_sqlite_row(row: &rusqlite::Row) -> Result<SqliteRow, String> {
    let mut columns = Vec::new();
    for index in 0..row.as_ref().column_count() {
        let name = row.as_ref().column_name(index).unwrap_or("").to_string();
        let value = match row.get_ref(index).map_err(|error| error.to_string())? {
            ValueRef::Null => SqliteValue::Null,
            ValueRef::Integer(value) => SqliteValue::Int(value),
            ValueRef::Real(value) => SqliteValue::Float(value),
            ValueRef::Text(bytes) => SqliteValue::Text(String::from_utf8_lossy(bytes).to_string()),
            ValueRef::Blob(bytes) => SqliteValue::Blob(bytes.to_vec()),
        };
        columns.push((name, value));
    }
    Ok(SqliteRow { columns })
}

/// rusqlite-backed database (JS better-sqlite3 analog).
pub struct RusqliteDatabase {
    connection: Mutex<Connection>,
}

impl RusqliteDatabase {
    pub fn open_in_memory() -> Result<Self, String> {
        let connection = Connection::open_in_memory().map_err(|error| error.to_string())?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }
}

impl SqliteDatabase for RusqliteDatabase {
    fn exec(&self, sql_text: &str) -> Result<(), String> {
        self.connection
            .lock()
            .unwrap()
            .execute_batch(sql_text)
            .map_err(|error| error.to_string())
    }

    fn run(&self, sql_text: &str, params: &[crate::sql::SqlValue]) -> Result<SqliteRunResult, String> {
        let connection = self.connection.lock().unwrap();
        let values: Vec<rusqlite::types::Value> = params.iter().map(to_rusqlite_value).collect();
        let mut statement = connection
            .prepare_cached(sql_text)
            .map_err(|error| error.to_string())?;
        let changes = statement
            .execute(params_from_iter(values))
            .map_err(|error| error.to_string())?;
        let last_insert_rowid = connection.last_insert_rowid();
        Ok(SqliteRunResult {
            changes,
            last_insert_rowid: Some(last_insert_rowid),
        })
    }

    fn get(&self, sql_text: &str, params: &[crate::sql::SqlValue]) -> Result<Option<SqliteRow>, String> {
        let connection = self.connection.lock().unwrap();
        let values: Vec<rusqlite::types::Value> = params.iter().map(to_rusqlite_value).collect();
        let mut statement = connection
            .prepare_cached(sql_text)
            .map_err(|error| error.to_string())?;
        let mut rows = statement.query(params_from_iter(values)).map_err(|error| error.to_string())?;
        match rows.next() {
            Ok(Some(row)) => row_to_sqlite_row(row).map(Some),
            Ok(None) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn all(&self, sql_text: &str, params: &[crate::sql::SqlValue]) -> Result<Vec<SqliteRow>, String> {
        let connection = self.connection.lock().unwrap();
        let values: Vec<rusqlite::types::Value> = params.iter().map(to_rusqlite_value).collect();
        let mut statement = connection
            .prepare_cached(sql_text)
            .map_err(|error| error.to_string())?;
        let mut rows = statement.query(params_from_iter(values)).map_err(|error| error.to_string())?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().map_err(|error| error.to_string())? {
            result.push(row_to_sqlite_row(row)?);
        }
        Ok(result)
    }

    fn transaction(&self, callback: Box<dyn FnOnce(&dyn SqliteDatabase) + Send>) {
        // Begin/commit each take and release the connection lock; the
        // callback re-enters the lock per statement (std Mutex is not
        // reentrant, so holding it across the callback would deadlock).
        self.exec("BEGIN").expect("BEGIN failed");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            callback(self);
        }));
        match result {
            Ok(()) => self.exec("COMMIT").expect("COMMIT failed"),
            Err(payload) => {
                let _ = self.exec("ROLLBACK");
                std::panic::resume_unwind(payload);
            }
        }
    }

    fn close(&self) {
        // Connection is behind a Mutex; dropping the guard closes nothing.
        // The DB is dropped when the last Arc goes away.
    }
}

/// Open a database file (or in-memory when path is ":memory:").
pub struct FileDatabaseFactory;

impl SqliteDatabaseFactory for FileDatabaseFactory {
    fn open(&self, path: &str) -> Result<Arc<RusqliteDatabase>, String> {
        let connection = if path == ":memory:" {
            Connection::open_in_memory()
        } else {
            Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
        }
        .map_err(|error| error.to_string())?;
        Ok(Arc::new(RusqliteDatabase {
            connection: Mutex::new(connection),
        }))
    }
}

/// Run a SqlQuery and return its result (JS `SqlQuery.run`).
pub fn query_run(db: &dyn SqliteDatabase, query: &SqlQuery) -> Result<SqliteRunResult, String> {
    db.run(&query.query_text, &query.params)
}

/// Run a SqlQuery and return the first row (JS `SqlQuery.get`).
pub fn query_get(db: &dyn SqliteDatabase, query: &SqlQuery) -> Result<Option<SqliteRow>, String> {
    db.get(&query.query_text, &query.params)
}

/// Run a SqlQuery and return all rows (JS `SqlQuery.all`).
pub fn query_all(db: &dyn SqliteDatabase, query: &SqlQuery) -> Result<Vec<SqliteRow>, String> {
    db.all(&query.query_text, &query.params)
}

/// Execute a parameterless SqlQuery (JS `SqlQuery.exec`).
pub fn query_exec(db: &dyn SqliteDatabase, query: &SqlQuery) -> Result<(), String> {
    if query.has_params() {
        return Err("SQLite exec queries cannot have parameters".to_string());
    }
    db.exec(&query.query_text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_and_reads_rows() {
        let db = RusqliteDatabase::open_in_memory().unwrap();
        db.exec("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
        db.run(
            "INSERT INTO t (name) VALUES (?)",
            &[crate::sql::SqlValue::Text("hello".to_string())],
        )
        .unwrap();
        let rows = db.all("SELECT id, name FROM t", &[]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_str("name"), Some("hello"));
        assert_eq!(rows[0].get_i64("id"), Some(1));
    }

    #[test]
    fn transaction_commits() {
        let db = RusqliteDatabase::open_in_memory().unwrap();
        db.exec("CREATE TABLE t (id INTEGER)").unwrap();
        db.transaction(Box::new(|transaction_db| {
            transaction_db
                .run("INSERT INTO t (id) VALUES (?)", &[crate::sql::SqlValue::Int(42)])
                .unwrap();
        }));
        let row = db.get("SELECT count(*) AS n FROM t", &[]).unwrap().unwrap();
        assert_eq!(row.get_i64("n"), Some(1));
    }

    #[test]
    fn row_values_decode() {
        let db = RusqliteDatabase::open_in_memory().unwrap();
        db.exec("CREATE TABLE t (a TEXT, b REAL, c INTEGER, d NULL)").unwrap();
        db.run(
            "INSERT INTO t VALUES (?, ?, ?, ?)",
            &[
                crate::sql::SqlValue::Text("x".to_string()),
                crate::sql::SqlValue::Float(1.5),
                crate::sql::SqlValue::Int(7),
                crate::sql::SqlValue::Null,
            ],
        )
        .unwrap();
        let row = db.get("SELECT a, b, c, d FROM t", &[]).unwrap().unwrap();
        assert_eq!(row.get_str("a"), Some("x"));
        assert_eq!(row.get_f64("b"), Some(1.5));
        assert_eq!(row.get_i64("c"), Some(7));
        assert_eq!(row.get_str("d"), None);
    }
}
