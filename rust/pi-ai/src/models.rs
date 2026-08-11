//! Model registry helpers, port of the pure functions in
//! `packages/ai/src/models.ts` (the full registry/catalog follows with the
//! providers layer).

use crate::types::{Model, Usage};

pub const EXTENDED_THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

pub fn get_supported_thinking_levels(model: &Model) -> Vec<String> {
    if !model.reasoning {
        return vec!["off".to_string()];
    }

    EXTENDED_THINKING_LEVELS
        .iter()
        .filter(|level| {
            // JS: `const mapped = thinkingLevelMap?.[level]`; `mapped === null`
            // excludes explicit nulls, while a missing key (undefined) keeps
            // non-xhigh/max levels and excludes xhigh/max.
            let mapped: Option<&Option<String>> = model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.iter().find(|(key, _)| key.as_str() == **level))
                .map(|(_, value)| value);
            match mapped {
                Some(None) => false, // explicit null mapping
                None => !(**level == "xhigh" || **level == "max"),
                Some(Some(_)) => true,
            }
        })
        .map(|level| level.to_string())
        .collect()
}

/// Mirrors `clampThinkingLevel`: returns the requested level when supported,
/// otherwise the nearest supported level (higher first, then lower).
pub fn clamp_thinking_level(model: &Model, level: &str) -> String {
    let available_levels = get_supported_thinking_levels(model);
    if available_levels.iter().any(|available| available == level) {
        return level.to_string();
    }

    let requested_index = EXTENDED_THINKING_LEVELS.iter().position(|candidate| *candidate == level);
    let Some(requested_index) = requested_index else {
        return available_levels.first().cloned().unwrap_or_else(|| "off".to_string());
    };

    for i in requested_index..EXTENDED_THINKING_LEVELS.len() {
        let candidate = EXTENDED_THINKING_LEVELS[i];
        if available_levels.iter().any(|available| available == candidate) {
            return candidate.to_string();
        }
    }
    for i in (0..requested_index).rev() {
        let candidate = EXTENDED_THINKING_LEVELS[i];
        if available_levels.iter().any(|available| available == candidate) {
            return candidate.to_string();
        }
    }
    available_levels.first().cloned().unwrap_or_else(|| "off".to_string())
}

/// Mirrors `calculateCost`: computes usage cost from model rates, applying
/// request-wide pricing tiers and the Anthropic 2x long-write rule. Mutates
/// the cost inside `usage` and returns it.
pub fn calculate_cost(model: &Model, usage: &mut Usage) -> crate::types::UsageCost {
    let input_tokens = usage.input + usage.cache_read + usage.cache_write;
    let mut rates = model.cost.rates.clone();
    let mut matched_threshold = -1.0f64;
    if let Some(tiers) = &model.cost.tiers {
        for tier in tiers {
            if input_tokens > tier.input_tokens_above && tier.input_tokens_above > matched_threshold {
                rates = tier.rates.clone();
                matched_threshold = tier.input_tokens_above;
            }
        }
    }

    // Anthropic charges 2x base input for 1h cache writes.
    let long_write = usage.cache_write_1h.unwrap_or(0.0);
    let short_write = usage.cache_write - long_write;
    usage.cost.input = (rates.input / 1_000_000.0) * usage.input;
    usage.cost.output = (rates.output / 1_000_000.0) * usage.output;
    usage.cost.cache_read = (rates.cache_read / 1_000_000.0) * usage.cache_read;
    usage.cost.cache_write = (rates.cache_write * short_write + rates.input * 2.0 * long_write) / 1_000_000.0;
    usage.cost.total =
        usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
    usage.cost.clone()
}

/// Mirrors `modelsAreEqual`.
pub fn models_are_equal(a: Option<&Model>, b: Option<&Model>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.id == b.id && a.provider == b.provider,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModelCost, ModelCostRates};

    fn model(reasoning: bool, levels: Option<Vec<(String, Option<String>)>>) -> Model {
        Model {
            id: "m".to_string(),
            name: "m".to_string(),
            api: "test".to_string(),
            provider: "p".to_string(),
            base_url: "https://x".to_string(),
            reasoning,
            thinking_level_map: levels,
            input: vec!["text".to_string()],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 3.0,
                    output: 15.0,
                    cache_read: 0.3,
                    cache_write: 3.0,
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

    #[test]
    fn supported_levels_for_non_reasoning_models() {
        assert_eq!(get_supported_thinking_levels(&model(false, None)), vec!["off".to_string()]);
    }

    #[test]
    fn supported_levels_respect_null_mappings() {
        let levels = vec![
            ("off".to_string(), None),
            ("low".to_string(), Some("low".to_string())),
            ("high".to_string(), Some("high".to_string())),
            ("xhigh".to_string(), None),
            ("max".to_string(), Some("max".to_string())),
        ];
        let supported = get_supported_thinking_levels(&model(true, Some(levels)));
        // JS: explicit null mappings (off, xhigh) are excluded; missing keys
        // (minimal, medium) are kept for non-xhigh/max levels.
        assert_eq!(supported, vec!["minimal", "low", "medium", "high", "max"]);
    }

    #[test]
    fn clamps_to_nearest_supported_level() {
        // Full map: off -> null (excluded), low/high mapped, others missing.
        let levels = Some(vec![
            ("off".to_string(), None),
            ("low".to_string(), Some("low".to_string())),
            ("high".to_string(), Some("high".to_string())),
        ]);
        let m = model(true, levels);
        // Supported: minimal, low, medium, high (max/off/xhigh excluded;
        // verified against the JS implementation).
        assert_eq!(clamp_thinking_level(&m, "high"), "high");
        assert_eq!(clamp_thinking_level(&m, "medium"), "medium");
        assert_eq!(clamp_thinking_level(&m, "minimal"), "minimal");
        // Excluded levels clamp upward first, then downward.
        assert_eq!(clamp_thinking_level(&m, "off"), "minimal");
        assert_eq!(clamp_thinking_level(&m, "xhigh"), "high");
        assert_eq!(clamp_thinking_level(&m, "max"), "high");
        assert_eq!(clamp_thinking_level(&model(false, None), "high"), "off");
    }

    #[test]
    fn calculates_cost_with_tiers_and_long_writes() {
        let mut m = model(true, None);
        m.cost.tiers = Some(vec![crate::types::ModelCostTier {
            rates: ModelCostRates {
                input: 1.0,
                output: 5.0,
                cache_read: 0.1,
                cache_write: 1.0,
            },
            input_tokens_above: 1000.0,
        }]);
        let mut usage = Usage {
            input: 2000.0,
            output: 500.0,
            cache_read: 100.0,
            cache_write: 50.0,
            cache_write_1h: Some(10.0),
            reasoning: None,
            total_tokens: 2650.0,
            cost: crate::types::UsageCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
        };
        calculate_cost(&m, &mut usage);
        // Tier applies: input 2000*1e-6, output 500*5e-6, cacheRead 100*0.1e-6,
        // cacheWrite (40*1 + 10*2) e-6.
        assert_eq!(usage.cost.input, 0.002);
        assert_eq!(usage.cost.output, 0.0025);
        assert_eq!(usage.cost.cache_read, 0.00001);
        assert_eq!(usage.cost.cache_write, 0.00006);
        assert!((usage.cost.total - 0.00457).abs() < 1e-12);
    }
}
