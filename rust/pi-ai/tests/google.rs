//! Integration tests for the Google provider conversion layer
//! (`packages/ai/src/api/google-shared.ts` / `google-generative-ai.ts`
//! behaviors).

use pi_ai::api::google_shared::{
    convert_messages, convert_tools, map_tool_choice, resolve_google_function_calling_mode,
    requires_tool_call_id, GooglePart,
};
use pi_ai::types::{
    AssistantMessage, Content, Context, Message, Model, ModelCost, ModelCostRates, StopReason, TextContent,
    ThinkingContent, Tool, ToolCall, ToolResultMessage, Usage, UsageCost, UserMessage, UserMessageContent,
};
use pi_protocol::Value;

fn model(id: &str, provider: &str, api: &str) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: api.to_string(),
        provider: provider.to_string(),
        base_url: String::new(),
        reasoning: true,
        thinking_level_map: None,
        input: vec!["text".to_string(), "image".to_string()],
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

fn assistant(content: Vec<Content>) -> Message {
    Message::Assistant(AssistantMessage {
        content,
        api: "google-generative-ai".to_string(),
        provider: "google".to_string(),
        model: "gemini-2.5-pro".to_string(),
        response_model: None,
        response_id: None,
        usage: usage(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1.0,
    })
}

#[test]
fn converts_user_and_assistant_messages() {
    let m = model("gemini-2.5-pro", "google", "google-generative-ai");
    let context = Context {
        system_prompt: Some("You are helpful".to_string()),
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("Hello".to_string()),
                timestamp: 1.0,
            }),
            assistant(vec![Content::Text(TextContent {
                text: "Hi there".to_string(),
                text_signature: None,
            })]),
        ],
        tools: None,
    };
    let contents = convert_messages(&m, &context);
    assert_eq!(contents.len(), 2);
    assert_eq!(contents[0].role, "user");
    assert_eq!(contents[1].role, "model");
    match &contents[1].parts[0] {
        GooglePart::Text { text, thought, .. } => {
            assert_eq!(text, "Hi there");
            assert!(!thought);
        }
        other => panic!("unexpected part {other:?}"),
    }
}

#[test]
fn converts_thinking_blocks_to_thought_parts() {
    let m = model("gemini-2.5-pro", "google", "google-generative-ai");
    let context = Context {
        system_prompt: None,
        messages: vec![assistant(vec![Content::Thinking(ThinkingContent {
            thinking: "let me think".to_string(),
            thinking_signature: Some("c2ln".to_string()), // base64("sig"), 4 chars
            redacted: None,
        })])],
        tools: None,
    };
    let contents = convert_messages(&m, &context);
    assert_eq!(contents.len(), 1);
    match &contents[0].parts[0] {
        GooglePart::Text {
            text,
            thought,
            thought_signature,
        } => {
            assert_eq!(text, "let me think");
            assert!(*thought);
            assert_eq!(thought_signature.as_deref(), Some("c2ln"));
        }
        other => panic!("unexpected part {other:?}"),
    }
}

