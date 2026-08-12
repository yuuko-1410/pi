//! Storage layer integration tests over an in-memory database.

use pi_sqlite::database::{query_run, RusqliteDatabase};
use pi_sqlite::migrations::apply_migrations;
use pi_sqlite::sql::SqlPart;
use pi_sqlite::storage::branch_tips::{insert_branch_tip, read_branch_tip_ids, update_branch_tip};
use pi_sqlite::storage::entries::{insert_entry_row, read_entry_rows, NewEntryRow, ReadEntryRowsOptions};
use pi_sqlite::storage::facts::{append_fact, read_latest_fact, read_latest_label_facts};
use pi_sqlite::storage::records::{
    append_record_row, read_open_operation_rows, read_record_rows, NewRecordRow, ReadRecordRowsOptions,
};
use pi_sqlite::storage::lanes::{
    create_initial_lane, create_lane, finish_lane_operation, read_lane_move_rows, read_lanes,
    start_lane_operation,
};
use pi_sqlite::storage::sessions::{
    decode_session_metadata, insert_session_row, read_session_row, read_session_rows, session_exists,
    NewSessionRow,
};
use pi_sqlite::storage::session_sequences::{advance_sequence, create_sequence, get_next_sequence};
use pi_sqlite::storage::session_stats::{add_usage_to_stats, create_stats, read_stats};
use pi_sqlite::storage::writer_leases::{acquire_writer_lease, release_writer_lease, renew_writer_lease};

fn new_db() -> RusqliteDatabase {
    let db = RusqliteDatabase::open_in_memory().unwrap();
    apply_migrations(&db).unwrap();
    db
}

#[test]
fn entries_roundtrip_and_queries() {
    let db = new_db();
    insert_entry_row(
        &db,
        "s1",
        &NewEntryRow {
            seq: 1.0,
            id: "e1".to_string(),
            parent_id: None,
            type_: "message".to_string(),
            timestamp: 1000.0,
            payload: "{\"role\":\"user\"}".to_string(),
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
            payload: "{\"role\":\"assistant\"}".to_string(),
        },
    )
    .unwrap();

    let rows = read_entry_rows(&db, "s1", &ReadEntryRowsOptions::default()).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, "e2");
    assert_eq!(rows[0].parent_id.as_deref(), Some("e1"));

    let rows = read_entry_rows(
        &db,
        "s1",
        &ReadEntryRowsOptions {
            after_seq: Some(1.0),
            oldest_first: Some(true),
            ..ReadEntryRowsOptions::default()
        },
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "e2");
}

#[test]
fn sequences_and_stats() {
    let db = new_db();
    create_sequence(&db, "s1", 1.0).unwrap();
    assert_eq!(get_next_sequence(&db, "s1").unwrap(), 1.0);
    advance_sequence(&db, "s1", 1.0).unwrap();
    assert_eq!(get_next_sequence(&db, "s1").unwrap(), 2.0);

    create_stats(&db, "s1", 0.0).unwrap();
    let usage = pi_ai::types::Usage {
        input: 10.0,
        output: 5.0,
        cache_read: 2.0,
        cache_write: 1.0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 18.0,
        cost: pi_ai::types::UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.5,
        },
    };
    add_usage_to_stats(&db, "s1", &usage).unwrap();
    let stats = read_stats(&db, "s1").unwrap();
    assert_eq!(stats.cached_tokens, 2.0);
    assert_eq!(stats.uncached_tokens, 11.0);
    assert_eq!(stats.total_tokens, 18.0);
    assert_eq!(stats.cost_total, 0.5);
}

#[test]
fn writer_leases_claim_renew_release() {
    let db = new_db();
    let lease = acquire_writer_lease(&db, "s1", "owner-a", 100.0, 200.0)
        .unwrap()
        .unwrap();
    assert_eq!(lease.fence, 1.0);
    assert_eq!(lease.owner_id, "owner-a");

    let mut lease = lease;
    assert!(renew_writer_lease(&db, "s1", &mut lease, 150.0, 300.0).unwrap());
    assert_eq!(lease.expires_at_ms, 300.0);

    assert!(acquire_writer_lease(&db, "s1", "owner-b", 250.0, 400.0).unwrap().is_none());

    let stolen = acquire_writer_lease(&db, "s1", "owner-b", 350.0, 500.0)
        .unwrap()
        .unwrap();
    assert_eq!(stolen.fence, 2.0);
    assert_eq!(stolen.owner_id, "owner-b");

    release_writer_lease(&db, "s1", &stolen).unwrap();
}

#[test]
fn facts_and_labels() {
    let db = new_db();
    append_fact(&db, "s1", 1.0, "label", Some("name"), Some("first")).unwrap();
    append_fact(&db, "s1", 2.0, "label", Some("name"), Some("second")).unwrap();
    append_fact(&db, "s1", 3.0, "other", None, Some("x")).unwrap();

    let latest = read_latest_fact(&db, "s1", "label", Some("name")).unwrap().unwrap();
    assert_eq!(latest.value.as_deref(), Some("second"));
    assert_eq!(latest.seq, 2.0);

    let labels = read_latest_label_facts(&db, "s1").unwrap();
    assert_eq!(labels, vec![("name".to_string(), "second".to_string())]);
}

