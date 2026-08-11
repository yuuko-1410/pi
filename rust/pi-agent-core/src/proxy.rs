//! Proxy stream function, port of `packages/agent/src/proxy.ts`.
//!
//! Routes LLM calls through a server that manages auth and proxies to LLM
//! providers. The server strips the `partial` field from delta events; the
//! client reconstructs the partial message.

use pi_ai::event_stream::AssistantMessageEventStream;
use pi_ai::http::client::HttpClient;
use pi_ai::types::{
    AssistantMessage, AssistantMessageEvent, Content, Context, Message, Model, StopReason, TextContent,
    ThinkingContent, ToolCall, Usage, UsageCost,
};
use pi_ai::utils::json::{json_stringify, parse_streaming_json};
use pi_protocol::Value;

use crate::harness::session_types::JsonValue;

/// Proxy event types — the server sends these with the partial field
/// stripped to reduce bandwidth.
#[derive(Clone, Debug, PartialEq)]
pub enum ProxyAssistantMessageEvent {
    Start,
    TextStart { content_index: f64 },
    TextDelta { content_index: f64, delta: String },
    TextEnd { content_index: f64, content_signature: Option<String> },
    ThinkingStart { content_index: f64 },
    ThinkingDelta { content_index: f64, delta: String },
    ThinkingEnd { content_index: f64, content_signature: Option<String> },
    ToolCallStart {
        content_index: f64,
        id: String,
        tool_name: String,
    },
    ToolCallDelta { content_index: f64, delta: String },
    ToolCallEnd { content_index: f64, tool_call: ToolCall },
    Done { reason: String, usage: Usage },
    Error {
        reason: String,
        error_message: Option<String>,
        usage: Usage,
    },
}

fn get_str<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a str> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_str())
}

fn get_num(entries: &[(String, Value)], key: &str) -> Option<f64> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_number())
}

fn get_obj<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a [(String, Value)]> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_map())
}

fn parse_usage(entries: &[(String, Value)]) -> Usage {
    let cost = get_obj(entries, "cost");
    Usage {
        input: get_num(entries, "input").unwrap_or(0.0),
        output: get_num(entries, "output").unwrap_or(0.0),
        cache_read: get_num(entries, "cacheRead").unwrap_or(0.0),
        cache_write: get_num(entries, "cacheWrite").unwrap_or(0.0),
        cache_write_1h: get_num(entries, "cacheWrite1h"),
        reasoning: get_num(entries, "reasoning"),
        total_tokens: get_num(entries, "totalTokens").unwrap_or(0.0),
        cost: UsageCost {
            input: cost.and_then(|c| get_num(c, "input")).unwrap_or(0.0),
            output: cost.and_then(|c| get_num(c, "output")).unwrap_or(0.0),
            cache_read: cost.and_then(|c| get_num(c, "cacheRead")).unwrap_or(0.0),
            cache_write: cost.and_then(|c| get_num(c, "cacheWrite")).unwrap_or(0.0),
            total: cost.and_then(|c| get_num(c, "total")).unwrap_or(0.0),
        },
    }
}