#[test]
fn cross_model_thinking_becomes_plain_text() {
    let m = model("gemini-2.5-pro", "google", "google-generative-ai");
    let context = Context {
        system_prompt: None,
        messages: vec![assistant(vec![Content::Thinking(ThinkingContent {
            thinking: "hmm".to_string(),
            thinking_signature: Some("c2ln".to_string()),
            redacted: None,
        })])],
        tools: None,
    };
    // Assistant message is from provider "google" / model "gemini-2.5-pro",
    // matching the model — so this stays a thought part. Build a foreign one.
    let foreign = Message::Assistant(AssistantMessage {
        content: vec![Content::Thinking(ThinkingContent {
            thinking: "hmm".to_string(),
            thinking_signature: Some("c2ln".to_string()),
            redacted: None,
        })],
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        model: "claude-sonnet".to_string(),
        response_model: None,
        response_id: None,
        usage: usage(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1.0,
    });
    let context = Context {
        system_prompt: None,
        messages: vec![foreign],
        tools: None,
    };
    let contents = convert_messages(&m, &context);
    match &contents[0].parts[0] {
        GooglePart::Text {
            text,
            thought,
            thought_signature,
        } => {
            assert_eq!(text, "hmm");
            assert!(!thought, "cross-model thinking must become plain text");
            assert_eq!(thought_signature, &None, "foreign signature must be dropped");
        }
        other => panic!("unexpected part {other:?}"),
    }
}

#[test]
fn converts_tool_calls_with_optional_ids() {
    let m = model("gemini-2.5-pro", "google", "google-generative-ai");
    let tool_call = ToolCall {
        id: "call_1".to_string(),
        name: "read".to_string(),
        arguments: Value::Map(vec![("path".to_string(), Value::String("/tmp".to_string()))]),
        thought_signature: None,
        namespace: None,
    };
    let context = Context {
        system_prompt: None,
        messages: vec![assistant(vec![Content::ToolCall(tool_call)])],
        tools: None,
    };
    let contents = convert_messages(&m, &context);
    match &contents[0].parts[0] {
        GooglePart::FunctionCall { name, args, id, .. } => {
            assert_eq!(name, "read");
            assert!(matches!(args, Value::Map(_)));
            // Gemini < 3: no explicit tool call id.
            assert_eq!(id, &None);
        }
        other => panic!("unexpected part {other:?}"),
    }

    // Gemini 3 requires explicit ids.
    let m3 = model("gemini-3-pro", "google", "google-generative-ai");
    let context = Context {
        system_prompt: None,
        messages: vec![assistant(vec![Content::ToolCall(ToolCall {
            id: "call_1".to_string(),
            name: "read".to_string(),
            arguments: Value::Map(Vec::new()),
            thought_signature: None,
            namespace: None,
        })])],
        tools: None,
    };
    let contents = convert_messages(&m3, &context);
    match &contents[0].parts[0] {
        GooglePart::FunctionCall { id, .. } => {
            assert_eq!(id.as_deref(), Some("call_1"));
        }
        other => panic!("unexpected part {other:?}"),
    }
}

#[test]
fn converts_tool_results_with_output_key() {
    let m = model("gemini-2.5-pro", "google", "google-generative-ai");
    let tool_result = Message::ToolResult(ToolResultMessage {
        tool_call_id: "call_1".to_string(),
        tool_name: "read".to_string(),
        content: vec![Content::Text(TextContent {
            text: "file contents".to_string(),
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
        messages: vec![tool_result],
        tools: None,
    };
    let contents = convert_messages(&m, &context);
    assert_eq!(contents.len(), 1);
    match &contents[0].parts[0] {
        GooglePart::FunctionResponse { name, response, id, .. } => {
            assert_eq!(name, "read");
            assert_eq!(id, &None);
            match response {
                Value::Map(entries) => {
                    assert!(entries.iter().any(|(k, v)| k == "output" && v.as_str() == Some("file contents")));
                }
                other => panic!("unexpected response {other:?}"),
            }
        }
        other => panic!("unexpected part {other:?}"),
    }
}

#[test]
fn converts_tools_with_parameter_json_schema() {
    let tool = Tool {
        name: "read".to_string(),
        description: "Reads a file".to_string(),
        parameters: pi_ai::types::JsonSchemaObject {
            type_: Some(vec!["object".to_string()]),
            properties: Some(vec![(
                "path".to_string(),
                pi_ai::types::JsonSchemaObject {
                    type_: Some(vec!["string".to_string()]),
                    ..pi_ai::types::JsonSchemaObject::default()
                },
            )]),
            required: Some(vec!["path".to_string()]),
            ..pi_ai::types::JsonSchemaObject::default()
        },
        constrained_sampling: None,
    };
    let converted = convert_tools(&[tool], false).unwrap();
    let Value::Array(tool_sets) = &converted else { panic!() };
    let Value::Map(tool_set) = &tool_sets[0] else { panic!() };
    let declarations = tool_set
        .iter()
        .find(|(k, _)| k == "functionDeclarations")
        .unwrap()
        .1
        .clone();
    let Value::Array(declarations) = declarations else { panic!() };
    let Value::Map(declaration) = &declarations[0] else { panic!() };
    assert!(declaration.iter().any(|(k, _)| k == "name"));
    assert!(declaration.iter().any(|(k, _)| k == "parametersJsonSchema"));
    assert!(!declaration.iter().any(|(k, _)| k == "parameters"));
}

#[test]
fn resolves_function_calling_modes() {
    assert_eq!(map_tool_choice("auto"), "AUTO");
    assert_eq!(map_tool_choice("none"), "NONE");
    assert_eq!(map_tool_choice("any"), "ANY");
    // No strict tools, no explicit choice: undefined.
    assert_eq!(resolve_google_function_calling_mode(&[], None, false), None);
    assert_eq!(
        resolve_google_function_calling_mode(&[], Some("auto"), false).as_deref(),
        Some("AUTO")
    );
    assert_eq!(
        resolve_google_function_calling_mode(&[], Some("any"), false).as_deref(),
        Some("ANY")
    );
}

#[test]
fn detects_tool_call_id_requirements() {
    assert!(requires_tool_call_id("gemini-3-pro"));
    assert!(requires_tool_call_id("claude-sonnet-4"));
    assert!(requires_tool_call_id("gpt-oss-20b"));
    assert!(!requires_tool_call_id("gemini-2.5-pro"));
    assert!(!requires_tool_call_id("gemini-flash-latest"));
}