#[test]
fn records_and_open_operations() {
    let db = new_db();
    query_run(
        &db,
        &pi_sqlite::sql! {
            "INSERT INTO lanes (session_id, lane, leaf_id, open_operation_id) VALUES (",
            SqlPart::Value("s1".into()),
            ", ",
            SqlPart::Value("main".into()),
            ", NULL, ",
            SqlPart::Value("r1".into()),
            ")"
        },
    )
    .unwrap();
    append_record_row(
        &db,
        "s1",
        &NewRecordRow {
            seq: 1.0,
            id: "r1".to_string(),
            lane: "main".to_string(),
            run_id: Some("run-1".to_string()),
            type_: "operation_started".to_string(),
            op_kind: Some("run".to_string()),
            timestamp: 1000.0,
            payload: "{}".to_string(),
        },
    )
    .unwrap();

    let open = read_open_operation_rows(&db, "s1", "main").unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, "r1");

    let rows = read_record_rows(
        &db,
        "s1",
        &ReadRecordRowsOptions {
            lane: Some("main".to_string()),
            ..ReadRecordRowsOptions::default()
        },
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].run_id.as_deref(), Some("run-1"));
}

#[test]
fn branch_tips_roundtrip() {
    let db = new_db();
    insert_branch_tip(&db, "s1", "tip-1", "branch-a").unwrap();
    insert_branch_tip(&db, "s1", "tip-2", "branch-b").unwrap();
    let tips = read_branch_tip_ids(&db, "s1").unwrap();
    assert_eq!(tips, vec!["tip-1".to_string(), "tip-2".to_string()]);
    assert!(update_branch_tip(&db, "s1", "branch-a", "tip-1", "tip-3").unwrap());
    assert!(!update_branch_tip(&db, "s1", "branch-a", "tip-1", "tip-4").unwrap());
    let tips = read_branch_tip_ids(&db, "s1").unwrap();
    assert_eq!(tips, vec!["tip-2".to_string(), "tip-3".to_string()]);
}

#[test]
fn lanes_create_move_operations() {
    let db = new_db();
    create_initial_lane(&db, "s1", "main", None).unwrap();
    let lanes = read_lanes(&db, "s1").unwrap();
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].lane, "main");

    // A lane pointing at a missing entry is a storage error.
    create_lane(&db, "s1", 2.0, "broken", Some("missing-id")).unwrap();
    let error = read_lanes(&db, "s1").unwrap_err();
    assert_eq!(error.code, "storage");

    // Open operation lifecycle.
    start_lane_operation(&db, "s1", "main", "run-1").unwrap();
    let error = start_lane_operation(&db, "s1", "main", "run-2").unwrap_err();
    assert!(error.message.contains("already has an open operation"));
    finish_lane_operation(&db, "s1", "main", "run-1").unwrap();
    start_lane_operation(&db, "s1", "main", "run-2").unwrap();

    // Lane moves are recorded.
    // createInitialLane records no move; only createLane does.
    let moves = read_lane_move_rows(&db, "s1", None, None).unwrap();
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0].lane, "broken");
    assert_eq!(moves[0].seq, 2.0);
}

#[test]
fn sessions_insert_read_decode() {
    let db = new_db();
    insert_session_row(
        &db,
        &NewSessionRow {
            id: "s1".to_string(),
            created_at: 1000.0,
            cwd: "/tmp".to_string(),
            parent_session_id: None,
            metadata: Some(pi_protocol::cbor::Value::Map(vec![(
                "app".to_string(),
                pi_protocol::cbor::Value::String("test".to_string()),
            )])),
        },
    )
    .unwrap();
    assert!(session_exists(&db, "s1").unwrap());
    assert!(!session_exists(&db, "nope").unwrap());

    let row = read_session_row(&db, "s1").unwrap().unwrap();
    assert_eq!(row.cwd, "/tmp");
    let decoded = decode_session_metadata(&row, "/db/pi.db").unwrap();
    assert_eq!(decoded.id, "s1");
    assert_eq!(decoded.path, "/db/pi.db");
    assert!(decoded.metadata.is_some());

    // Session name comes from the latest 'name' fact.
    append_fact(&db, "s1", 1.0, "name", None, Some("\"first\"")).unwrap();
    append_fact(&db, "s1", 2.0, "name", None, Some("\"second\"")).unwrap();
    let row = read_session_row(&db, "s1").unwrap().unwrap();
    assert!(row.has_session_name);
    let decoded = decode_session_metadata(&row, "/db/pi.db").unwrap();
    assert_eq!(decoded.name.as_deref(), Some("second"));

    // cwd-filtered listing.
    let rows = read_session_rows(&db, Some("/tmp")).unwrap();
    assert_eq!(rows.len(), 1);
    let rows = read_session_rows(&db, Some("/other")).unwrap();
    assert!(rows.is_empty());
}
