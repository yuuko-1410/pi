//! Tests for the OpenAI Responses conversion layer
//! (`packages/ai/src/api/openai-responses-shared.ts` behaviors).

use pi_ai::api::openai_responses_shared::{
    convert_responses_tools, encode_text_signature_v1, normalize_id_part, parse_text_signature,
    ConvertResponsesToolsOptions,
};
use pi_ai::types::{ConstrainedSampling, ConstrainedSamplingConfig, JsonSchemaObject, Tool};
use pi_protocol::Value;

#[test]
fn normalizes_ids_like_the_js_regex() {
    // [^a-zA-Z0-9_-] -> _, truncate to 64, strip trailing _.
    assert_eq!(normalize_id_part("call_1"), "call_1");
    assert_eq!(normalize_id_part("weird|id!"), "weird_id");
    assert_eq!(normalize_id_part("a|b"), "a_b");
    let long = "x".repeat(100);
    assert_eq!(normalize_id_part(&long).chars().count(), 64);
    assert_eq!(normalize_id_part("id___"), "id");
}

#[test]
fn round_trips_text_signatures() {
    let encoded = encode_text_signature_v1("msg_1", Some("commentary"));
    assert_eq!(encoded, "{\"v\":1,\"id\":\"msg_1\",\"phase\":\"commentary\"}");
    let parsed = parse_text_signature(Some(&encoded));
    assert_eq!(parsed, Some(("msg_1".to_string(), Some("commentary".to_string()))));

    // Legacy plain-string signatures.
    let parsed = parse_text_signature(Some("legacy-id"));
    assert_eq!(parsed, Some(("legacy-id".to_string(), None)));
    assert_eq!(parse_text_signature(None), None);

    // Invalid JSON object falls through to plain-string handling.
    let parsed = parse_text_signature(Some("{not json"));
    assert_eq!(parsed, Some(("{not json".to_string(), None)));
}

#[test]
fn converts_tools_with_strict_and_grammar_modes() {
    let tool = Tool {
        name: "add".to_string(),
        description: "Adds".to_string(),
        parameters: JsonSchemaObject {
            type_: Some(vec!["object".to_string()]),
            ..JsonSchemaObject::default()
        },
        constrained_sampling: None,
    };
    let options = ConvertResponsesToolsOptions {
        strict: Some(true),
        supports_strict_mode: Some(true),
        supports_openai_grammar_tools: None,
        defer_loading: None,
    };
    let converted = convert_responses_tools(&[tool], Some(&options));
    let pi_ai::api::openai_responses_shared::OpenAITool::Function {
        name, strict, parameters, ..
    } = &converted[0]
    else {
        panic!("expected function tool");
    };
    assert_eq!(name, "add");
    assert_eq!(strict, &Some(true));
    assert!(matches!(parameters, Value::Map(_)));

    // Grammar-constrained tool converts to a custom tool.
    let grammar_tool = Tool {
        name: "search".to_string(),
        description: "Search".to_string(),
        parameters: JsonSchemaObject {
            type_: Some(vec!["object".to_string()]),
            required: Some(vec!["query".to_string()]),
            properties: Some(vec![(
                "query".to_string(),
                JsonSchemaObject {
                    type_: Some(vec!["string".to_string()]),
                    ..JsonSchemaObject::default()
                },
            )]),
            ..JsonSchemaObject::default()
        },
        constrained_sampling: Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::Grammar {
            variants: vec![("openai_lark".to_string(), "start: \"x\"".to_string())],
        })),
    };
    let options = ConvertResponsesToolsOptions {
        strict: None,
        supports_strict_mode: Some(true),
        supports_openai_grammar_tools: Some(true),
        defer_loading: Some(true),
    };
    let converted = convert_responses_tools(&[grammar_tool], Some(&options));
    let pi_ai::api::openai_responses_shared::OpenAITool::Custom {
        name,
        format,
        definition,
        defer_loading,
        ..
    } = &converted[0]
    else {
        panic!("expected custom tool");
    };
    assert_eq!(name, "search");
    assert_eq!(format, "lark");
    assert_eq!(definition, "start: \"x\"");
    assert_eq!(defer_loading, &Some(true));
}
