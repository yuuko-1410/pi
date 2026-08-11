//! OpenRouter image generation provider, port of
//! `packages/ai/src/api/openrouter-images.ts`.
//!
//! POSTs a chat-completions request with `modalities: ["image"]` (plus text
//! when supported) to `<baseUrl>/chat/completions` and parses the JSON
//! response. Mirroring the JS implementation, `generate_images` never
//! rejects: errors are encoded in the returned `AssistantImages`
//! (`stop_reason` = error/aborted + `error_message`). The `Result` wrapper
//! is kept per the port contract but always yields `Ok` (the `Err` branch is
//! unreachable and exists for API-shape parity).

use pi_protocol::Value;

use crate::http::client::HttpClient;
use crate::types::{
    AssistantImages, Content, ImageContent, ImagesContext, ImagesModel, ImagesStopReason, ProviderRequestOptions,
    TextContent, Usage, UsageCost,
};

/// Image-generation request options. The JS `ImagesOptions` (ProviderRequestOptions
/// plus metadata) is not yet in types.rs; defined here until it moves there.
#[derive(Clone, Debug, Default)]
pub struct ImagesOptions {
    pub request: ProviderRequestOptions,
    pub metadata: Option<Vec<(String, Value)>>,
}
use crate::utils::error_body::{format_provider_error, NormalizedProviderError};
use crate::utils::headers::provider_headers_to_record;
use crate::utils::provider_retry::{retry_provider_request, ProviderError, ProviderRetryOptions};
use crate::utils::sanitize::sanitize_surrogates;

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as f64
}

fn empty_output(model: &ImagesModel) -> AssistantImages {
    AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.model.id.clone(),
        output: vec![],
        response_id: None,
        usage: None,
        stop_reason: ImagesStopReason::Stop,
        error_message: None,
        timestamp: now_ms(),
    }
}

/// Mirrors `buildParams`: chat-completions request with image/text modalities.
fn build_params(model: &ImagesModel, context: &ImagesContext) -> Value {
    let content: Vec<Value> = context
        .input
        .iter()
        .map(|item| match item {
            Content::Text(text) => Value::Map(vec![
                ("type".to_string(), Value::String("text".to_string())),
                ("text".to_string(), Value::String(sanitize_surrogates(&text.text))),
            ]),
            Content::Image(image) => Value::Map(vec![
                ("type".to_string(), Value::String("image_url".to_string())),
                (
                    "image_url".to_string(),
                    Value::Map(vec![(
                        "url".to_string(),
                        Value::String(format!("data:{};base64,{}", image.mime_type, image.data)),
                    )]),
                ),
            ]),
            _ => Value::Map(vec![]),
        })
        .collect();

    let modalities: Vec<Value> = if model.output.iter().any(|kind| kind == "text") {
        vec![Value::String("image".to_string()), Value::String("text".to_string())]
    } else {
        vec![Value::String("image".to_string())]
    };

    Value::Map(vec![
        ("model".to_string(), Value::String(model.model.id.clone())),
        (
            "messages".to_string(),
            Value::Array(vec![Value::Map(vec![
                ("role".to_string(), Value::String("user".to_string())),
                ("content".to_string(), Value::Array(content)),
            ])]),
        ),
        ("stream".to_string(), Value::Bool(false)),
        ("modalities".to_string(), Value::Array(modalities)),
    ])
}

/// Mirrors `parseUsage` for OpenAI chat-completion usage.
fn parse_usage(raw: &Value, model: &ImagesModel) -> Usage {
    let prompt_tokens = raw
        .as_map()
        .and_then(|entries| get_num(entries, "prompt_tokens"))
        .unwrap_or(0.0);
    let completion_tokens = raw
        .as_map()
        .and_then(|entries| get_num(entries, "completion_tokens"))
        .unwrap_or(0.0);
    let details = raw
        .as_map()
        .and_then(|entries| get_obj(entries, "prompt_tokens_details"));
    let reported_cached_tokens = details
        .and_then(|d| get_num(d, "cached_tokens"))
        .unwrap_or(0.0);
    let cache_write_tokens = details
        .and_then(|d| get_num(d, "cache_write_tokens"))
        .unwrap_or(0.0);
    let cache_read_tokens = if cache_write_tokens > 0.0 {
        (reported_cached_tokens - cache_write_tokens).max(0.0)
    } else {
        reported_cached_tokens
    };
    let input = (prompt_tokens - cache_read_tokens - cache_write_tokens).max(0.0);
    let output = completion_tokens;

    let rates = &model.model.cost.rates;
    let cost = UsageCost {
        input: (rates.input / 1_000_000.0) * input,
        output: (rates.output / 1_000_000.0) * output,
        cache_read: (rates.cache_read / 1_000_000.0) * cache_read_tokens,
        cache_write: (rates.cache_write / 1_000_000.0) * cache_write_tokens,
        total: 0.0,
    };
    let total = cost.input + cost.output + cost.cache_read + cost.cache_write;
    Usage {
        input,
        output,
        cache_read: cache_read_tokens,
        cache_write: cache_write_tokens,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: input + output + cache_read_tokens + cache_write_tokens,
        cost: UsageCost { total, ..cost },
    }
}

