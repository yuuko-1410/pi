//! Prompt-cache waste accounting, port of `core/cache-stats.ts`.

use pi_ai::types::{AssistantMessage, Message};

use super::session_types::SessionEntry;

/// Prompt-cache TTL: idle gaps longer than this are worth mentioning as the
/// likely cause of a miss. Anthropic's default cache TTL is 5 minutes.
pub const CACHE_TTL_MS: f64 = 5.0 * 60.0 * 1000.0;

/// Per-turn misses at or below this are cache breakpoint granularity noise.
const NOISE_FLOOR_TOKENS: f64 = 1024.0;

#[derive(Clone, Debug, PartialEq)]
pub struct CacheMiss {
    pub missed_tokens: f64,
    pub missed_cost: f64,
    pub idle_ms: f64,
    pub model_changed: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CacheWasteTotals {
    pub missed_tokens: f64,
    pub missed_cost: f64,
    pub miss_count: i64,
}

/// Minimal pricing lookup, satisfied by ModelRuntime. Cost is $/million tokens.
pub trait ModelPriceSource {
    fn get_model(&self, provider: &str, model_id: &str) -> Option<ModelPrice>;
}

pub struct ModelPrice {
    pub cache_read: f64,
}

/// The last request seen by the scan; everything in its prompt should be cached.
#[derive(Clone)]
struct PreviousRequest {
    prompt_tokens: f64,
    model_key: String,
    timestamp: f64,
    /// Sticky: some earlier request in this scan segment reported cache
    /// activity (distinguishes a total miss on a cache-read-only provider
    /// from one that never reports caching).
    reported_cache: bool,
}

fn prompt_tokens(message: &AssistantMessage) -> f64 {
    message.usage.input + message.usage.cache_read + message.usage.cache_write
}

/// Compute the cache miss for one assistant message relative to the previous
/// request; None when nothing is counted.
fn detect_miss(
    prev: Option<&PreviousRequest>,
    message: &AssistantMessage,
    models: &dyn ModelPriceSource,
) -> Option<CacheMiss> {
    let usage = &message.usage;
    let prompt_tokens = prompt_tokens(message);
    // A zero-cache turn only counts when cache activity was reported before.
    if prev.is_none()
        || prompt_tokens <= 0.0
        || (usage.cache_read + usage.cache_write == 0.0 && !prev.unwrap().reported_cache)
    {
        return None;
    }
    let prev = prev.unwrap();

    let missed_tokens = prev.prompt_tokens.min(prompt_tokens) - usage.cache_read;
    if missed_tokens <= NOISE_FLOOR_TOKENS {
        return None;
    }

    let paid_tokens = usage.input + usage.cache_write;
    let paid_per_token = if paid_tokens > 0.0 {
        (usage.cost.input + usage.cost.cache_write) / paid_tokens
    } else {
        0.0
    };
    let read_per_token = if usage.cache_read > 0.0 {
        usage.cost.cache_read / usage.cache_read
    } else {
        (models.get_model(&message.provider, &message.model).map(|m| m.cache_read).unwrap_or(0.0)) / 1_000_000.0
    };

    Some(CacheMiss {
        missed_tokens,
        missed_cost: missed_tokens * (paid_per_token - read_per_token).max(0.0),
        idle_ms: (message.timestamp - prev.timestamp).max(0.0),
        model_changed: format!("{}/{}", message.provider, message.model) != prev.model_key,
    })
}

fn as_previous_request(message: &AssistantMessage, reported_cache: bool) -> Option<PreviousRequest> {
    let prompt_tokens = prompt_tokens(message);
    if prompt_tokens <= 0.0 {
        return None;
    }
    Some(PreviousRequest {
        prompt_tokens,
        model_key: format!("{}/{}", message.provider, message.model),
        timestamp: message.timestamp,
        reported_cache: reported_cache || message.usage.cache_read + message.usage.cache_write > 0.0,
    })
}

fn scan(
    entries: &[SessionEntry],
    models: &dyn ModelPriceSource,
) -> (Option<PreviousRequest>, CacheWasteTotals, Vec<(usize, CacheMiss)>) {
    let mut prev: Option<PreviousRequest> = None;
    let mut totals = CacheWasteTotals::default();
    let mut misses: Vec<(usize, CacheMiss)> = Vec::new();

    for (index, entry) in entries.iter().enumerate() {
        match entry {
            SessionEntry::Compaction { .. } | SessionEntry::BranchSummary { .. } => {
                // The context legitimately changed; the next turn's prompt is
                // new content. Model switches are NOT exempt.
                prev = None;
            }
            SessionEntry::Message {
                message: super::session_types::SessionMessage::Llm(Message::Assistant(assistant)),
                ..
            } => {
                if let Some(miss) = detect_miss(prev.as_ref(), assistant, models) {
                    totals.missed_tokens += miss.missed_tokens;
                    totals.missed_cost += miss.missed_cost;
                    totals.miss_count += 1;
                    misses.push((index, miss));
                }
                prev = as_previous_request(assistant, prev.as_ref().map(|p| p.reported_cache).unwrap_or(false))
                    .or(prev);
            }
            _ => {}
        }
    }
    (prev, totals, misses)
}

/// Cumulative cache waste across a session.
pub fn compute_cache_waste(entries: &[SessionEntry], models: &dyn ModelPriceSource) -> CacheWasteTotals {
    scan(entries, models).1
}

/// All counted cache misses across a session, keyed by the entry index of the
/// assistant message that paid for them.
pub fn collect_cache_misses(entries: &[SessionEntry], models: &dyn ModelPriceSource) -> Vec<(usize, CacheMiss)> {
    scan(entries, models).2
}

/// Detect a cache miss on a just-completed assistant message. `entries` must
/// not yet contain `message` (message_end fires before persistence).
pub fn detect_cache_miss(
    entries: &[SessionEntry],
    message: &AssistantMessage,
    models: &dyn ModelPriceSource,
) -> Option<CacheMiss> {
    detect_miss(scan(entries, models).0.as_ref(), message, models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::{Usage, UsageCost};

    fn assistant(model: &str, input: f64, cache_read: f64, cache_write: f64, timestamp: f64) -> AssistantMessage {
        AssistantMessage {
            content: vec![],
            api: "api".into(),
            provider: "anthropic".into(),
            model: model.to_string(),
            response_model: None,
            response_id: None,
            usage: Usage {
                input,
                output: 0.0,
                cache_read,
                cache_write,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: input + cache_read + cache_write,
                cost: UsageCost {
                    input: input * 3.0,
                    output: 0.0,
                    cache_read: cache_read * 0.3,
                    cache_write: cache_write * 3.75,
                    total: 0.0,
                },
            },
            stop_reason: pi_ai::types::StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp,
        }
    }

    fn message_entry(message: AssistantMessage) -> SessionEntry {
        SessionEntry::Message {
            base: super::super::session_types::SessionEntryBase {
                id: String::new(),
                parent_id: None,
                timestamp: String::new(),
            },
            message: super::super::session_types::SessionMessage::Llm(Message::Assistant(message)),
        }
    }

    struct NoModels;
    impl ModelPriceSource for NoModels {
        fn get_model(&self, _provider: &str, _model_id: &str) -> Option<ModelPrice> {
            None
        }
    }

    #[test]
    fn first_turn_counts_nothing() {
        let entries = vec![message_entry(assistant("m", 1000.0, 0.0, 0.0, 1.0))];
        let totals = compute_cache_waste(&entries, &NoModels);
        assert_eq!(totals.miss_count, 0);
        assert_eq!(totals.missed_tokens, 0.0);
    }

    #[test]
    fn cache_hit_counts_nothing() {
        let entries = vec![
            message_entry(assistant("m", 0.0, 5000.0, 1000.0, 1.0)),
            message_entry(assistant("m", 0.0, 6000.0, 0.0, 2.0)),
        ];
        let totals = compute_cache_waste(&entries, &NoModels);
        assert_eq!(totals.miss_count, 0);
    }

    #[test]
    fn total_miss_counts() {
        // First request reports cache activity (cacheWrite).
        // Second request: same prompt re-billed -> full miss counted.
        let entries = vec![
            message_entry(assistant("m", 5000.0, 0.0, 5000.0, 1.0)),
            message_entry(assistant("m", 10000.0, 0.0, 0.0, 2.0)),
        ];
        let totals = compute_cache_waste(&entries, &NoModels);
        assert_eq!(totals.miss_count, 1);
        // prev.prompt_tokens (10000) min current (10000) - cacheRead (0).
        assert_eq!(totals.missed_tokens, 10000.0);
        // paidPerToken = 3.0, readPerToken = 0 -> missedCost = 30000.
        assert_eq!(totals.missed_cost, 30000.0);
    }

    #[test]
    fn miss_below_noise_floor_ignored() {
        let entries = vec![
            message_entry(assistant("m", 500.0, 0.0, 0.0, 1.0)),
            message_entry(assistant("m", 500.0, 0.0, 0.0, 2.0)),
        ];
        let totals = compute_cache_waste(&entries, &NoModels);
        assert_eq!(totals.miss_count, 0);
    }

    #[test]
    fn compaction_resets_previous() {
        let entries = vec![
            message_entry(assistant("m", 5000.0, 0.0, 0.0, 1.0)),
            SessionEntry::Compaction {
                base: super::super::session_types::SessionEntryBase {
                    id: "c".into(),
                    parent_id: None,
                    timestamp: "".into(),
                },
                summary: "s".into(),
                first_kept_entry_id: "e".into(),
                tokens_before: 0.0,
                details: None,
                usage: None,
                from_hook: None,
                first_kept_entry_index: None,
            },
            message_entry(assistant("m", 5000.0, 0.0, 0.0, 3.0)),
        ];
        let totals = compute_cache_waste(&entries, &NoModels);
        assert_eq!(totals.miss_count, 0);
    }

    #[test]
    fn model_change_sticky_cache() {
        // Provider never reports cache (zero cacheRead+Write both turns) but
        // first turn reported nothing -> second turn not counted.
        let entries = vec![
            message_entry(assistant("a", 5000.0, 0.0, 0.0, 1.0)),
            message_entry(assistant("b", 5000.0, 0.0, 0.0, 2.0)),
        ];
        let totals = compute_cache_waste(&entries, &NoModels);
        assert_eq!(totals.miss_count, 0);

        // Once cache activity was reported, zero-cache turns count as misses.
        let entries = vec![
            message_entry(assistant("a", 0.0, 5000.0, 0.0, 1.0)),
            message_entry(assistant("b", 5000.0, 0.0, 0.0, 2.0)),
        ];
        let totals = compute_cache_waste(&entries, &NoModels);
        assert_eq!(totals.miss_count, 1);
        assert!(totals.missed_cost > 0.0);
    }

    #[test]
    fn detect_cache_miss_uses_scan_state() {
        // First request reports cache activity so the miss on the next
        // message is countable (JS semantics).
        let entries = vec![message_entry(assistant("m", 5000.0, 0.0, 5000.0, 1.0))];
        let miss = detect_cache_miss(&entries, &assistant("m", 10000.0, 0.0, 0.0, 2.0), &NoModels).unwrap();
        assert_eq!(miss.missed_tokens, 10000.0);
        assert!(!miss.model_changed);
    }
}