/// Parses a proxy event from its JSON form.
pub fn parse_proxy_event(data: &str) -> Option<ProxyAssistantMessageEvent> {
    let value: Value = pi_ai::utils::json::parse_json_with_repair(data).ok()?;
    let entries = value.as_map()?;
    let type_ = get_str(entries, "type")?;
    let content_index = get_num(entries, "contentIndex").unwrap_or(0.0);
    Some(match type_ {
        "start" => ProxyAssistantMessageEvent::Start,
        "text_start" => ProxyAssistantMessageEvent::TextStart { content_index },
        "text_delta" => ProxyAssistantMessageEvent::TextDelta {
            content_index,
            delta: get_str(entries, "delta").unwrap_or("").to_string(),
        },
        "text_end" => ProxyAssistantMessageEvent::TextEnd {
            content_index,
            content_signature: get_str(entries, "contentSignature").map(|s| s.to_string()),
        },
        "thinking_start" => ProxyAssistantMessageEvent::ThinkingStart { content_index },
        "thinking_delta" => ProxyAssistantMessageEvent::ThinkingDelta {
            content_index,
            delta: get_str(entries, "delta").unwrap_or("").to_string(),
        },
        "thinking_end" => ProxyAssistantMessageEvent::ThinkingEnd {
            content_index,
            content_signature: get_str(entries, "contentSignature").map(|s| s.to_string()),
        },
        "toolcall_start" => ProxyAssistantMessageEvent::ToolCallStart {
            content_index,
            id: get_str(entries, "id").unwrap_or("").to_string(),
            tool_name: get_str(entries, "toolName").unwrap_or("").to_string(),
        },
        "toolcall_delta" => ProxyAssistantMessageEvent::ToolCallDelta {
            content_index,
            delta: get_str(entries, "delta").unwrap_or("").to_string(),
        },
        "toolcall_end" => ProxyAssistantMessageEvent::ToolCallEnd {
            content_index,
            tool_call: parse_tool_call(get_obj(entries, "toolCall")?)?,
        },
        "done" => ProxyAssistantMessageEvent::Done {
            reason: get_str(entries, "reason").unwrap_or("stop").to_string(),
            usage: parse_usage(get_obj(entries, "usage").unwrap_or(&[])),
        },
        "error" => ProxyAssistantMessageEvent::Error {
            reason: get_str(entries, "reason").unwrap_or("error").to_string(),
            error_message: get_str(entries, "errorMessage").map(|s| s.to_string()),
            usage: parse_usage(get_obj(entries, "usage").unwrap_or(&[])),
        },
        _ => return None,
    })
}

fn parse_tool_call(entries: &[(String, Value)]) -> Option<ToolCall> {
    Some(ToolCall {
        id: get_str(entries, "id")?.to_string(),
        name: get_str(entries, "name")?.to_string(),
        arguments: entries
            .iter()
            .find(|(k, _)| k == "arguments")
            .map(|(_, v)| v.clone())
            .unwrap_or(Value::Map(Vec::new())),
        thought_signature: get_str(entries, "thoughtSignature").map(|s| s.to_string()),
        namespace: get_str(entries, "namespace").map(|s| s.to_string()),
    })
}

fn empty_usage() -> Usage {
    Usage {
        input: 0.0,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0.0,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
    }
}