fn get_str(entries: &[(String, Value)], key: &str) -> Option<String> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_str())
        .map(|s| s.to_string())
}

fn get_num(entries: &[(String, Value)], key: &str) -> Option<f64> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_number())
}

fn get_obj<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a [(String, Value)]> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_map())
}

fn get_arr<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a [Value]> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_array())
}

/// Parses `data:<mime>;base64,<data>` image URLs into image content blocks.
fn parse_image_data_url(url: &str) -> Option<(String, String)> {
    if !url.starts_with("data:") {
        return None;
    }
    let rest = url.strip_prefix("data:")?;
    let (mime_type, payload) = rest.split_once(';')?;
    let payload = payload.strip_prefix("base64,")?;
    Some((mime_type.to_string(), payload.to_string()))
}

fn format_images_error(error: &ProviderError) -> String {
    let normalized = NormalizedProviderError::new(error.message.clone(), error.status, None);
    format_provider_error(&normalized, None)
}

/// Generates images via OpenRouter's chat-completions-compatible API.
pub fn generate_images(
    model: &ImagesModel,
    context: &ImagesContext,
    options: Option<&ImagesOptions>,
    api_key: Option<&str>,
    client: &HttpClient,
) -> Result<AssistantImages, String> {
    let mut output = empty_output(model);

    let api_key = match api_key {
        Some(key) if !key.is_empty() => key.to_string(),
        _ => {
            output.stop_reason = ImagesStopReason::Error;
            output.error_message = Some(format!("No API key for provider: {}", model.provider));
            return Ok(output);
        }
    };

    let params = build_params(model, context);
    let mut headers: Vec<(String, String)> = vec![("Authorization".to_string(), format!("Bearer {api_key}"))];
    if let Some(options_headers) = options.and_then(|o| o.request.headers.as_ref()) {
        if let Some(record) = provider_headers_to_record(Some(options_headers)) {
            for (key, value) in record {
                if let Some(existing) = headers.iter_mut().find(|(k, _)| k == &key) {
                    existing.1 = value;
                } else {
                    headers.push((key, value));
                }
            }
        }
    }
    // Model headers (JS merges `{ ...model.headers, ...optionsHeaders }`).
    if let Some(model_headers) = &model.model.headers {
        for (key, value) in model_headers {
            if let Some(existing) = headers.iter_mut().find(|(k, _)| k == key) {
                existing.1 = value.clone();
            } else {
                headers.push((key.clone(), value.clone()));
            }
        }
    }
    let url = format!("{}/chat/completions", model.model.base_url.trim_end_matches('/'));

    let response = match retry_provider_request(
        || {
            client
                .post_json(
                    &url,
                    &headers,
                    &params,
                    options.and_then(|o| o.request.timeout_ms),
                )
                .map(|response| response)
                .map_err(|error| ProviderError::new(error.status, error.headers.clone(), error.message.clone()))
        },
        ProviderRetryOptions {
            max_retries: options.and_then(|o| o.request.max_retries),
            max_retry_delay_ms: options.and_then(|o| o.request.max_retry_delay_ms).map(|v| v as f64),
            token: None,
        },
    ) {
        Ok(response) => response,
        Err(failure) => {
            output.stop_reason = ImagesStopReason::Error;
            output.error_message = Some(match failure {
                crate::utils::provider_retry::ProviderRetryFailure::Error(error) => format_images_error(&error),
                crate::utils::provider_retry::ProviderRetryFailure::Aborted => "Request was aborted".to_string(),
            });
            return Ok(output);
        }
    };

    // Read the full JSON body.
    let body: String = {
        use std::io::Read;
        let mut reader = response.reader;
        let mut buffer = Vec::new();
        reader.read_to_end(&mut buffer).ok();
        String::from_utf8_lossy(&buffer).to_string()
    };
    let parsed: Value = match crate::utils::json::parse_json_with_repair(&body) {
        Ok(value) => value,
        Err(_) => {
            output.stop_reason = ImagesStopReason::Error;
            output.error_message = Some("Failed to parse image generation response".to_string());
            return Ok(output);
        }
    };

    let Some(response_entries) = parsed.as_map() else {
        output.stop_reason = ImagesStopReason::Error;
        output.error_message = Some("Malformed image generation response".to_string());
        return Ok(output);
    };

    output.response_id = get_str(response_entries, "id");
    if let Some(usage) = get_obj(response_entries, "usage") {
        output.usage = Some(parse_usage(&Value::Map(usage.to_vec()), model));
    }

    if let Some(choices) = get_arr(response_entries, "choices") {
        if let Some(choice) = choices.first() {
            if let Some(choice_entries) = choice.as_map() {
                if let Some(message) = get_obj(choice_entries, "message") {
                    // Text content.
                    if let Some(text) = get_str(message, "content") {
                        if !text.is_empty() {
                            output.output.push(Content::Text(TextContent {
                                text,
                                text_signature: None,
                            }));
                        }
                    }
                    // Generated images.
                    if let Some(images) = get_arr(message, "images") {
                        for image in images {
                            let image_url = match image.as_map().and_then(|e| get_str(e, "image_url")) {
                                Some(url) => url,
                                None => match image.as_map().and_then(|e| get_obj(e, "image_url")) {
                                    Some(url_entries) => get_str(url_entries, "url").unwrap_or_default(),
                                    None => String::new(),
                                },
                            };
                            if let Some((mime_type, data)) = parse_image_data_url(&image_url) {
                                output.output.push(Content::Image(ImageContent { mime_type, data }));
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn images_model() -> ImagesModel {
        ImagesModel {
            model: crate::types::Model {
                id: "img".to_string(),
                name: "img".to_string(),
                api: "openrouter-images".to_string(),
                provider: "openrouter".to_string(),
                base_url: "https://openrouter.ai/api/v1".to_string(),
                reasoning: false,
                thinking_level_map: None,
                input: vec!["text".to_string(), "image".to_string()],
                cost: crate::types::ModelCost {
                    rates: crate::types::ModelCostRates {
                        input: 1.0,
                        output: 1.0,
                        cache_read: 0.1,
                        cache_write: 1.0,
                    },
                    tiers: None,
                },
                context_window: 0.0,
                max_tokens: 0.0,
                sampling_params: None,
                headers: None,
                compat: None,
            },
            api: "openrouter-images".to_string(),
            provider: "openrouter".to_string(),
            output: vec!["image".to_string(), "text".to_string()],
        }
    }

    #[test]
    fn builds_params_with_text_and_image_modalities() {
        let model = images_model();
        let context = ImagesContext {
            input: vec![Content::Text(TextContent {
                text: "a cat".to_string(),
                text_signature: None,
            })],
        };
        let params = build_params(&model, &context);
        let entries = params.as_map().unwrap();
        assert_eq!(get_str(entries, "model").as_deref(), Some("img"));
        assert_eq!(
            entries.iter().find(|(k, _)| k == "stream").map(|(_, v)| v),
            Some(&Value::Bool(false))
        );
        let modalities = get_arr(entries, "modalities").unwrap();
        assert_eq!(modalities.len(), 2);
    }

    #[test]
    fn parses_usage_with_cache_split() {
        let raw = Value::Map(vec![
            ("prompt_tokens".to_string(), Value::Number(100.0)),
            ("completion_tokens".to_string(), Value::Number(10.0)),
            (
                "prompt_tokens_details".to_string(),
                Value::Map(vec![
                    ("cached_tokens".to_string(), Value::Number(30.0)),
                    ("cache_write_tokens".to_string(), Value::Number(10.0)),
                ]),
            ),
        ]);
        let usage = parse_usage(&raw, &images_model());
        // cacheRead = max(0, 30 - 10) = 20; input = 100 - 20 - 10 = 70.
        assert_eq!(usage.input, 70.0);
        assert_eq!(usage.cache_read, 20.0);
        assert_eq!(usage.cache_write, 10.0);
        assert_eq!(usage.output, 10.0);
        assert_eq!(usage.total_tokens, 110.0);
        assert_eq!(usage.cost.input, 0.00007);
    }

    #[test]
    fn parses_image_data_urls() {
        let url = "data:image/png;base64,AAAA";
        assert_eq!(
            parse_image_data_url(url),
            Some(("image/png".to_string(), "AAAA".to_string()))
        );
        assert_eq!(parse_image_data_url("https://x/y.png"), None);
        assert_eq!(parse_image_data_url("data:image/png;base64,"), Some(("image/png".to_string(), String::new())));
    }

    #[test]
    fn missing_api_key_returns_error_output() {
        let output = generate_images(
            &images_model(),
            &ImagesContext { input: vec![] },
            None,
            None,
            &HttpClient::new(),
        )
        .unwrap();
        assert_eq!(output.stop_reason, ImagesStopReason::Error);
        assert!(output.error_message.as_deref().unwrap().contains("No API key"));
    }
}
