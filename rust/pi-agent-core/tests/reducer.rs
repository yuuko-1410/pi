//! Reducer tests: lane state reconstruction from recovery slices.

use pi_agent_core::harness::reducer::{
    reduce_lane_state, validate_record_log, EffectiveLaneConfiguration, LaneReductionInput, RecordLogSlice,
};
use pi_agent_core::harness::session_types::*;
use pi_agent_core::types::AgentMessage;
use pi_ai::types::{Message, UserMessage, UserMessageContent};

pub(crate) fn test_defaults() -> EffectiveLaneConfiguration {
    EffectiveLaneConfiguration {
        model: ("test".to_string(), "model".to_string()),
        thinking_level: "off".to_string(),
        active_tool_names: vec![],
    }
}

fn user_message(text: &str) -> AgentMessage {
    AgentMessage::Llm(Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_string()),
        timestamp: 1.0,
    }))
}

fn run_intent() -> RunIntent {
    RunIntent::Run {
        original_prompt: vec![user_message("hi")],
        initial_messages: vec![],
        system_prompt_override: None,
        resume_data: None,
    }
}

fn record(lane: &str, id: &str, seq: f64, _run_id: Option<&str>) -> RecordBase {
    RecordBase {
        id: id.to_string(),
        seq,
        lane: lane.to_string(),
        timestamp: seq,
    }
}

#[test]
fn idle_lane_reduces_to_no_operation() {
    let input = LaneReductionInput {
        lane: "main".to_string(),
        leaf_id: None,
        open_operations: vec![],
        records: vec![],
        entries: vec![],
        own_entries: vec![],
        configuration_entries: vec![],
        defaults: test_defaults(),
    };
    let result = reduce_lane_state(&input).unwrap();
    assert!(result.lane_state.operation.is_none());
    assert!(result.lane_state.pending_next_run.is_empty());
    assert!(result.terminal_failure.is_none());
}

#[test]
fn open_run_operation_reconstructs_state() {
    let started = OperationStartedRecord {
        base: record("main", "op-1", 1.0, None),
        source_leaf_id: None,
        intent: run_intent(),
    };
    let input = LaneReductionInput {
        lane: "main".to_string(),
        leaf_id: Some("e-1".to_string()),
        open_operations: vec![started.clone()],
        records: vec![LaneRecord::OperationStarted(started)],
        entries: vec![],
        own_entries: vec![],
        configuration_entries: vec![],
        defaults: test_defaults(),
    };
    let result = reduce_lane_state(&input).unwrap();
    let operation = result.lane_state.operation.expect("operation present");
    assert_eq!(operation.id, "op-1");
    assert_eq!(operation.kind, "run");
    assert!(!operation.aborting);
    assert!(operation.step.is_none());
}

#[test]
fn multiple_open_operations_are_corruption() {
    let first = OperationStartedRecord {
        base: record("main", "op-1", 1.0, None),
        source_leaf_id: None,
        intent: run_intent(),
    };
    let second = OperationStartedRecord {
        base: record("main", "op-2", 2.0, None),
        source_leaf_id: None,
        intent: run_intent(),
    };
    let error = validate_record_log(&RecordLogSlice {
        lane: "main".to_string(),
        open_operations: vec![first, second],
        records: vec![],
        entries: vec![],
    })
    .unwrap_err();
    assert_eq!(error.reason, "multiple_open_operations");
}

#[test]
fn step_attempts_must_be_consecutive() {
    let started = OperationStartedRecord {
        base: record("main", "op-1", 1.0, None),
        source_leaf_id: None,
        intent: run_intent(),
    };
    let attempt = StepAttemptRecord {
        base: record("main", "step-1", 2.0, Some("op-1")),
        run_id: "op-1".to_string(),
        step: "assistant".to_string(),
        attempt: 2.0, // expected 1
        result_entry_id: "r-1".to_string(),
        compaction_reason: None,
    };
    let error = validate_record_log(&RecordLogSlice {
        lane: "main".to_string(),
        open_operations: vec![started.clone()],
        records: vec![LaneRecord::OperationStarted(started), LaneRecord::StepAttempt(attempt)],
        entries: vec![],
    })
    .unwrap_err();
    assert_eq!(error.reason, "non_consecutive_attempt");
}

#[test]
fn record_after_finish_is_corruption() {
    let started = OperationStartedRecord {
        base: record("main", "op-1", 1.0, None),
        source_leaf_id: None,
        intent: run_intent(),
    };
    let finished = LaneRecord::OperationFinished(OperationFinishedRecord {
        base: record("main", "fin-1", 2.0, Some("op-1")),
        run_id: "op-1".to_string(),
        outcome: OperationOutcome::Completed,
        error: None,
    });
    let after = LaneRecord::AbortRequested(AbortRequestedRecord {
        base: record("main", "abort-1", 3.0, Some("op-1")),
        run_id: "op-1".to_string(),
    });
    let error = validate_record_log(&RecordLogSlice {
        lane: "main".to_string(),
        open_operations: vec![started.clone()],
        records: vec![LaneRecord::OperationStarted(started), finished, after],
        entries: vec![],
    })
    .unwrap_err();
    assert_eq!(error.reason, "record_after_finish");
}
