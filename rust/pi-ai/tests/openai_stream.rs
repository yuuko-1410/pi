//! End-to-end test of the OpenAI stream processing pipeline: raw SSE wire
//! bytes -> parsed events -> process_responses_stream -> assistant message.

use pi_ai::api::openai_stream::{
    parse_stream_event, process_responses_stream, OpenAIResponsesStreamOptions,
};
use pi_ai::event_stream::AssistantMessageEventStream;
use pi_ai::http::sse::SseParser;
use pi_ai::types::{AssistantMessage, Content, Model, ModelCost, ModelCostRates, StopReason, Usage, UsageCost};

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

fn model() -> Model {
    Model {
        id: "m".to_string(),
        name: "m".to_string(),
        api: "openai-responses".to_string(),
        provider: "openai".to_string(),
        base_url: "https://api.openai.com/v1".to_string(),
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
        context_window: 128_000.0,
        max_tokens: 4096.0,
        sampling_params: None,
        headers: None,
        compat: None,
    }
}

fn process_wire(wire: &str) -> Result<AssistantMessage, String> {
    let mut parser = SseParser::new();
    let events: Vec<_> = parser
        .push(wire.as_bytes())
        .into_iter()
        .filter_map(|sse| parse_stream_event(&sse.data))
        .collect();
    parser.end();

    let mut output = AssistantMessage {
        content: vec![],
        api: "openai-responses".to_string(),
        provider: "openai".to_string(),
        model: "m".to_string(),
        response_model: None,
        response_id: None,
        usage: empty_usage(),
        stop_reason: StopReason::Pending,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1.0,
    };
    let stream = AssistantMessageEventStream::new();
    process_responses_stream(
        events,
        &mut output,
        &stream,
        &model(),
        Some(&OpenAIResponsesStreamOptions::default()),
    )?;
    Ok(output)
}

#[test]
fn processes_a_simple_text_stream() {
    let wire = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\"}}\n\n",
        "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"content\":[{\"type\":\"output_text\",\"text\":\"\",\"annotations\":[]}]}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"Hello\"}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\" world\"}\n\n",
        "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"phase\":\"final_answer\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello world\",\"annotations\":[]}]}}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"total_tokens\":12,\"input_tokens_details\":{\"cached_tokens\":4},\"output_tokens_details\":{\"reasoning_tokens\":0}}}}\n\n",
    );
    let output = process_wire(wire).unwrap();

    assert_eq!(output.response_id.as_deref(), Some("resp_1"));
    assert_eq!(output.stop_reason, StopReason::Stop);
    assert_eq!(
        output.content,
        vec![Content::Text(pi_ai::types::TextContent {
            text: "Hello world".to_string(),
            text_signature: Some(
                "{\"v\":1,\"id\":\"msg_1\",\"phase\":\"final_answer\"}".to_string()
            ),
        })]
    );
    // usage: input subtracts cached tokens.
    assert_eq!(output.usage.input, 6.0);
    assert_eq!(output.usage.cache_read, 4.0);
    assert_eq!(output.usage.output, 2.0);
    assert_eq!(output.usage.total_tokens, 12.0);
}

#[test]
fn processes_a_tool_call_stream() {
    let wire = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_2\"}}\n\n",
        "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"read\",\"arguments\":\"\"}}\n\n",
        "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"path\\\":\\\"/tm\"}\n\n",
        "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"p/file\\\"}\"}\n\n",
        "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"/tmp/file\\\"}\"}}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_2\",\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
    );
    let output = process_wire(wire).unwrap();

    // Tool calls force stopReason toolUse even when the status is completed.
    assert_eq!(output.stop_reason, StopReason::ToolUse);
    assert_eq!(output.content.len(), 1);
    match &output.content[0] {
        Content::ToolCall(tool_call) => {
            assert_eq!(tool_call.id, "call_1|fc_1");
            assert_eq!(tool_call.name, "read");
            assert_eq!(
                tool_call.arguments,
                pi_protocol::Value::Map(vec![("path".to_string(), pi_protocol::Value::String("/tmp/file".to_string()))])
            );
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}

#[test]
fn errors_without_a_terminal_event() {
    let wire = "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_3\"}}\n\n";
    let error = process_wire(wire).unwrap_err();
    assert!(error.contains("ended before a terminal response event"), "{error}");
}

#[test]
fn processes_reasoning_and_thinking_events() {
    let wire = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_4\"}}\n\n",
        "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\"}}\n\n",
        "event: response.reasoning_summary_text.delta\ndata: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":0,\"delta\":\"thinking...\"}\n\n",
        "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"final thought\"}],\"content\":[]}}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_4\",\"status\":\"completed\"}}\n\n",
    );
    let output = process_wire(wire).unwrap();
    assert_eq!(output.content.len(), 1);
    match &output.content[0] {
        Content::Thinking(thinking) => {
            // output_item.done replaces the accumulated delta with the summary.
            assert_eq!(thinking.thinking, "final thought");
            let signature = thinking.thinking_signature.as_deref().unwrap();
            assert!(signature.contains("\"type\":\"reasoning\""), "{signature}");
        }
        other => panic!("expected thinking, got {other:?}"),
    }
}

#[test]
fn parses_events_from_fragmented_sse() {
    let wire = concat!(
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"x\"}\n\n",
    );
    let mut parser = SseParser::new();
    let mut events = Vec::new();
    for byte in wire.as_bytes() {
        events.extend(parser.push(&[*byte]));
    }
    let parsed: Vec<_> = events.iter().filter_map(|e| parse_stream_event(&e.data)).collect();
    assert_eq!(parsed.len(), 1);
    match &parsed[0] {
        pi_ai::api::openai_stream::ResponseStreamEvent::ResponseOutputTextDelta { delta, .. } => {
            assert_eq!(delta, "x");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}
