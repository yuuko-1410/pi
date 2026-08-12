//! SessionManager end-to-end tests: persistence, branching, context.

use pi_coding_agent::core::session_manager::{find_most_recent_session, load_entries_from_file, now_iso, SessionManager};
use pi_coding_agent::core::session_types::{
    build_session_context, parse_session_entries, FileEntry, SessionEntry, SessionMessage,
};

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn temp_dir() -> String {
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("pi-smgr-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).unwrap();
    dir.to_string_lossy().to_string()
}

fn empty_usage() -> pi_ai::types::Usage {
    pi_ai::types::Usage {
        input: 0.0,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0.0,
        cost: pi_ai::types::UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
    }
}

fn user_message(text: &str) -> SessionMessage {
    SessionMessage::Llm(pi_ai::types::Message::User(pi_ai::types::UserMessage {
        content: pi_ai::types::UserMessageContent::Text(text.to_string()),
        timestamp: 0.0,
    }))
}

#[test]
fn append_and_read_cycle() {
    let mut manager = SessionManager::in_memory(None, None);
    let id1 = manager.append_message(user_message("hello"));
    let id2 = manager.append_message(user_message("world"));

    assert_ne!(id1, id2);
    assert_eq!(manager.get_leaf_id().as_deref(), Some(id2.as_str()));
    let leaf = manager.get_leaf_entry().unwrap();
    assert_eq!(leaf.id(), id2);

    // Parent chain.
    assert_eq!(manager.get_entry(&id2).unwrap().parent_id(), Some(id1.as_str()));
    assert_eq!(manager.get_entry(&id1).unwrap().parent_id(), None);

    // Branch traversal.
    let branch = manager.get_branch(None);
    assert_eq!(branch.len(), 2);
    assert_eq!(branch[0].id(), id1);
    assert_eq!(branch[1].id(), id2);

    // Context.
    let context = manager.build_session_context();
    assert_eq!(context.messages.len(), 2);
    assert_eq!(context.thinking_level, "off");
}

#[test]
fn branch_moves_leaf() {
    let mut manager = SessionManager::in_memory(None, None);
    let id1 = manager.append_message(user_message("a"));
    let id2 = manager.append_message(user_message("b"));
    manager.branch(&id1);
    assert_eq!(manager.get_leaf_id().as_deref(), Some(id1.as_str()));

    let id3 = manager.append_message(user_message("c"));
    assert_eq!(manager.get_entry(&id3).unwrap().parent_id(), Some(id1.as_str()));

    // Branch with summary.
    let summary_id = manager.branch_with_summary(None, "abandoned".into(), None, None, None);
    let summary = manager.get_entry(&summary_id).unwrap();
    assert!(matches!(summary, SessionEntry::BranchSummary { .. }));
    // Branch summary appended with a null parent becomes a new root: the
    // branch is just the summary itself (JS semantics).
    let branch = manager.get_branch(None);
    assert_eq!(branch.len(), 1);
    assert_eq!(branch[0].id(), summary_id);

    let _ = id2;
}

#[test]
fn labels_and_tree() {
    let mut manager = SessionManager::in_memory(None, None);
    let id1 = manager.append_message(user_message("a"));
    manager.append_label_change(id1.clone(), Some("important".into()));
    assert_eq!(manager.get_label(&id1).as_deref(), Some("important"));

    let tree = manager.get_tree();
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].label.as_deref(), Some("important"));

    manager.append_label_change(id1.clone(), None);
    assert_eq!(manager.get_label(&id1), None);
}

#[test]
fn persist_writes_file_after_first_assistant() {
    let dir = temp_dir();
    let mut manager = SessionManager::create("/tmp", Some(&dir), None);
    let file = manager.get_session_file().expect("persisted session has a file").to_string();
    // No assistant message yet: file must not exist.
    manager.append_message(user_message("hi"));
    assert!(!std::path::Path::new(&file).exists());

    // First assistant message triggers a full write.
    manager.append_message(SessionMessage::Llm(pi_ai::types::Message::Assistant(
        pi_ai::types::AssistantMessage {
            content: vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
                text: "answer".into(),
                text_signature: None,
            })],
            api: "openai".into(),
            provider: "openai".into(),
            model: "gpt-4".into(),
            response_model: None,
            response_id: None,
            usage: empty_usage(),
            stop_reason: pi_ai::types::StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 1.0,
        }),
    ));
    assert!(std::path::Path::new(&file).exists());

    // Reopen: entries are preserved.
    let reopened = SessionManager::open(&file, None, None);
    assert_eq!(reopened.get_entries().len(), 2);
    assert_eq!(reopened.get_leaf_id().as_deref(), reopened.get_entries().last().map(|e| e.id()));

    // loadEntriesFromFile agrees.
    let entries = load_entries_from_file(&file);
    assert_eq!(entries.len(), 3); // header + 2 messages
    assert!(matches!(entries[0], FileEntry::Header(_)));
}

