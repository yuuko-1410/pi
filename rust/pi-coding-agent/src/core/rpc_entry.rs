//! JSON-RPC entry point, port of `core/rpc-entry.ts`.
//!
//! The Rust port implements a minimal line-delimited JSON-RPC over stdio:
//! requests are `{"id", "method", "params"}`, responses
//! `{"id", "result"|"error"}`. The full RPC surface (session
//! CRUD/transcripts) is simplified; see ponytail notes.

use std::io::BufRead;
use std::sync::Arc;

use pi_protocol::Value;

use crate::core::agent_session::AgentSession;

fn parse_value(s: &str) -> Result<Value, String> {
    pi_ai::utils::json::parse_json_with_repair(s).map_err(|e| e.to_string())
}

fn get_str(value: &Value, key: &str) -> Option<String> {
    match value {
        Value::Map(entries) => entries
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
}

#[allow(dead_code)]
fn get_array(value: &Value, key: &str) -> Vec<Value> {
    match value {
        Value::Map(entries) => entries
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.as_array())
            .map(|a| a.to_vec())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Handle a single RPC request against the session. Returns (id, result)
/// where id is None for notifications.
pub fn handle_request(session: &Arc<AgentSession>, request: &Value) -> (Option<String>, Result<Value, String>) {
    let id = get_str(request, "id");
    let method = get_str(request, "method").unwrap_or_default();
    let params = get_str(request, "params")
        .and_then(|raw| parse_value(&raw).ok())
        .unwrap_or(Value::Null);

    let result = match method.as_str() {
        "session.info" => Ok(session_info_value(session)),
        "session.state" => Ok(session_state_value(session)),
        "session.prompt" => {
            match get_str(&params, "text") {
                Some(text) => session.prompt(&text, &Default::default()).map(|_| Value::Null),
                None => Err("prompt requires text".to_string()),
            }
        }
        "session.abort" => {
            session.abort();
            Ok(Value::Null)
        }
        "session.setModel" => {
            let provider = get_str(&params, "provider");
            let model_id = get_str(&params, "modelId");
            match (provider, model_id) {
                (Some(provider), Some(model_id)) => {
                    match session.model_runtime().get_model(&provider, &model_id) {
                        Some(model) => session.set_model(&model).map(|_| Value::Null),
                        None => Err(format!("Model not found: {provider}/{model_id}")),
                    }
                }
                _ => Err("setModel requires provider and modelId".to_string()),
            }
        }
        "session.setThinkingLevel" => {
            match get_str(&params, "level") {
                Some(level) => {
                    session.set_thinking_level(&level);
                    Ok(Value::Null)
                }
                None => Err("setThinkingLevel requires level".to_string()),
            }
        }
        "session.getMessages" => Ok(session_messages_value(session)),
        "session.export" => Ok(export_session_value(session)),
        "ping" => Ok(Value::String("pong".to_string())),
        "shutdown" => Err("shutdown".to_string()),
        other => Err(format!("Unknown method: {other}")),
    };

    (id, result)
}

fn session_info_value(session: &Arc<AgentSession>) -> Value {
    let id = session.get_session_id();
    let cwd = session.session_manager.lock().unwrap().get_cwd().to_string();
    Value::Map(vec![
        ("id".to_string(), Value::String(id)),
        ("cwd".to_string(), Value::String(cwd)),
        (
            "name".to_string(),
            session
                .get_session_name()
                .map(Value::String)
                .unwrap_or(Value::Null),
        ),
        (
            "model".to_string(),
            session
                .model()
                .map(|m| Value::String(format!("{}/{}", m.provider, m.id)))
                .unwrap_or(Value::Null),
        ),
    ])
}

fn session_state_value(session: &Arc<AgentSession>) -> Value {
    let state = session.state();
    Value::Map(vec![
        (
            "isStreaming".to_string(),
            Value::Bool(state.is_streaming),
        ),
        (
            "thinkingLevel".to_string(),
            Value::String(state.thinking_level.clone()),
        ),
    ])
}

fn session_messages_value(session: &Arc<AgentSession>) -> Value {
    let messages = session.messages();
    let mut items = Vec::new();
    for message in messages {
        use pi_agent_core::types::AgentMessage;
        match message {
            AgentMessage::Llm(pi_ai::types::Message::User(user)) => {
                let text = match user.content {
                    pi_ai::types::UserMessageContent::Text(text) => text,
                    pi_ai::types::UserMessageContent::Blocks(_) => String::new(),
                };
                items.push(Value::Map(vec![
                    ("role".to_string(), Value::String("user".to_string())),
                    ("text".to_string(), Value::String(text)),
                ]));
            }
            AgentMessage::Llm(pi_ai::types::Message::Assistant(assistant)) => {
                let text = assistant
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        pi_ai::types::Content::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                items.push(Value::Map(vec![
                    ("role".to_string(), Value::String("assistant".to_string())),
                    ("text".to_string(), Value::String(text)),
                ]));
            }
            _ => {}
        }
    }
    Value::Array(items)
}

fn export_session_value(session: &Arc<AgentSession>) -> Value {
    let entries = session.session_manager.lock().unwrap().get_entries();
    let mut items = Vec::new();
    for entry in &entries {
        items.push(crate::core::session_types::entry_to_json(entry));
    }
    Value::Array(items)
}

/// Run the RPC server loop over stdin/stdout (line-delimited JSON).
pub fn run_rpc_mode(session: Arc<AgentSession>) -> i32 {
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    while let Some(Ok(line)) = lines.next() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let request = match parse_value(line) {
            Ok(request) => request,
            Err(_) => {
                println!(r#"{{"error": "invalid JSON"}}"#);
                continue;
            }
        };
        let (id, result) = handle_request(&session, &request);
        let response = match result {
            Ok(result) => {
                let mut fields = vec![("result".to_string(), result)];
                if let Some(id) = id {
                    fields.insert(0, ("id".to_string(), Value::String(id)));
                }
                Value::Map(fields)
            }
            Err(error) => {
                if error == "shutdown" {
                    println!(r#"{{"result": null}}"#);
                    break;
                }
                let mut fields = vec![(
                    "error".to_string(),
                    Value::Map(vec![("message".to_string(), Value::String(error))]),
                )];
                if let Some(id) = id {
                    fields.insert(0, ("id".to_string(), Value::String(id)));
                }
                Value::Map(fields)
            }
        };
        println!("{}", pi_ai::utils::json::json_stringify(&response));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session() -> Arc<AgentSession> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let agent_dir = std::env::temp_dir().join(format!("pi-rpc-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&agent_dir).unwrap();
        let result = crate::core::sdk::create_agent_session(crate::core::sdk::CreateAgentSessionOptions {
            cwd: Some("/tmp".to_string()),
            agent_dir: Some(agent_dir.to_string_lossy().to_string()),
            ..Default::default()
        })
        .unwrap();
        result.session
    }

    #[test]
    fn handles_ping_and_session_info() {
        let session = make_session();
        let request = parse_value(r#"{"id": "1", "method": "ping"}"#).unwrap();
        let (id, result) = handle_request(&session, &request);
        assert_eq!(id.as_deref(), Some("1"));
        assert_eq!(result.unwrap(), Value::String("pong".to_string()));

        let request = parse_value(r#"{"id": "2", "method": "session.info"}"#).unwrap();
        let (_, result) = handle_request(&session, &request);
        let info = result.unwrap();
        assert!(get_str(&info, "id").is_some());
    }

    #[test]
    fn unknown_method_errors() {
        let session = make_session();
        let request = parse_value(r#"{"id": "3", "method": "nope"}"#).unwrap();
        let (id, result) = handle_request(&session, &request);
        assert_eq!(id.as_deref(), Some("3"));
        assert!(result.is_err());
    }

    #[test]
    fn prompt_without_model_errors() {
        let session = make_session();
        let prompt = parse_value(r#"{"method": "session.prompt", "params": {"text": "hello"}}"#).unwrap();
        let (_, result) = handle_request(&session, &prompt);
        // No model configured: prompt fails with a helpful message.
        assert!(result.is_err());
    }

    #[test]
    fn export_returns_array() {
        let session = make_session();
        let request = parse_value(r#"{"method": "session.export"}"#).unwrap();
        let (_, result) = handle_request(&session, &request);
        assert!(matches!(result.unwrap(), Value::Array(_)));
    }
}
