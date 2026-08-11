//! OpenAI prompt cache key clamping, port of
//! `packages/ai/src/api/openai-prompt-cache.ts`.

pub const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

/// Mirrors `clampOpenAIPromptCacheKey`: clamps by Unicode scalar count
/// (JS `Array.from(key)` iterates code points).
pub fn clamp_openai_prompt_cache_key(key: Option<&str>) -> Option<String> {
    let key = key?;
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH {
        return Some(key.to_string());
    }
    Some(chars[..OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH].iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_long_keys_by_code_point() {
        assert_eq!(clamp_openai_prompt_cache_key(None), None);
        assert_eq!(
            clamp_openai_prompt_cache_key(Some("short")),
            Some("short".to_string())
        );
        let long = "x".repeat(100);
        let clamped = clamp_openai_prompt_cache_key(Some(&long)).unwrap();
        assert_eq!(clamped.len(), 64);
        // Multi-byte characters count as one.
        let emoji = "🙈".repeat(80);
        let clamped = clamp_openai_prompt_cache_key(Some(&emoji)).unwrap();
        assert_eq!(clamped.chars().count(), 64);
    }
}
