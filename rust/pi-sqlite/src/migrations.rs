//! Schema migrations, port of
//! `packages/session-backends/sqlite-node/src/sqlite/migrations.ts`.

use crate::database::{query_all, query_run, SqliteDatabase};
use crate::sql::SqlValue;

pub const INITIAL_SCHEMA_SQL: &str = include_str!("migrations/001_initial.sql");

pub struct SqliteMigration {
    pub id: &'static str,
    pub order: u64,
    pub sql: &'static str,
}

pub fn load_migrations() -> Vec<SqliteMigration> {
    vec![SqliteMigration {
        id: "001_initial.sql",
        order: 1,
        sql: INITIAL_SCHEMA_SQL,
    }]
}

fn ensure_migrations_table(db: &dyn SqliteDatabase) -> Result<(), String> {
    db.exec(
        "CREATE TABLE IF NOT EXISTS migrations (
            id TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL
        );",
    )
}

/// Apply pending migrations inside transactions (JS `applyMigrations`).
pub fn apply_migrations(db: &dyn SqliteDatabase) -> Result<(), String> {
    ensure_migrations_table(db)?;
    let migrations = load_migrations();
    let applied_rows = query_all(
        db,
        &crate::sql! {
            "SELECT id FROM migrations ORDER BY applied_at, id"
        },
    )?;
    let mut applied: std::collections::HashSet<String> = applied_rows
        .iter()
        .filter_map(|row| row.get_str("id").map(|id| id.to_string()))
        .collect();

    for migration in migrations {
        if applied.contains(migration.id) {
            continue;
        }
        let applied_at = now_iso();
        db.transaction(Box::new(move |transaction_db| {
            transaction_db.exec(migration.sql).expect("migration failed");
            query_run(
                transaction_db,
                &crate::sql! {
                    "INSERT INTO migrations (id, applied_at) VALUES (",
                    SqlPartValue(migration.id.to_string()),
                    ", ",
                    SqlPartValue(applied_at),
                    ")"
                },
            )
            .expect("migration record failed");
        }));
        applied.insert(migration.id.to_string());
    }
    Ok(())
}

/// Helper to build SqlPart::Value from a string (macro hygiene).
pub struct SqlPartValue(pub String);

impl From<SqlPartValue> for crate::sql::SqlPart {
    fn from(value: SqlPartValue) -> Self {
        crate::sql::SqlPart::Value(SqlValue::Text(value.0))
    }
}

fn now_iso() -> String {
    // JS new Date().toISOString(): UTC with milliseconds.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock");
    let millis = now.as_millis() as u64;
    let secs = millis / 1000;
    let days = secs / 86400;
    let (year, month, day) = civil_from_days(days as i64);
    let (hour, minute, second) = ((secs % 86400) / 3600, (secs % 3600) / 60, secs % 60);
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        millis % 1000
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant's algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{query_all, RusqliteDatabase};

    #[test]
    fn applies_migrations_once() {
        let db = RusqliteDatabase::open_in_memory().unwrap();
        apply_migrations(&db).unwrap();
        apply_migrations(&db).unwrap();
        // Tables exist.
        let rows = query_all(&db, &crate::sql! { "SELECT name FROM sqlite_master WHERE type='table' AND name='entries'" }).unwrap();
        assert_eq!(rows.len(), 1);
        let rows = query_all(&db, &crate::sql! { "SELECT id FROM migrations" }).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn iso_timestamp_format() {
        let text = now_iso();
        assert!(text.ends_with('Z'));
        assert_eq!(text.len(), 24); // YYYY-MM-DDTHH:MM:SS.mmmZ
    }
}
