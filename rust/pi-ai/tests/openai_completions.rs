//! Tests for the OpenAI Completions provider conversion layer
//! (`packages/ai/src/api/openai-completions.ts` behaviors).

use pi_ai::api::openai_completions::{
    convert_messages, convert_tools, get_compat, map_stop_reason, normalize_tool_call_id_for_completions, ResolvedOpenAICompletionsCompat,
};
use pi_ai::types::{
    AssistantMessage, Content, Context, JsonSchemaObject, Message, Model, ModelCost, ModelCostRates,
    StopReason, TextContent, Tool, ToolCall, ToolResultMessage, Usage, UsageCost, UserMessage,
    UserMessageContent,
};
use pi_protocol::Value;

fn base_model(provider: &str, base_url: &str) -> Model {
    Model {
        id: "m".to_string(),
        name: "m".to_string(),
        api: "openai-completions".to_string(),
        provider: provider.to_string(),
        base_url: base_url.to_string(),
        reasoning: false,
        thinking_level_map: None,
        input: vec!["text".to_string()],
        cost: ModelCost {
            rates: ModelCostRates {
                input: 1.0,
                output: 2.0,
                cache_read: 0.1,
                cache_write: 1.0,
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

fn compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    get_compat(model)
}

#[test]
fn normalizes_tool_call_ids_like_the_js_regex() {
    let model = base_model("openai", "https://api.openai.com/v1");
    // Pipe-separated Responses IDs are split, sanitized, and re-joined.
    assert_eq!(
        normalize_tool_call_id_for_completions("call_1|item_1", &model),
        "call_1_item_1"
    );
    // Long IDs hash to a 40-char prefix + 8-char hash.
    let long_call = "x".repeat(50);
    let long_item = "y".repeat(50);
    let normalized = normalize_tool_call_id_for_completions(&format!("{long_call}|{long_item}"), &model);
    assert_eq!(normalized.chars().count(), 40);
    // Non-pipe IDs for openai are truncated to 40 chars.
    let long = "z".repeat(60);
    assert_eq!(normalize_tool_call_id_for_completions(&long, &model).chars().count(), 40);
    // Non-openai providers keep IDs as-is.
    let other = base_model("anthropic", "https://api.anthropic.com");
    assert_eq!(
        normalize_tool_call_id_for_completions("call-1", &other),
        "call-1"
    );
}

#[test]
fn maps_stop_reasons() {
    assert_eq!(map_stop_reason("stop"), (StopReason::Stop, None));
    assert_eq!(map_stop_reason("end"), (StopReason::Stop, None));
    assert_eq!(map_stop_reason("length"), (StopReason::Length, None));
    assert_eq!(map_stop_reason("tool_calls"), (StopReason::ToolUse, None));
    assert_eq!(map_stop_reason("function_call"), (StopReason::ToolUse, None));
    let (reason, message) = map_stop_reason("content_filter");
    assert_eq!(reason, StopReason::Error);
    assert!(message.unwrap().contains("content_filter"));
    let (reason, message) = map_stop_reason("weird");
    assert_eq!(reason, StopReason::Error);
    assert!(message.unwrap().contains("weird"));
}

#[test]
fn detects_compat_from_provider_and_url() {
    // DeepSeek: deepseek thinking format, max_tokens field, reasoning content.
    let deepseek = base_model("deepseek", "https://api.deepseek.com");
    let c = compat(&deepseek);
    assert_eq!(c.thinking_format, "deepseek");
    assert_eq!(c.max_tokens_field, "max_tokens");
    assert!(c.requires_reasoning_content_on_assistant_messages);

    // OpenAI: standard compat.
    let openai = base_model("openai", "https://api.openai.com/v1");
    let c = compat(&openai);
    assert_eq!(c.thinking_format, "openai");
    assert_eq!(c.max_tokens_field, "max_completion_tokens");
    assert!(c.supports_store);

    // Moonshot: non-standard, max_tokens.
    let moonshot = base_model("moonshotai", "https://api.moonshot.ai");
    let c = compat(&moonshot);
    assert_eq!(c.max_tokens_field, "max_tokens");
    assert!(!c.supports_store);
    assert!(!c.supports_reasoning_effort);
}

#[test]
fn converts_tools_with_strict_and_grammar_modes() {
    let model = base_model("openai", "https://api.openai.com/v1");
    let compat = compat(&model);
    let tool = Tool {
        name: "add".to_string(),
        description: "Adds".to_string(),
        parameters: JsonSchemaObject {
            type_: Some(vec!["object".to_string()]),
            ..JsonSchemaObject::default()
        },
        constrained_sampling: None,
    };
    let converted = convert_tools(&[tool], &compat);
    let entries = converted[0].as_map().unwrap();
    let function = entries
        .iter()
        .find(|(k, _)| k == "function")
        .map(|(_, v)| v)
        .unwrap();
    assert!(matches!(function, Value::Map(_)));
    let function_entries = function.as_map().unwrap();
    assert!(function_entries.iter().any(|(k, v)| k == "strict" && matches!(v, Value::Bool(false))));
    assert!(function_entries
        .iter()
        .any(|(k, _)| k == "parameters"));
}

#[test]
fn converts_messages_with_system_prompt_and_user_text() {
    let model = base_model("openai", "https://api.openai.com/v1");
    let compat = compat(&model);
    let context = Context {
        system_prompt: Some("You are helpful.".to_string()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_string()),
            timestamp: 1.0,
        })],
        tools: None,
    };
    let messages = convert_messages(&model, &context, &compat, None);
    assert_eq!(messages.len(), 2);
    let system = messages[0].as_map().unwrap();
    assert_eq!(get_str(system, "role"), Some("system"));
    let user = messages[1].as_map().unwrap();
    assert_eq!(get_str(user, "role"), Some("user"));
    assert_eq!(get_str(user, "content"), Some("hello"));
}