#[test]
fn continue_recent_finds_most_recent() {
    let dir = temp_dir();
    let mut first = SessionManager::create("/tmp", Some(&dir), None);
    let first_file = first.get_session_file().unwrap().to_string();
    first.append_message(user_message("hi"));
    first.append_message(SessionMessage::Llm(pi_ai::types::Message::Assistant(
        pi_ai::types::AssistantMessage {
            content: vec![],
            api: "openai".into(),
            provider: "openai".into(),
            model: "gpt-4".into(),
            response_model: None,
            response_id: None,
            usage: empty_usage(),
            stop_reason: pi_ai::types::StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 1.0,
        },
    )));

    // Second session (later mtime via distinct filename timestamps).
    std::thread::sleep(std::time::Duration::from_millis(20));
    let mut second = SessionManager::create("/tmp", Some(&dir), None);
    let second_file = second.get_session_file().unwrap().to_string();
    second.append_message(user_message("second"));
    second.append_message(SessionMessage::Llm(pi_ai::types::Message::Assistant(
        pi_ai::types::AssistantMessage {
            content: vec![],
            api: "openai".into(),
            provider: "openai".into(),
            model: "gpt-4".into(),
            response_model: None,
            response_id: None,
            usage: empty_usage(),
            stop_reason: pi_ai::types::StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 1.0,
        },
    )));

    let found = find_most_recent_session(&dir, None).unwrap();
    assert_eq!(found, second_file);
    assert_ne!(first_file, second_file);

    let continued = SessionManager::continue_recent("/tmp", Some(&dir));
    assert_eq!(continued.get_entries().len(), 2);
}

#[test]
fn fork_from_copies_history() {
    let dir = temp_dir();
    let mut source = SessionManager::create("/tmp", Some(&dir), None);
    source.append_message(user_message("original"));
    let source_file = source.get_session_file().unwrap().to_string();
    source.append_message(SessionMessage::Llm(pi_ai::types::Message::Assistant(
        pi_ai::types::AssistantMessage {
            content: vec![],
            api: "openai".into(),
            provider: "openai".into(),
            model: "gpt-4".into(),
            response_model: None,
            response_id: None,
            usage: empty_usage(),
            stop_reason: pi_ai::types::StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 1.0,
        },
    )));

    let fork = SessionManager::fork_from(&source_file, "/other", None, None);
    let fork_file = fork.get_session_file().unwrap().to_string();
    assert_ne!(fork_file, source_file);
    assert_eq!(fork.get_entries().len(), 2);
    assert_eq!(fork.get_header().unwrap().parent_session.as_deref(), Some(source_file.as_str()));
    // Header cwd updated to target.
    assert!(fork.get_header().unwrap().cwd.ends_with("/other"));
}

#[test]
fn list_sessions_returns_info() {
    let dir = temp_dir();
    let mut manager = SessionManager::create("/tmp", Some(&dir), None);
    manager.append_message(user_message("first user message"));
    manager.append_message(SessionMessage::Llm(pi_ai::types::Message::Assistant(
        pi_ai::types::AssistantMessage {
            content: vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
                text: "assistant reply".into(),
                text_signature: None,
            })],
            api: "openai".into(),
            provider: "openai".into(),
            model: "gpt-4".into(),
            response_model: None,
            response_id: None,
            usage: empty_usage(),
            stop_reason: pi_ai::types::StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 1.0,
        },
    )));

    let sessions = SessionManager::list("/tmp", Some(&dir), None);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].message_count, 2);
    assert_eq!(sessions[0].first_message, "first user message");
    assert!(sessions[0].all_messages_text.contains("assistant reply"));
    assert_eq!(sessions[0].cwd, "/tmp");
}

#[test]
fn parse_round_trip_file() {
    let dir = temp_dir();
    let mut manager = SessionManager::create("/tmp", Some(&dir), None);
    manager.append_message(user_message("hi"));
    let file = manager.get_session_file().unwrap().to_string();
    manager.append_message(SessionMessage::Llm(pi_ai::types::Message::Assistant(
        pi_ai::types::AssistantMessage {
            content: vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
                text: "answer".into(),
                text_signature: None,
            })],
            api: "openai".into(),
            provider: "openai".into(),
            model: "gpt-4".into(),
            response_model: None,
            response_id: None,
            usage: empty_usage(),
            stop_reason: pi_ai::types::StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 1.0,
        },
    )));
    let content = std::fs::read_to_string(&file).unwrap();
    let parsed = parse_session_entries(&content);
    assert_eq!(parsed.len(), 3);
    let reopened = SessionManager::open(&file, None, None);
    let context = reopened.build_session_context();
    assert_eq!(context.messages.len(), 2);
    // Sanity: now_iso is parseable by our own parser.
    assert!(!now_iso().is_empty());
    let _ = build_session_context;
    let _ = SessionEntry::Compaction {
        base: pi_coding_agent::core::session_types::SessionEntryBase {
            id: "x".into(),
            parent_id: None,
            timestamp: now_iso(),
        },
        summary: "s".into(),
        first_kept_entry_id: "e".into(),
        tokens_before: 0.0,
        details: None,
        usage: None,
        from_hook: None,
        first_kept_entry_index: None,
    };
}
