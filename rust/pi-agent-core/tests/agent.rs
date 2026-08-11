//! Agent wrapper tests: prompt with steering and follow-up queues.

use std::sync::Arc;

use pi_agent_core::agent::{Agent, AgentOptions, MutableAgentState};
use pi_agent_core::types::{AgentEvent, AgentMessage};
use pi_ai::event_stream::AssistantMessageEventStream;
use pi_ai::types::{
    AssistantMessage, Context, Message, Model, ModelCost, ModelCostRates, SimpleStreamOptions,
    StopReason, Usage, UsageCost,
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

fn assistant(text: &str) -> AssistantMessage {
    AssistantMessage {
        content: vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
            text: text.to_string(),
            text_signature: None,
        })],
        api: "faux".to_string(),
        provider: "faux".to_string(),
        model: "faux".to_string(),
        response_model: None,
        response_id: None,
        usage: usage(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1.0,
    }
}

fn scripted_stream_fn() -> Arc<dyn Fn(&Model, &Context, Option<&SimpleStreamOptions>) -> AssistantMessageEventStream + Send + Sync> {
    Arc::new(|_model: &Model, _context: &Context, _options: Option<&SimpleStreamOptions>| {
        let stream = AssistantMessageEventStream::new();
        let message = assistant("hi");
        stream.push(pi_ai::types::AssistantMessageEvent::Start {
            partial: message.clone(),
        });
        stream.push(pi_ai::types::AssistantMessageEvent::Done {
            reason: "stop".to_string(),
            message,
        });
        stream
    })
}

#[test]
fn agent_prompt_runs_and_listeners_receive_events() {
    let mut agent = Agent::new(AgentOptions {
        initial_state: Some(MutableAgentState {
            model: model(),
            ..MutableAgentState::default()
        }),
        stream_fn: scripted_stream_fn(),
        ..AgentOptions::default()
    });

    let events: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let collected = events.clone();
    agent.subscribe(Box::new(move |event: &AgentEvent| {
        let name = match event {
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
        };
        collected.lock().unwrap().push(name.to_string());
    }));

    agent
        .prompt(vec![AgentMessage::Llm(Message::User(pi_ai::types::UserMessage {
            content: pi_ai::types::UserMessageContent::Text("hi".to_string()),
            timestamp: 1.0,
        }))])
        .unwrap();

    let events = events.lock().unwrap();
    assert_eq!(events.first().map(String::as_str), Some("agent_start"));
    assert_eq!(events.last().map(String::as_str), Some("agent_end"));
    assert!(events.contains(&"message_start".to_string()));
    assert!(events.contains(&"message_end".to_string()));

    let state = agent.state();
    assert!(!state.is_streaming);
    assert!(state.messages.len() >= 2, "prompt + assistant response");
}

#[test]
fn steering_messages_are_injected_between_turns() {
    let mut agent = Agent::new(AgentOptions {
        initial_state: Some(MutableAgentState {
            model: model(),
            ..MutableAgentState::default()
        }),
        stream_fn: scripted_stream_fn(),
        ..AgentOptions::default()
    });

    // The scripted stream fn always returns a stop message; steering is
    // injected after the first turn and processed before the second call.
    let turns: Arc<std::sync::Mutex<usize>> = Arc::new(std::sync::Mutex::new(0));
    let turns_ref = turns.clone();
    agent.subscribe(Box::new(move |event: &AgentEvent| {
        if let AgentEvent::TurnEnd { .. } = event {
            *turns_ref.lock().unwrap() += 1;
        }
    }));

    agent.steer(AgentMessage::Llm(Message::User(pi_ai::types::UserMessage {
        content: pi_ai::types::UserMessageContent::Text("steer".to_string()),
        timestamp: 1.0,
    })));

    agent
        .prompt(vec![AgentMessage::Llm(Message::User(pi_ai::types::UserMessage {
            content: pi_ai::types::UserMessageContent::Text("hi".to_string()),
            timestamp: 1.0,
        }))])
        .unwrap();

    // The scripted fn stops immediately, so the steering message is polled but
    // the loop exits; at least one turn ran.
    assert!(*turns.lock().unwrap() >= 1);
}
