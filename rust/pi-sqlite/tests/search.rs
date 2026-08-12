//! FTS5 search integration tests.

use pi_sqlite::search_backend::create_sqlite_session_search;
use pi_sqlite::storage::entries::{insert_entry_row, NewEntryRow};
use pi_sqlite::storage::sessions::{insert_session_row, NewSessionRow};
use pi_sqlite::database::{query_run, RusqliteDatabase};
use pi_sqlite::migrations::apply_migrations;

fn new_db() -> RusqliteDatabase {
    let db = RusqliteDatabase::open_in_memory().unwrap();
    apply_migrations(&db).unwrap();
    db
}

#[test]
fn fts_search_finds_entries() {
    let db = new_db();
    insert_session_row(
        &db,
        &NewSessionRow {
            id: "s1".to_string(),
            created_at: 1000.0,
            cwd: "/tmp".to_string(),
            parent_session_id: None,
            metadata: None,
        },
    )
    .unwrap();
    insert_entry_row(
        &db,
        "s1",
        &NewEntryRow {
            seq: 1.0,
            id: "e1".to_string(),
            parent_id: None,
            type_: "message".to_string(),
            timestamp: 1000.0,
            payload: "{\"text\":\"hello world example\"}".to_string(),
        },
    )
    .unwrap();
    insert_entry_row(
        &db,
        "s1",
        &NewEntryRow {
            seq: 2.0,
            id: "e2".to_string(),
            parent_id: Some("e1".to_string()),
            type_: "message".to_string(),
            timestamp: 2000.0,
            payload: "{\"text\":\"unrelated content\"}".to_string(),
        },
    )
    .unwrap();

    // Build the FTS index over the entries table (external-content table).
    query_run(
        &db,
        &pi_sqlite::sql! {
            "CREATE VIRTUAL TABLE IF NOT EXISTS session_search_fts USING fts5(payload, content = 'entries', content_rowid = 'rowid', tokenize = 'trigram remove_diacritics 1'); INSERT INTO session_search_fts(session_search_fts) VALUES('rebuild');"
        },
    )
    .unwrap();

    // Search via the search backend over a temp file DB (in-memory not
    // supported by the backend path resolution); use a temp directory.
    let dir = std::env::temp_dir().join(format!("pi-search-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("search.db");
    let path_str = path.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&path_str);

    let search = create_sqlite_session_search(&path_str);
    // Seed the file DB.
    {
        let file_db = pi_sqlite::database::FileDatabaseFactory::open_file(&path_str).unwrap();
        apply_migrations(&file_db).unwrap();
        pi_sqlite::search_backend::ensure_search_schema_for_test(&file_db).unwrap();
        insert_session_row(
            &file_db,
            &NewSessionRow {
                id: "s1".to_string(),
                created_at: 1000.0,
                cwd: "/tmp".to_string(),
                parent_session_id: None,
                metadata: None,
            },
        )
        .unwrap();
        insert_entry_row(
            &file_db,
            "s1",
            &NewEntryRow {
                seq: 1.0,
                id: "e1".to_string(),
                parent_id: None,
                type_: "message".to_string(),
                timestamp: 1000.0,
                payload: "{\"text\":\"hello world example\"}".to_string(),
            },
        )
        .unwrap();
    }

    let hits = search.search("hello", Some("/tmp")).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].entry_id, "e1");
    assert_eq!(hits[0].metadata.id, "s1");

    let hits = search.search("missing-term", None).unwrap();
    assert!(hits.is_empty());

    let hits = search.search("   ", None).unwrap();
    assert!(hits.is_empty());

    std::fs::remove_dir_all(&dir).ok();
}
