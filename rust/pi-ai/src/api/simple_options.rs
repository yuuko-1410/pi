//! Simple stream option builders, port of `packages/ai/src/api/simple-options.ts`.

use crate::types::{Context, Model, SimpleStreamOptions, ThinkingBudgets, ThinkingLevel};
use crate::utils::estimate::{estimate_context_tokens, ContextOrMessages};

pub const CONTEXT_SAFETY_TOKENS: f64 = 4096.0;
pub const MIN_MAX_TOKENS: f64 = 1.0;
/// Tokens always left for the answer when a thinking budget shares the
/// response ceiling.
pub const MIN_ANSWER_TOKENS: f64 = 1024.0;

pub fn clamp_max_tokens_to_context(model: &Model, context: &Context, max_tokens: f64) -> f64 {
    if model.context_window <= 0.0 {
        return max_tokens.max(MIN_MAX_TOKENS);
    }
    let estimate = estimate_context_tokens(ContextOrMessages::Context(context));
    let available = model.context_window - estimate.tokens - CONTEXT_SAFETY_TOKENS;
    max_tokens.min(available.max(MIN_MAX_TOKENS))
}

/// Builds the base stream options for a request. The Rust `SimpleStreamOptions`
/// already holds the shared request fields; this computes the derived ones
/// (merged sampling params and clamped max tokens).
pub fn build_base_options(
    model: &Model,
    context: &Context,
    options: Option<&SimpleStreamOptions>,
    _api_key: Option<&str>,
) -> crate::types::StreamOptions {
    let sampling_params = match (&model.sampling_params, options.and_then(|o| o.stream.sampling_params.as_ref())) {
        (None, None) => None,
        (model_params, request_params) => {
            let mut merged = model_params.clone().unwrap_or_default();
            if let Some(request_params) = request_params {
                for (key, value) in request_params {
                    if let Some(existing) = merged.iter_mut().find(|(k, _)| k == key) {
                        existing.1 = value.clone();
                    } else {
                        merged.push((key.clone(), value.clone()));
                    }
                }
            }
            Some(merged)
        }
    };
    let max_tokens = match options.and_then(|o| o.stream.max_tokens) {
        Some(value) => value,
        None => model.max_tokens,
    };
    crate::types::StreamOptions {
        request: options
            .map(|o| o.stream.request.clone())
            .unwrap_or_default(),
        temperature: options.and_then(|o| o.stream.temperature),
        sampling_params,
        max_tokens: Some(clamp_max_tokens_to_context(model, context, max_tokens)),
        transport: options.and_then(|o| o.stream.transport.clone()),
        cache_retention: options.and_then(|o| o.stream.cache_retention.clone()),
        session_id: options.and_then(|o| o.stream.session_id.clone()),
        websocket_connect_timeout_ms: options.and_then(|o| o.stream.websocket_connect_timeout_ms),
        metadata: options.and_then(|o| o.stream.metadata.clone()),
    }
}

/// Clamps xhigh/max reasoning to high, mirroring `clampReasoning`.
pub fn clamp_reasoning(effort: Option<&ThinkingLevel>) -> Option<ThinkingLevel> {
    match effort {
        Some(effort) if effort == "xhigh" || effort == "max" => Some("high".to_string()),
        other => other.cloned(),
    }
}

const DEFAULT_THINKING_BUDGETS: ThinkingBudgets = ThinkingBudgets {
    minimal: Some(1024.0),
    low: Some(2048.0),
    medium: Some(8192.0),
    high: Some(16384.0),
};

/// Mirrors `adjustMaxTokensForThinking`.
pub fn adjust_max_tokens_for_thinking(
    base_max_tokens: Option<f64>,
    model_max_tokens: f64,
    reasoning_level: &ThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> (f64, f64) {
    let mut budgets = DEFAULT_THINKING_BUDGETS.clone();
    if let Some(custom) = custom_budgets {
        if let Some(minimal) = custom.minimal {
            budgets.minimal = Some(minimal);
        }
        if let Some(low) = custom.low {
            budgets.low = Some(low);
        }
        if let Some(medium) = custom.medium {
            budgets.medium = Some(medium);
        }
        if let Some(high) = custom.high {
            budgets.high = Some(high);
        }
    }

    let level = clamp_reasoning(Some(reasoning_level)).expect("clamped to non-xhigh/max");
    let thinking_budget = match level.as_str() {
        "minimal" => budgets.minimal,
        "low" => budgets.low,
        "medium" => budgets.medium,
        _ => budgets.high,
    }
    .expect("default budgets cover all levels");
    let max_tokens = match base_max_tokens {
        None => model_max_tokens,
        Some(base) => (base + thinking_budget).min(model_max_tokens),
    };

    let thinking_budget = if max_tokens <= thinking_budget {
        (max_tokens - MIN_ANSWER_TOKENS).max(0.0)
    } else {
        thinking_budget
    };

    (max_tokens, thinking_budget)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_reasoning_levels() {
        assert_eq!(clamp_reasoning(Some(&"high".to_string())), Some("high".to_string()));
        assert_eq!(clamp_reasoning(Some(&"xhigh".to_string())), Some("high".to_string()));
        assert_eq!(clamp_reasoning(Some(&"max".to_string())), Some("high".to_string()));
        assert_eq!(clamp_reasoning(Some(&"low".to_string())), Some("low".to_string()));
        assert_eq!(clamp_reasoning(None), None);
    }

    #[test]
    fn adjusts_max_tokens_for_thinking() {
        // Default: base + budget capped at model max.
        let (max_tokens, budget) = adjust_max_tokens_for_thinking(Some(8000.0), 200_000.0, &"high".to_string(), None);
        assert_eq!(max_tokens, 24_384.0);
        assert_eq!(budget, 16_384.0);
        // Undefined base: model max, thinking fits inside.
        let (max_tokens, budget) = adjust_max_tokens_for_thinking(None, 20_000.0, &"high".to_string(), None);
        assert_eq!(max_tokens, 20_000.0);
        assert_eq!(budget, 16_384.0);
        // Budget collides with model max: answer tokens are reserved.
        let (max_tokens, budget) = adjust_max_tokens_for_thinking(Some(1000.0), 10_000.0, &"high".to_string(), None);
        assert_eq!(max_tokens, 10_000.0);
        assert_eq!(budget, 8_976.0);
        // Custom budgets.
        let custom = ThinkingBudgets {
            minimal: None,
            low: Some(512.0),
            medium: None,
            high: None,
        };
        let (max_tokens, budget) = adjust_max_tokens_for_thinking(Some(1000.0), 200_000.0, &"low".to_string(), Some(&custom));
        assert_eq!(max_tokens, 1512.0);
        assert_eq!(budget, 512.0);
    }
}
