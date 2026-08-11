//! End-to-end agent loop tests with a faux provider stream function.

use std::sync::Arc;

use pi_agent_core::agent_loop::{agent_loop, AgentLoopConfig};
use pi_agent_core::types::{
    AgentContext, AgentEvent, AgentMessage, AgentTool, AgentToolResult, ToolExecutionMode,
};
use pi_ai::event_stream::AssistantMessageEventStream;
use pi_ai::types::{
    AssistantMessage, Content, Context, Message, Model, ModelCost, ModelCostRates, SimpleStreamOptions,
    StopReason, TextContent, Tool, ToolCall, Usage, UsageCost,
};

fn model() -> Model {
    Model {
        id: "faux".to_string(),
        name: "Faux".to_string(),
        api: "faux".to_string(),
        provider: "faux".to_string(),
        base_url: "https://faux".to_string(),
        reasoning: false,
        thinking_level_map: None,
        input: vec!["text".to_string()],
        cost: ModelCost {
            rates: ModelCostRates {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            tiers: None,
        },
        context_window: 1000.0,
        max_tokens: 100.0,
        sampling_params: None,
        headers: None,
        compat: None,
    }
}

fn usage() -> Usage {
    Usage {
        input: 1.0,
        output: 1.0,
        cache_read: 0.0,
        cache_write: 0.0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 2.0,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
    }
}

fn assistant(content: Vec<Content>, stop_reason: StopReason) -> AssistantMessage {
    AssistantMessage {
        content,
        api: "faux".to_string(),
        provider: "faux".to_string(),
        model: "faux".to_string(),
        response_model: None,
        response_id: None,
        usage: usage(),
        stop_reason,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1.0,
    }
}

fn tool_call(name: &str, id: &str) -> Content {
    Content::ToolCall(ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: pi_ai::types::JsonValue::Map(Vec::new()),
        thought_signature: None,
        namespace: None,
    })
}

fn text(text: &str) -> Content {
    Content::Text(TextContent {
        text: text.to_string(),
        text_signature: None,
    })
}

/// A scripted stream fn: returns one assistant message per call from a queue.
fn scripted_stream_fn(responses: Vec<AssistantMessage>) -> Arc<dyn Fn(&Model, &Context, Option<&SimpleStreamOptions>) -> AssistantMessageEventStream + Send + Sync> {
    let responses = Arc::new(std::sync::Mutex::new(responses.into_iter()));
    Arc::new(move |_model: &Model, _context: &Context, _options: Option<&SimpleStreamOptions>| {
        let stream = AssistantMessageEventStream::new();
        let mut queue = responses.lock().unwrap();
        let message = queue.next().expect("scripted response exhausted");
        stream.push(pi_ai::types::AssistantMessageEvent::Start {
            partial: message.clone(),
        });
        stream.push(pi_ai::types::AssistantMessageEvent::Done {
            reason: message.stop_reason.as_str().to_string(),
            message,
        });
        stream
    })
}

fn default_config() -> AgentLoopConfig {
    AgentLoopConfig {
        model: model(),
        reasoning: None,
        api_key: None,
        convert_to_llm: Arc::new(|messages: &[AgentMessage]| {
            messages
                .iter()
                .filter_map(|message| match message {
                    AgentMessage::Llm(message) => Some(message.clone()),
                    AgentMessage::Custom(_) => None,
                })
                .collect()
        }),
        transform_context: None,
        get_api_key: None,
        should_stop_after_turn: None,
        prepare_next_turn: None,
        get_steering_messages: None,
        get_follow_up_messages: None,
        tool_execution: ToolExecutionMode::Parallel,
        before_tool_call: None,
        after_tool_call: None,
    }
}

fn collect_events(stream: pi_ai::event_stream::EventStream<AgentEvent, Vec<AgentMessage>>) -> (Vec<AgentEvent>, Vec<AgentMessage>) {
    let mut events = Vec::new();
    while let Some(event) = stream.next() {
        events.push(event);
    }
    let messages = stream.result();
    (events, messages)
}