/// Process a proxy event and update the partial message, mirroring
/// `processProxyEvent`.
pub fn process_proxy_event(
    proxy_event: &ProxyAssistantMessageEvent,
    partial: &mut AssistantMessage,
) -> Option<AssistantMessageEvent> {
    match proxy_event {
        ProxyAssistantMessageEvent::Start => Some(AssistantMessageEvent::Start {
            partial: partial.clone(),
        }),
        ProxyAssistantMessageEvent::TextStart { content_index } => {
            set_content(partial, *content_index, Content::Text(TextContent {
                text: String::new(),
                text_signature: None,
            }));
            Some(AssistantMessageEvent::TextStart {
                content_index: *content_index,
                partial: partial.clone(),
            })
        }
        ProxyAssistantMessageEvent::TextDelta {
            content_index,
            delta,
        } => {
            let index = *content_index as usize;
            let Some(Content::Text(text)) = partial.content.get_mut(index) else {
                return None;
            };
            text.text.push_str(delta);
            Some(AssistantMessageEvent::TextDelta {
                content_index: *content_index,
                delta: delta.clone(),
                partial: partial.clone(),
            })
        }
        ProxyAssistantMessageEvent::TextEnd {
            content_index,
            content_signature,
        } => {
            let index = *content_index as usize;
            let Some(Content::Text(text)) = partial.content.get_mut(index) else {
                return None;
            };
            text.text_signature = content_signature.clone();
            Some(AssistantMessageEvent::TextEnd {
                content_index: *content_index,
                content: text.text.clone(),
                partial: partial.clone(),
            })
        }
        ProxyAssistantMessageEvent::ThinkingStart { content_index } => {
            set_content(partial, *content_index, Content::Thinking(ThinkingContent {
                thinking: String::new(),
                thinking_signature: None,
                redacted: None,
            }));
            Some(AssistantMessageEvent::ThinkingStart {
                content_index: *content_index,
                partial: partial.clone(),
            })
        }
        ProxyAssistantMessageEvent::ThinkingDelta {
            content_index,
            delta,
        } => {
            let index = *content_index as usize;
            let Some(Content::Thinking(thinking)) = partial.content.get_mut(index) else {
                return None;
            };
            thinking.thinking.push_str(delta);
            Some(AssistantMessageEvent::ThinkingDelta {
                content_index: *content_index,
                delta: delta.clone(),
                partial: partial.clone(),
            })
        }
        ProxyAssistantMessageEvent::ThinkingEnd {
            content_index,
            content_signature,
        } => {
            let index = *content_index as usize;
            let Some(Content::Thinking(thinking)) = partial.content.get_mut(index) else {
                return None;
            };
            thinking.thinking_signature = content_signature.clone();
            Some(AssistantMessageEvent::ThinkingEnd {
                content_index: *content_index,
                content: thinking.thinking.clone(),
                partial: partial.clone(),
            })
        }
        ProxyAssistantMessageEvent::ToolCallStart {
            content_index,
            id,
            tool_name,
        } => {
            let mut tool_call = ToolCall {
                id: id.clone(),
                name: tool_name.clone(),
                arguments: Value::Map(Vec::new()),
                thought_signature: None,
                namespace: None,
            };
            tool_call.arguments = Value::Map(Vec::new());
            set_content(partial, *content_index, Content::ToolCall(tool_call));
            Some(AssistantMessageEvent::ToolCallStart {
                content_index: *content_index,
                partial: partial.clone(),
            })
        }
        ProxyAssistantMessageEvent::ToolCallDelta {
            content_index,
            delta,
        } => {
            let index = *content_index as usize;
            let Some(Content::ToolCall(tool_call)) = partial.content.get_mut(index) else {
                return None;
            };
            // The proxy server streams incremental JSON text in `delta`;
            // accumulate via the partial-streaming parser over the appended
            // text. The current arguments are re-serialized and appended.
            let current = json_stringify(&tool_call.arguments);
            let merged = if current == "{}" {
                delta.clone()
            } else {
                format!("{current}{delta}")
            };
            tool_call.arguments = parse_streaming_json(Some(&merged));
            Some(AssistantMessageEvent::ToolCallDelta {
                content_index: *content_index,
                delta: delta.clone(),
                partial: partial.clone(),
            })
        }
        ProxyAssistantMessageEvent::ToolCallEnd {
            content_index,
            tool_call: final_tool_call,
        } => {
            let index = *content_index as usize;
            let Some(Content::ToolCall(content)) = partial.content.get_mut(index) else {
                return None;
            };
            *content = final_tool_call.clone();
            Some(AssistantMessageEvent::ToolCallEnd {
                content_index: *content_index,
                tool_call: final_tool_call.clone(),
                partial: partial.clone(),
            })
        }
        ProxyAssistantMessageEvent::Done { reason, usage } => {
            partial.stop_reason = StopReason::parse(reason).unwrap_or(StopReason::Stop);
            partial.usage = usage.clone();
            Some(AssistantMessageEvent::Done {
                reason: reason.clone(),
                message: partial.clone(),
            })
        }
        ProxyAssistantMessageEvent::Error {
            reason,
            error_message,
            usage,
        } => {
            partial.stop_reason = StopReason::parse(reason).unwrap_or(StopReason::Error);
            partial.error_message = error_message.clone();
            partial.usage = usage.clone();
            Some(AssistantMessageEvent::Error {
                reason: reason.clone(),
                error: partial.clone(),
            })
        }
    }
}

fn set_content(partial: &mut AssistantMessage, content_index: f64, content: Content) {
    let index = content_index as usize;
    if partial.content.len() <= index {
        partial.content.resize(index + 1, Content::Text(TextContent {
            text: String::new(),
            text_signature: None,
        }));
    }
    partial.content[index] = content;
}

