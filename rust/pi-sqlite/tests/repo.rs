//! Repository end-to-end tests over a temp-file database.

use pi_sqlite::repo::SessionStorageExt;

use pi_agent_core::harness::session_types::{
    Entry, EntryBase, EntryQuery, LaneRecord, MessageEntry, OperationStartedRecord, RecordBase, RunIntent,
    SessionError, SessionMetadata, SessionStats, SessionStorage,
};
use pi_agent_core::types::AgentMessage;
use pi_protocol::cbor::Value;
use pi_sqlite::repo::{SqliteSessionRepository, SqliteSessionRepositoryOptions, SqliteSessionStorage};

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn temp_db_path() -> String {
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("pi-repo-{}-{counter}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("sessions.db").to_string_lossy().to_string()
}

fn repo() -> std::sync::Arc<SqliteSessionRepository> {
    let path = temp_db_path();
    let _ = std::fs::remove_file(&path);
    SqliteSessionRepository::new(SqliteSessionRepositoryOptions {
        database_path: path,
        writer_lease: None,
    })
    .unwrap()
}

fn user_entry(id: &str) -> Entry {
    Entry::Message(MessageEntry {
        base: EntryBase {
            type_: "message".to_string(),
            id: id.to_string(),
            seq: 0.0,
            parent_id: None,
            timestamp: 0.0,
        },
        message: AgentMessage::Llm(pi_ai::types::Message::User(pi_ai::types::UserMessage {
            content: pi_ai::types::UserMessageContent::Text(format!("hello {id}")),
            timestamp: 1000.0,
        })),
        terminate: None,
    })
}

fn operation_started(id: &str, lane: &str) -> LaneRecord {
    LaneRecord::OperationStarted(OperationStartedRecord {
        base: RecordBase {
            id: id.to_string(),
            seq: 0.0,
            lane: lane.to_string(),
            timestamp: 0.0,
        },
        source_leaf_id: None,
        intent: RunIntent::Run {
            original_prompt: vec![],
            initial_messages: vec![],
            system_prompt_override: None,
            resume_data: None,
        },
    })
}

#[test]
fn create_append_read_cycle() {
    let repository = repo();
    let storage = repository
        .create_session(None, "/tmp", None, None)
        .unwrap();

    // Append entries.
    let e1 = storage.append_entry(user_entry("e1"), "main").unwrap();
    assert_eq!(e1.id(), "e1");
    assert_eq!(e1.seq(), 1.0);
    assert!(e1.seq() > 0.0);
    let e2 = storage.append_entry(user_entry("e2"), "main").unwrap();
    assert_eq!(e2.seq(), 2.0);
    // The second entry's parent is the first entry.
    match &e2 {
        Entry::Message(entry) => assert_eq!(entry.base.parent_id.as_deref(), Some("e1")),
        _ => panic!("expected message"),
    }

    // Read back.
    let found = storage.get_entry("e1").unwrap().unwrap();
    assert_eq!(found.id(), "e1");
    assert!(storage.get_entry("nope").unwrap().is_none());

    let entries = storage
        .find_entries(&EntryQuery::default())
        .unwrap();
    assert_eq!(entries.len(), 2);

    // Lane head moved.
    let lanes = storage.get_lanes().unwrap();
    assert_eq!(lanes[0].leaf_id.as_deref(), Some("e2"));

    // Records.
    let record = storage.append_record(operation_started("r1", "main")).unwrap();
    assert_eq!(record_seq(&record), 3.0);
    let open = storage.find_open_operations("main", None).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].base.id, "r1");

    // Name and label facts.
    storage.set_name(Some("my-session")).unwrap();
    assert_eq!(storage.get_name().unwrap().as_deref(), Some("my-session"));
    storage.set_label("e1", Some("important")).unwrap();
    assert_eq!(storage.get_label("e1").unwrap().as_deref(), Some("important"));
    storage.set_label("e1", None).unwrap();
    assert_eq!(storage.get_label("e1").unwrap(), None);

    // Stats.
    let stats = storage.get_stats().unwrap();
    assert_eq!(stats.message_count, 2.0);

    // Metadata.
    let metadata = storage.get_metadata().unwrap();
    assert_eq!(metadata.id, "s1".to_string().as_str().to_string().replace("s1", &storage.session_id_for_test()));

    // Log.
    let log = storage.get_log(&Default::default()).unwrap();
    assert!(log.len() >= 4);

    storage.release().unwrap();
    repository.close().unwrap();
}

fn record_seq(record: &LaneRecord) -> f64 {
    match record {
        LaneRecord::OperationStarted(record) => record.base.seq,
        _ => 0.0,
    }
}

#[test]
fn list_and_delete() {
    let repository = repo();
    let storage = repository
        .create_session(None, "/tmp", None, None)
        .unwrap();
    let session_id = storage.session_id_for_test();
    storage.release().unwrap();

    let sessions = repository.list_sessions(None).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, session_id);

    repository
        .delete_session(&SessionMetadata {
            id: session_id.clone(),
            created_at: 0.0,
            parent_session_id: None,
        })
        .unwrap();
    let sessions = repository.list_sessions(None).unwrap();
    assert!(sessions.is_empty());
}

#[test]
fn duplicate_id_rejected() {
    let repository = repo();
    let storage = repository
        .create_session(None, "/tmp", None, None)
        .unwrap();
    storage.append_entry(user_entry("e1"), "main").unwrap();
    let error = storage.append_entry(user_entry("e1"), "main").unwrap_err();
    assert_eq!(error.code, "already_exists");
}

#[test]
fn reopen_reuses_active_storage_and_works_after_release() {
    let repository = repo();
    let storage = repository
        .create_session(None, "/tmp", None, None)
        .unwrap();
    let session_id = storage.session_id_for_test();

    // A second open while active reuses the same storage (JS semantics).
    let reopened = repository
        .open_session(&SessionMetadata {
            id: session_id.clone(),
            created_at: 0.0,
            parent_session_id: None,
        })
        .unwrap();
    reopened.append_entry(user_entry("via-reopen"), "main").unwrap();

    // After release, the session can be reopened with a fresh lease.
    storage.release().unwrap();
    let reopened = repository
        .open_session(&SessionMetadata {
            id: session_id,
            created_at: 0.0,
            parent_session_id: None,
        })
        .unwrap();
    assert_eq!(reopened.get_name().unwrap(), None);
    reopened.release().unwrap();
}

fn _unused(_: SessionError, _: SessionStats, _: Value) {}