#[test]
fn converts_messages_with_tool_calls_and_results() {
    let model = base_model("openai", "https://api.openai.com/v1");
    let compat = compat(&model);
    let tool_call = ToolCall {
        id: "call_1".to_string(),
        name: "read".to_string(),
        arguments: Value::Map(vec![("path".to_string(), Value::String("/tmp/f".to_string()))]),
        thought_signature: None,
        namespace: None,
    };
    let assistant = Message::Assistant(AssistantMessage {
        content: vec![Content::ToolCall(tool_call)],
        api: "openai-completions".to_string(),
        provider: "openai".to_string(),
        model: "m".to_string(),
        response_model: None,
        response_id: None,
        usage: Usage {
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
        },
        stop_reason: StopReason::ToolUse,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1.0,
    });
    let tool_result = Message::ToolResult(ToolResultMessage {
        tool_call_id: "call_1".to_string(),
        tool_name: "read".to_string(),
        content: vec![Content::Text(TextContent {
            text: "done".to_string(),
            text_signature: None,
        })],
        details: None,
        usage: None,
        added_tool_names: None,
        is_error: false,
        timestamp: 2.0,
    });
    let context = Context {
        system_prompt: None,
        messages: vec![assistant, tool_result],
        tools: Some(vec![Tool {
            name: "read".to_string(),
            description: "Reads".to_string(),
            parameters: JsonSchemaObject {
                type_: Some(vec!["object".to_string()]),
                ..JsonSchemaObject::default()
            },
            constrained_sampling: None,
        }]),
    };
    let messages = convert_messages(&model, &context, &compat, None);
    assert_eq!(messages.len(), 2);
    let assistant_msg = messages[0].as_map().unwrap();
    assert_eq!(get_str(assistant_msg, "role"), Some("assistant"));
    let tool_calls = assistant_msg
        .iter()
        .find(|(k, _)| k == "tool_calls")
        .map(|(_, v)| v)
        .unwrap();
    let tool_calls = tool_calls.as_array().unwrap();
    assert_eq!(tool_calls.len(), 1);
    let call = tool_calls[0].as_map().unwrap();
    assert_eq!(get_str(call, "type"), Some("function"));
    let tool_msg = messages[1].as_map().unwrap();
    assert_eq!(get_str(tool_msg, "role"), Some("tool"));
    assert_eq!(get_str(tool_msg, "tool_call_id"), Some("call_1"));
}

#[test]
fn converts_thinking_blocks_for_requires_thinking_as_text() {
    let model = base_model("openai", "https://api.openai.com/v1");
    let mut compat = compat(&model);
    compat.requires_thinking_as_text = true;
    let assistant = Message::Assistant(AssistantMessage {
        content: vec![Content::Thinking(pi_ai::types::ThinkingContent {
            thinking: "hmm".to_string(),
            thinking_signature: Some("sig".to_string()),
            redacted: None,
        })],
        api: "openai-completions".to_string(),
        provider: "openai".to_string(),
        model: "m".to_string(),
        response_model: None,
        response_id: None,
        usage: Usage {
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
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1.0,
    });
    let context = Context {
        system_prompt: None,
        messages: vec![assistant],
        tools: None,
    };
    let messages = convert_messages(&model, &context, &compat, None);
    assert_eq!(messages.len(), 1);
    let assistant_msg = messages[0].as_map().unwrap();
    let content = assistant_msg
        .iter()
        .find(|(k, _)| k == "content")
        .map(|(_, v)| v)
        .unwrap();
    assert!(matches!(content, Value::Array(_)), "thinking-as-text produces a content array");
}

#[test]
fn requires_assistant_after_tool_result_bridges_the_gap() {
    let model = base_model("openai", "https://api.openai.com/v1");
    let mut compat = compat(&model);
    compat.requires_assistant_after_tool_result = true;
    let tool_result = Message::ToolResult(ToolResultMessage {
        tool_call_id: "call_1".to_string(),
        tool_name: "read".to_string(),
        content: vec![Content::Text(TextContent {
            text: "done".to_string(),
            text_signature: None,
        })],
        details: None,
        usage: None,
        added_tool_names: None,
        is_error: false,
        timestamp: 2.0,
    });
    let user = Message::User(UserMessage {
        content: UserMessageContent::Text("next".to_string()),
        timestamp: 3.0,
    });
    let context = Context {
        system_prompt: None,
        messages: vec![tool_result, user],
        tools: None,
    };
    let messages = convert_messages(&model, &context, &compat, None);
    // tool + synthetic assistant + user
    assert_eq!(messages.len(), 3);
    assert_eq!(get_str(messages[1].as_map().unwrap(), "role"), Some("assistant"));
    assert_eq!(get_str(messages[2].as_map().unwrap(), "role"), Some("user"));
}

fn get_str<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, value)| value.as_str())
}
