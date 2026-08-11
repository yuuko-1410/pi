//! Unit tests for the OpenAI Codex Responses provider port
//! (`packages/ai/src/api/openai-codex-responses.ts` behaviors).

use pi_ai::api::openai_codex_responses::{
    extract_account_id, is_retryable_error, is_terminal_rate_limit_error, parse_error_response,
    resolve_codex_url,
};
use pi_ai::types::{Context, Model, ModelCost, ModelCostRates};
use pi_protocol::Value;

fn model() -> Model {
    Model {
        id: "gpt-5-codex".to_string(),
        name: "gpt-5-codex".to_string(),
        api: "openai-codex-responses".to_string(),
        provider: "openai-codex".to_string(),
        base_url: "https://chatgpt.com/backend-api".to_string(),
        reasoning: true,
        thinking_level_map: Some(vec![
            ("off".to_string(), Some("none".to_string())),
            ("high".to_string(), Some("high".to_string())),
        ]),
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
        context_window: 200_000.0,
        max_tokens: 8192.0,
        sampling_params: None,
        headers: None,
        compat: None,
    }
}

#[test]
fn resolves_codex_urls() {
    assert_eq!(
        resolve_codex_url(Some("https://chatgpt.com/backend-api")),
        "https://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(
        resolve_codex_url(Some("https://x.example/codex")),
        "https://x.example/codex/responses"
    );
    assert_eq!(
        resolve_codex_url(Some("https://x.example/codex/responses/")),
        "https://x.example/codex/responses"
    );
    assert_eq!(
        resolve_codex_url(None),
        "https://chatgpt.com/backend-api/codex/responses"
    );
}

#[test]
fn extracts_account_id_from_jwt() {
    // Header: {"alg":"HS256"}; payload: {"https://api.openai.com/auth":{"chatgpt_account_id":"acct_123"}}
    let header = base64url("{\"alg\":\"HS256\"}");
    let payload = base64url(
        "{\"https://api.openai.com/auth\":{\"chatgpt_account_id\":\"acct_123\"}}",
    );
    let token = format!("{header}.{payload}.sig");
    assert_eq!(extract_account_id(&token).unwrap(), "acct_123");

    assert!(extract_account_id("not-a-token").is_err());
    let bad_payload = base64url("{\"other\": 1}");
    assert!(extract_account_id(&format!("{header}.{bad_payload}.sig")).is_err());
}

fn base64url(input: &str) -> String {
    let encoded = base64::encode_standard(input.as_bytes());
    encoded.replace('+', "-").replace('/', "_").trim_end_matches('=').to_string()
}

mod base64 {
    pub fn encode_standard(input: &[u8]) -> String {
        const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut result = String::with_capacity(input.len().div_ceil(3) * 4);
        for chunk in input.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            result.push(TABLE[(n >> 18) as usize & 63] as char);
            result.push(TABLE[(n >> 12) as usize & 63] as char);
            if chunk.len() > 1 {
                result.push(TABLE[(n >> 6) as usize & 63] as char);
            } else {
                result.push('=');
            }
            if chunk.len() > 2 {
                result.push(TABLE[n as usize & 63] as char);
            } else {
                result.push('=');
            }
        }
        result
    }
}

#[test]
fn classifies_retryable_errors() {
    assert!(is_retryable_error(Some(429), "rate limit"));
    assert!(is_retryable_error(Some(500), "server error"));
    assert!(is_retryable_error(Some(503), ""));
    assert!(!is_retryable_error(Some(400), "bad request"));
    assert!(!is_retryable_error(Some(429), "GoUsageLimitError"));
    assert!(is_retryable_error(None, "connection refused"));
    assert!(is_terminal_rate_limit_error("insufficient_quota"));
    assert!(!is_terminal_rate_limit_error("rate limit"));
}

#[test]
fn parses_error_responses() {
    // Usage-limit friendly message.
    let body = r#"{"error":{"code":"usage_limit_reached","message":"limit","plan_type":"plus","resets_at":0}}"#;
    let message = parse_error_response(body, Some(429));
    assert!(message.contains("usage limit"), "{message}");

    // Plain error message.
    let body = r#"{"error":{"message":"boom"}}"#;
    assert_eq!(parse_error_response(body, Some(400)), "boom");

    // Raw text fallback.
    assert_eq!(parse_error_response("plain text", Some(500)), "plain text");
}

#[test]
fn builds_request_body_with_codex_fields() {
    let context = Context {
        system_prompt: Some("Be concise.".to_string()),
        ..Context::default()
    };
    let options = pi_ai::api::openai_codex_responses::OpenAICodexResponsesOptions {
        text_verbosity: Some("high".to_string()),
        reasoning_effort: Some("high".to_string()),
        tool_choice: Some("required".to_string()),
        ..Default::default()
    };
    let body = pi_ai::api::openai_codex_responses::build_request_body_for_test(
        &model(),
        &context,
        Some(&options),
        Some("session-1"),
        &[],
    );
    let Value::Map(entries) = &body else {
        panic!("expected map");
    };
    let get = |key: &str| entries.iter().find(|(k, _)| k == key).map(|(_, v)| v);

    assert_eq!(get("model"), Some(&Value::String("gpt-5-codex".to_string())));
    assert_eq!(get("store"), Some(&Value::Bool(false)));
    assert_eq!(get("stream"), Some(&Value::Bool(true)));
    assert_eq!(
        get("instructions"),
        Some(&Value::String("Be concise.".to_string()))
    );
    assert_eq!(get("prompt_cache_key"), Some(&Value::String("session-1".to_string())));
    assert_eq!(get("tool_choice"), Some(&Value::String("required".to_string())));
    assert_eq!(get("parallel_tool_calls"), Some(&Value::Bool(true)));
    assert_eq!(
        get("text"),
        Some(&Value::Map(vec![("verbosity".to_string(), Value::String("high".to_string()))]))
    );
    // reasoning: effort resolved through thinkingLevelMap.
    if let Some(Value::Map(reasoning)) = get("reasoning") {
        assert_eq!(
            reasoning.iter().find(|(k, _)| k == "effort").map(|(_, v)| v),
            Some(&Value::String("high".to_string()))
        );
    } else {
        panic!("expected reasoning block");
    }
}