/// Stream function that proxies through a server. The server strips the
/// partial field from delta events; we reconstruct it client-side.
pub fn stream_proxy(
    model: &Model,
    context: &Context,
    auth_token: &str,
    proxy_url: &str,
    client: &HttpClient,
) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let worker_stream = stream.clone();
    let model = model.clone();
    let context = context.clone();
    let auth_token = auth_token.to_string();
    let proxy_url = proxy_url.to_string();
    let client = client.clone();

    std::thread::spawn(move || {
        let mut partial = AssistantMessage {
            content: vec![],
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_model: None,
            response_id: None,
            usage: empty_usage(),
            stop_reason: StopReason::Pending,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: now_ms(),
        };

        let body = Value::Map(vec![
            ("model".to_string(), model_to_value(&model)),
            ("context".to_string(), context_to_value(&context)),
        ]);

        let result = (|| -> Result<(), String> {
            let url = format!("{}/api/stream", proxy_url.trim_end_matches('/'));
            let headers = vec![
                ("Authorization".to_string(), format!("Bearer {auth_token}")),
                ("Content-Type".to_string(), "application/json".to_string()),
            ];
            let response = client
                .post_json(&url, &headers, &body, None)
                .map_err(|error| error.message.clone())?;
            if !(200..300).contains(&response.status) {
                return Err(format!("Proxy error: {}", response.status));
            }

            crate::harness::session_state::assert_valid_limit(None).ok();
            let mut events = Vec::new();
            pi_ai::http::client::read_sse_stream(response.reader, |sse| {
                let data = sse.data.trim();
                if data.is_empty() || !sse.data.starts_with("data:") {
                    // SseParser already strips the field; handle bare JSON too.
                }
                if let Some(proxy_event) = parse_proxy_event(data) {
                    if let Some(event) = process_proxy_event(&proxy_event, &mut partial) {
                        events.push(event);
                    }
                }
            });
            for event in events {
                worker_stream.push(event);
            }

            worker_stream.end(None);
            Ok(())
        })();

        if let Err(error_message) = result {
            partial.stop_reason = StopReason::Error;
            partial.error_message = Some(error_message);
            worker_stream.push(AssistantMessageEvent::Error {
                reason: "error".to_string(),
                error: partial,
            });
            worker_stream.end(None);
        }
    });

    stream
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as f64
}

// ---------------------------------------------------------------------------
// Model/Context JSON serialization for the proxy request body
// ---------------------------------------------------------------------------

fn model_to_value(model: &Model) -> Value {
    Value::Map(vec![
        ("id".to_string(), Value::String(model.id.clone())),
        ("name".to_string(), Value::String(model.name.clone())),
        ("api".to_string(), Value::String(model.api.clone())),
        ("provider".to_string(), Value::String(model.provider.clone())),
        ("baseUrl".to_string(), Value::String(model.base_url.clone())),
        ("reasoning".to_string(), Value::Bool(model.reasoning)),
        (
            "input".to_string(),
            Value::Array(model.input.iter().map(|kind| Value::String(kind.clone())).collect()),
        ),
        (
            "cost".to_string(),
            Value::Map(vec![
                ("input".to_string(), Value::Number(model.cost.rates.input)),
                ("output".to_string(), Value::Number(model.cost.rates.output)),
                ("cacheRead".to_string(), Value::Number(model.cost.rates.cache_read)),
                ("cacheWrite".to_string(), Value::Number(model.cost.rates.cache_write)),
            ]),
        ),
        ("contextWindow".to_string(), Value::Number(model.context_window)),
        ("maxTokens".to_string(), Value::Number(model.max_tokens)),
    ])
}

fn context_to_value(context: &Context) -> Value {
    let mut entries = Vec::new();
    if let Some(system_prompt) = &context.system_prompt {
        entries.push(("systemPrompt".to_string(), Value::String(system_prompt.clone())));
    }
    entries.push((
        "messages".to_string(),
        Value::Array(context.messages.iter().map(message_to_value).collect()),
    ));
    Value::Map(entries)
}

