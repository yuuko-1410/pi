//! Transcript state for remote sessions, port of `client/transcript.ts`.

use std::collections::HashMap;

use pi_protocol::schemas::{
    AssistantOrTool, Content, FinishedItem, SessionSnapshot, TranscriptItem, TranscriptProgress,
};

#[cfg(test)]
use pi_protocol::schemas::AssistantItem;
use pi_protocol::Value;

#[derive(Clone, Debug)]
pub struct TranscriptState {
    pub snapshot: SessionSnapshot,
    pub progress_items: HashMap<String, TranscriptItem>,
    pub progress_order: Vec<String>,
    pub tool_call_buffers: HashMap<String, String>,
}

fn is_json_value(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => true,
        Value::Number(_) => true,
        Value::Array(items) => items.iter().all(is_json_value),
        Value::Map(entries) => entries.iter().all(|(_, v)| is_json_value(v)),
        Value::Bytes(_) => false,
    }
}

/// Partial tool arguments stay as a raw prefix until they form valid JSON.
fn parse_partial_tool_input(value: &str) -> Value {
    match pi_ai::utils::json::parse_json_with_repair(value) {
        Ok(parsed) if is_json_value(&parsed) => parsed,
        _ => Value::String(value.to_string()),
    }
}

pub fn create_transcript_state(snapshot: SessionSnapshot) -> TranscriptState {
    TranscriptState {
        snapshot,
        progress_items: HashMap::new(),
        progress_order: Vec::new(),
        tool_call_buffers: HashMap::new(),
    }
}

pub fn apply_transcript_snapshot(state: &TranscriptState, snapshot: SessionSnapshot) -> TranscriptState {
    if state.snapshot.id == snapshot.id && snapshot.revision < state.snapshot.revision {
        return state.clone();
    }
    create_transcript_state(snapshot)
}

fn item_id(item: &TranscriptItem) -> &str {
    match item {
        TranscriptItem::User(user) => &user.id,
        TranscriptItem::Assistant(assistant) => &assistant.id,
        TranscriptItem::Tool(tool) => &tool.id,
    }
}

fn set_progress_item(state: &TranscriptState, item: TranscriptItem) -> TranscriptState {
    let id = item_id(&item).to_string();
    let mut progress_items = state.progress_items.clone();
    let mut progress_order = state.progress_order.clone();
    if !progress_items.contains_key(&id) {
        progress_order.push(id.clone());
    }
    progress_items.insert(id, item);
    TranscriptState {
        snapshot: state.snapshot.clone(),
        progress_items,
        progress_order,
        tool_call_buffers: state.tool_call_buffers.clone(),
    }
}

fn item_from_progress(progress_item: &TranscriptItem) -> Option<&TranscriptItem> {
    Some(progress_item)
}

fn assistant_content_mut(item: &mut TranscriptItem) -> Option<&mut Vec<Content>> {
    match item {
        TranscriptItem::Assistant(assistant) => Some(&mut assistant.content),
        _ => None,
    }
}

pub fn apply_transcript_progress(state: &TranscriptState, progress: &TranscriptProgress) -> TranscriptState {
    match progress {
        TranscriptProgress::ItemStarted { item } => set_progress_item(state, item.clone()),
        TranscriptProgress::ItemUpdated { item } => match item {
            AssistantOrTool::Assistant(assistant) => {
                set_progress_item(state, TranscriptItem::Assistant(assistant.clone()))
            }
            AssistantOrTool::Tool(tool) => set_progress_item(state, TranscriptItem::Tool(tool.clone())),
        },
        TranscriptProgress::ItemFinished { item } => {
            let finished: TranscriptItem = match item {
                FinishedItem::AssistantComplete(assistant)
                | FinishedItem::AssistantError(assistant)
                | FinishedItem::AssistantAborted(assistant) => TranscriptItem::Assistant(assistant.clone()),
                FinishedItem::ToolComplete(tool) | FinishedItem::ToolError(tool) => TranscriptItem::Tool(tool.clone()),
            };
            let id = item_id(&finished).to_string();
            let mut tool_call_buffers = state.tool_call_buffers.clone();
            let prefix = format!("{id}:");
            tool_call_buffers.retain(|key, _| !key.starts_with(&prefix));
            set_progress_item(
                &TranscriptState {
                    snapshot: state.snapshot.clone(),
                    progress_items: state.progress_items.clone(),
                    progress_order: state.progress_order.clone(),
                    tool_call_buffers,
                },
                finished,
            )
        }
        TranscriptProgress::AssistantDelta {
            message_id,
            content_index,
            kind,
            delta,
        } => {
            let existing = state
                .progress_items
                .get(message_id)
                .cloned()
                .or_else(|| state.snapshot.transcript.iter().find(|item| item_id(item) == message_id).cloned());
            let Some(mut item) = existing else {
                return state.clone();
            };
            // Only assistant items receive deltas.
            if !matches!(item, TranscriptItem::Assistant(_)) {
                return state.clone();
            }
            let mut tool_call_buffers = state.tool_call_buffers.clone();
            let index = *content_index as usize;
            if let Some(content) = assistant_content_mut(&mut item) {
                if let Some(part) = content.get_mut(index) {
                    match (kind.as_str(), part) {
                        ("text", Content::Text { text }) => {
                            text.push_str(delta);
                        }
                        ("thinking", Content::Thinking { thinking, .. }) => {
                            thinking.push_str(delta);
                        }
                        ("toolCall", Content::ToolCall { input, .. }) => {
                            let key = format!("{message_id}:{content_index}");
                            let existing_buffer = state
                                .tool_call_buffers
                                .get(&key)
                                .cloned()
                                .unwrap_or_else(|| match input {
                                    Value::String(value) => value.clone(),
                                    _ => String::new(),
                                });
                            let buffer = format!("{existing_buffer}{delta}");
                            tool_call_buffers.insert(key, buffer.clone());
                            *input = parse_partial_tool_input(&buffer);
                        }
                        _ => {}
                    }
                }
            }
            let _ = item_from_progress(&item);
            set_progress_item(
                &TranscriptState {
                    snapshot: state.snapshot.clone(),
                    progress_items: state.progress_items.clone(),
                    progress_order: state.progress_order.clone(),
                    tool_call_buffers,
                },
                item,
            )
        }
    }
}

