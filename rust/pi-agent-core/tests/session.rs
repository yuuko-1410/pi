//! Session storage tests: state machine, session view, fork.

use pi_agent_core::harness::session::{InMemorySessionRepo, InMemorySessionStorage, Session};
use pi_agent_core::harness::session_state::{MutKind, SessionState};
use pi_agent_core::harness::session_types::*;
use pi_agent_core::types::AgentMessage;
use pi_ai::types::{Message, UserMessage, UserMessageContent};

fn user_message(text: &str) -> AgentMessage {
    AgentMessage::Llm(Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_string()),
        timestamp: 1.0,
    }))
}

fn message_entry(id: &str, text: &str, parent: Option<String>, seq: f64) -> Entry {
    Entry::Message(MessageEntry {
        base: EntryBase {
            type_: "message".to_string(),
            id: id.to_string(),
            seq,
            parent_id: parent,
            timestamp: 1.0,
        },
        message: user_message(text),
        terminate: None,
    })
}

#[test]
fn state_appends_entries_and_queries() {
    let mut state = SessionState::new();
    state
        .apply_mutation(MutKind::Entry {
            lane: Some("main".to_string()),
            entry: message_entry("e1", "hello", None, state.next_sequence()),
        })
        .unwrap();
    let entry = state.get_entry("e1").cloned().unwrap();
    assert_eq!(entry.type_name(), "message");
    assert_eq!(state.get_lanes()[0].leaf_id.as_deref(), Some("e1"));

    // Second entry chains to the leaf.
    state
        .apply_mutation(MutKind::Entry {
            lane: Some("main".to_string()),
            entry: message_entry("e2", "world", Some("e1".to_string()), state.next_sequence()),
        })
        .unwrap();

    let entries = state.find_entries(&EntryQuery::default()).unwrap();
    assert_eq!(entries.len(), 2);
    // Newest first by default.
    assert_eq!(entries[0].id(), "e2");
    let oldest = state
        .find_entries(&EntryQuery {
            order: Some(EntryOrder::OldestFirst),
            ..EntryQuery::default()
        })
        .unwrap();
    assert_eq!(oldest[0].id(), "e1");

    // Branch scan from the leaf walks toward the root.
    let branch = state
        .find_entries_on_branch(&EntryQuery::default(), &BranchBounds::default(), "e2")
        .unwrap();
    assert_eq!(branch.len(), 2);
}

#[test]
fn state_rejects_non_consecutive_seqs() {
    let mut state = SessionState::new();
    let error = state
        .apply_mutation(MutKind::Lane {
            seq: 5.0,
            lane: "other".to_string(),
            leaf_id: None,
        })
        .unwrap_err();
    assert!(error.message.contains("non-consecutive"), "{error}");
}

#[test]
fn state_tracks_open_operations_and_stats() {
    let mut state = SessionState::new();
    state
        .apply_mutation(MutKind::Record {
            record: LaneRecord::OperationStarted(OperationStartedRecord {
                base: RecordBase {
                    id: "op-1".to_string(),
                    seq: 1.0,
                    lane: "main".to_string(),
                    timestamp: 1.0,
                },
                source_leaf_id: None,
                intent: RunIntent::Run {
                    original_prompt: vec![],
                    initial_messages: vec![],
                    system_prompt_override: None,
                    resume_data: None,
                },
            }),
        })
        .unwrap();
    let open = state.find_open_operations("main", None).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].base.id, "op-1");

    state
        .apply_mutation(MutKind::Record {
            record: LaneRecord::OperationFinished(OperationFinishedRecord {
                base: RecordBase {
                    id: "fin-1".to_string(),
                    seq: 2.0,
                    lane: "main".to_string(),
                    timestamp: 2.0,
                },
                run_id: "op-1".to_string(),
                outcome: OperationOutcome::Completed,
                error: None,
            }),
        })
        .unwrap();
    assert!(state.find_open_operations("main", None).unwrap().is_empty());
}

#[test]
fn session_appends_and_queries_through_storage() {
    let storage = InMemorySessionStorage::new(SessionMetadata {
        id: "s1".to_string(),
        created_at: 1.0,
        parent_session_id: None,
    })
    .unwrap();
    let session = Session::new(storage);

    let id = session.append_message(user_message("hi")).unwrap();
    assert_eq!(id, session.get_leaf_id().unwrap().unwrap());
    let entries = session.find_entries(&EntryQuery::default()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id(), id);
    let stats = session.get_stats().unwrap();
    assert_eq!(stats.message_count, 1.0);
}

#[test]
fn repo_creates_lists_and_forks() {
    let mut repo = InMemorySessionRepo::new();
    repo.create(&SessionCreateOptions {
        id: Some("a".to_string()),
        parent_session_id: None,
    })
    .unwrap();
    let metadata = repo.list().unwrap();
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0].id, "a");

    repo.fork(
        &metadata[0],
        &ForkOptions::Tree,
        &SessionCreateOptions {
            id: Some("b".to_string()),
            parent_session_id: None,
        },
    )
    .unwrap();
    assert_eq!(repo.list().unwrap().len(), 2);
    let listed = repo.list().unwrap();
    let child = listed.iter().find(|m| m.id == "b").unwrap();
    assert_eq!(child.parent_session_id.as_deref(), Some("a"));
}