fn message_to_value(message: &Message) -> Value {
    match message {
        Message::User(user) => match &user.content {
            pi_ai::types::UserMessageContent::Text(text) => Value::Map(vec![
                ("role".to_string(), Value::String("user".to_string())),
                ("content".to_string(), Value::String(text.clone())),
                ("timestamp".to_string(), Value::Number(user.timestamp)),
            ]),
            pi_ai::types::UserMessageContent::Blocks(blocks) => Value::Map(vec![
                ("role".to_string(), Value::String("user".to_string())),
                (
                    "content".to_string(),
                    Value::Array(blocks.iter().map(content_to_value).collect()),
                ),
                ("timestamp".to_string(), Value::Number(user.timestamp)),
            ]),
        },
        Message::Assistant(assistant) => {
            let mut entries = vec![
                ("role".to_string(), Value::String("assistant".to_string())),
                (
                    "content".to_string(),
                    Value::Array(assistant.content.iter().map(content_to_value).collect()),
                ),
                ("api".to_string(), Value::String(assistant.api.clone())),
                ("provider".to_string(), Value::String(assistant.provider.clone())),
                ("model".to_string(), Value::String(assistant.model.clone())),
                ("usage".to_string(), usage_to_value(&assistant.usage)),
                (
                    "stopReason".to_string(),
                    Value::String(assistant.stop_reason.as_str().to_string()),
                ),
                ("timestamp".to_string(), Value::Number(assistant.timestamp)),
            ];
            if let Some(error_message) = &assistant.error_message {
                entries.push(("errorMessage".to_string(), Value::String(error_message.clone())));
            }
            Value::Map(entries)
        }
        Message::ToolResult(tool) => Value::Map(vec![
            ("role".to_string(), Value::String("toolResult".to_string())),
            ("toolCallId".to_string(), Value::String(tool.tool_call_id.clone())),
            ("toolName".to_string(), Value::String(tool.tool_name.clone())),
            (
                "content".to_string(),
                Value::Array(tool.content.iter().map(content_to_value).collect()),
            ),
            ("isError".to_string(), Value::Bool(tool.is_error)),
            ("timestamp".to_string(), Value::Number(tool.timestamp)),
        ]),
    }
}

fn content_to_value(content: &Content) -> Value {
    match content {
        Content::Text(text) => Value::Map(vec![
            ("type".to_string(), Value::String("text".to_string())),
            ("text".to_string(), Value::String(text.text.clone())),
        ]),
        Content::Thinking(thinking) => Value::Map(vec![
            ("type".to_string(), Value::String("thinking".to_string())),
            ("thinking".to_string(), Value::String(thinking.thinking.clone())),
        ]),
        Content::Image(image) => Value::Map(vec![
            ("type".to_string(), Value::String("image".to_string())),
            ("data".to_string(), Value::String(image.data.clone())),
            ("mimeType".to_string(), Value::String(image.mime_type.clone())),
        ]),
        Content::ToolCall(tool_call) => Value::Map(vec![
            ("type".to_string(), Value::String("toolCall".to_string())),
            ("id".to_string(), Value::String(tool_call.id.clone())),
            ("name".to_string(), Value::String(tool_call.name.clone())),
            ("arguments".to_string(), tool_call.arguments.clone()),
        ]),
    }
}

fn usage_to_value(usage: &Usage) -> Value {
    Value::Map(vec![
        ("input".to_string(), Value::Number(usage.input)),
        ("output".to_string(), Value::Number(usage.output)),
        ("cacheRead".to_string(), Value::Number(usage.cache_read)),
        ("cacheWrite".to_string(), Value::Number(usage.cache_write)),
        ("totalTokens".to_string(), Value::Number(usage.total_tokens)),
        (
            "cost".to_string(),
            Value::Map(vec![
                ("input".to_string(), Value::Number(usage.cost.input)),
                ("output".to_string(), Value::Number(usage.cost.output)),
                ("cacheRead".to_string(), Value::Number(usage.cost.cache_read)),
                ("cacheWrite".to_string(), Value::Number(usage.cost.cache_write)),
                ("total".to_string(), Value::Number(usage.cost.total)),
            ]),
        ),
    ])
}

/// Kept for call-site parity with the JS options pick.
pub type ProxySerializableStreamOptions = JsonValue;