pub fn select_transcript(state: &TranscriptState) -> Vec<TranscriptItem> {
    let mut transcript: Vec<TranscriptItem> = state
        .snapshot
        .transcript
        .iter()
        .map(|item| state.progress_items.get(item_id(item)).cloned().unwrap_or_else(|| item.clone()))
        .collect();
    let mut ids: std::collections::HashSet<String> = transcript.iter().map(item_id).map(|s| s.to_string()).collect();
    for id in &state.progress_order {
        if ids.contains(id) {
            continue;
        }
        if let Some(item) = state.progress_items.get(id) {
            transcript.push(item.clone());
            ids.insert(id.clone());
        }
    }
    for item in &state.snapshot.queued_steer {
        if ids.contains(&item.id) {
            continue;
        }
        transcript.push(TranscriptItem::User(item.clone()));
        ids.insert(item.id.clone());
    }
    transcript
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::schemas::{AssistantStatus, ModelRef};

    fn model_ref() -> ModelRef {
        ModelRef {
            provider: "test".to_string(),
            id: "m".to_string(),
        }
    }

    fn snapshot(id: &str, revision: f64) -> SessionSnapshot {
        SessionSnapshot {
            id: id.to_string(),
            name: None,
            cwd: "/tmp".to_string(),
            created_at: 0.0,
            updated_at: 0.0,
            phase: "idle".to_string(),
            model: model_ref(),
            thinking_level: "off".to_string(),
            attached: true,
            locked: false,
            revision,
            transcript: Vec::new(),
            queued_steer: Vec::new(),
            queued_steer_count: 0.0,
        }
    }

    #[test]
    fn snapshot_replaces_older_revision() {
        let state = create_transcript_state(snapshot("s1", 1.0));
        let replaced = apply_transcript_snapshot(&state, snapshot("s1", 5.0));
        assert_eq!(replaced.snapshot.revision, 5.0);
        let kept = apply_transcript_snapshot(&replaced, snapshot("s1", 2.0));
        assert_eq!(kept.snapshot.revision, 5.0);
    }

    #[test]
    fn partial_tool_input_stays_raw_prefix() {
        let parsed = parse_partial_tool_input("{\"path\": \"/tm");
        assert!(matches!(parsed, Value::String(_)));
        let parsed2 = parse_partial_tool_input(r#"{"path": "/tmp"}"#);
        assert!(matches!(parsed2, Value::Map(_)));
    }

    #[test]
    fn assistant_delta_accumulates_text() {
        let mut snapshot = snapshot("s1", 1.0);
        snapshot.transcript = vec![TranscriptItem::Assistant(AssistantItem {
            id: "a1".to_string(),
            content: vec![Content::Text { text: "hello".to_string() }],
            model: model_ref(),
            response_model: None,
            usage: None,
            timestamp: 0.0,
            status: AssistantStatus::Streaming,
        })];
        let state = create_transcript_state(snapshot);
        let progress = TranscriptProgress::AssistantDelta {
            message_id: "a1".to_string(),
            content_index: 0.0,
            kind: "text".to_string(),
            delta: " world".to_string(),
        };
        let updated = apply_transcript_progress(&state, &progress);
        let selected = select_transcript(&updated);
        match &selected[0] {
            TranscriptItem::Assistant(assistant) => match &assistant.content[0] {
                Content::Text { text } => assert_eq!(text, "hello world"),
                _ => panic!("expected text"),
            },
            _ => panic!("expected assistant"),
        }
    }

    #[test]
    fn select_transcript_includes_progress_items() {
        let state = create_transcript_state(snapshot("s1", 1.0));
        let started = TranscriptProgress::ItemStarted {
            item: TranscriptItem::Assistant(AssistantItem {
                id: "streaming".to_string(),
                content: vec![],
                model: model_ref(),
                response_model: None,
                usage: None,
                timestamp: 0.0,
                status: AssistantStatus::Streaming,
            }),
        };
        let updated = apply_transcript_progress(&state, &started);
        let selected = select_transcript(&updated);
        assert_eq!(selected.len(), 1);
        assert_eq!(item_id(&selected[0]), "streaming");
    }
}