#[test]
fn runs_a_simple_turn_with_agent_end() {
    let stream_fn = scripted_stream_fn(vec![assistant(vec![text("hello")], StopReason::Stop)]);
    let config = default_config();
    let context = AgentContext {
        system_prompt: "sys".to_string(),
        messages: vec![],
        tools: None,
    };
    let stream = agent_loop(
        vec![AgentMessage::Llm(Message::User(pi_ai::types::UserMessage {
            content: pi_ai::types::UserMessageContent::Text("hi".to_string()),
            timestamp: 1.0,
        }))],
        context,
        Arc::new(config),
        None,
        stream_fn,
    );
    // Wait for the worker thread to finish.
    let (events, messages) = collect_events(stream);

    // Event sequence: agent_start, turn_start, message_start/end (prompt),
    // turn_start is not repeated for the first turn; message_start/update/end
    // for the assistant message, turn_end, agent_end.
    assert!(matches!(events[0], AgentEvent::AgentStart));
    assert!(matches!(events[1], AgentEvent::TurnStart));
    let event_types: Vec<&str> = events.iter().map(|event| match event {
        AgentEvent::AgentStart => "agent_start",
        AgentEvent::AgentEnd { .. } => "agent_end",
        AgentEvent::TurnStart => "turn_start",
        AgentEvent::TurnEnd { .. } => "turn_end",
        AgentEvent::MessageStart { .. } => "message_start",
        AgentEvent::MessageUpdate { .. } => "message_update",
        AgentEvent::MessageEnd { .. } => "message_end",
        AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
        AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
        AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
    }).collect();
    assert!(event_types.contains(&"message_start"));
    assert!(event_types.contains(&"message_end"));
    assert!(event_types.contains(&"turn_end"));
    assert_eq!(event_types.last(), Some(&"agent_end"));
    assert_eq!(messages.len(), 2, "prompt + assistant message");
}

#[test]
fn executes_tool_calls_and_continues() {
    // First response: a tool call. Second: final text.
    let stream_fn = scripted_stream_fn(vec![
        assistant(vec![tool_call("echo", "call-1")], StopReason::ToolUse),
        assistant(vec![text("done")], StopReason::Stop),
    ]);
    let echo_tool = AgentTool {
        tool: Tool {
            name: "echo".to_string(),
            description: "Echo".to_string(),
            parameters: pi_ai::types::JsonSchemaObject {
                type_: Some(vec!["object".to_string()]),
                ..pi_ai::types::JsonSchemaObject::default()
            },
            constrained_sampling: None,
        },
        label: "Echo".to_string(),
        execute: Some(Arc::new(|_id, _args, _signal, _update| {
            Ok(AgentToolResult {
                content: vec![text("echoed")],
                details: pi_ai::types::JsonValue::Map(Vec::new()),
                usage: None,
                added_tool_names: None,
                terminate: None,
            })
        })),
        execution_mode: None,
    };
    let config = default_config();
    let context = AgentContext {
        system_prompt: "sys".to_string(),
        messages: vec![],
        tools: Some(vec![echo_tool]),
    };
    let stream = agent_loop(
        vec![AgentMessage::Llm(Message::User(pi_ai::types::UserMessage {
            content: pi_ai::types::UserMessageContent::Text("hi".to_string()),
            timestamp: 1.0,
        }))],
        context,
        Arc::new(config),
        None,
        stream_fn,
    );
    let (events, _messages) = collect_events(stream);

    let event_types: Vec<&str> = events.iter().map(|event| match event {
        AgentEvent::AgentStart => "agent_start",
        AgentEvent::AgentEnd { .. } => "agent_end",
        AgentEvent::TurnStart => "turn_start",
        AgentEvent::TurnEnd { .. } => "turn_end",
        AgentEvent::MessageStart { .. } => "message_start",
        AgentEvent::MessageUpdate { .. } => "message_update",
        AgentEvent::MessageEnd { .. } => "message_end",
        AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
        AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
        AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
    }).collect();

    // Tool execution events for the first turn.
    assert!(event_types.contains(&"tool_execution_start"));
    assert!(event_types.contains(&"tool_execution_end"));
    // Second turn runs.
    assert!(event_types.iter().filter(|t| **t == "turn_start").count() >= 2);
    assert_eq!(event_types.last(), Some(&"agent_end"));
}

#[test]
fn errors_terminate_the_loop() {
    let stream_fn = scripted_stream_fn(vec![assistant(
        vec![],
        StopReason::Error,
    )]);
    let config = default_config();
    let context = AgentContext {
        system_prompt: "sys".to_string(),
        messages: vec![],
        tools: None,
    };
    let stream = agent_loop(
        vec![AgentMessage::Llm(Message::User(pi_ai::types::UserMessage {
            content: pi_ai::types::UserMessageContent::Text("hi".to_string()),
            timestamp: 1.0,
        }))],
        context,
        Arc::new(config),
        None,
        stream_fn,
    );
    let (events, _messages) = collect_events(stream);
    let event_types: Vec<&str> = events.iter().map(|event| match event {
        AgentEvent::AgentStart => "agent_start",
        AgentEvent::AgentEnd { .. } => "agent_end",
        AgentEvent::TurnStart => "turn_start",
        AgentEvent::TurnEnd { .. } => "turn_end",
        AgentEvent::MessageStart { .. } => "message_start",
        AgentEvent::MessageUpdate { .. } => "message_update",
        AgentEvent::MessageEnd { .. } => "message_end",
        AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
        AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
        AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
    }).collect();
    // Error stops the loop after the first turn without tool calls.
    assert!(!event_types.contains(&"tool_execution_start"));
    assert_eq!(event_types.last(), Some(&"agent_end"));
}
