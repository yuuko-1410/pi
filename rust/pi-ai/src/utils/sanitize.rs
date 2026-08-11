//! Port of `packages/ai/src/utils/sanitize-unicode.ts`.
//!
//! The JS implementation strips unpaired UTF-16 surrogate code units, which
//! can exist in JS strings and break JSON serialization at some providers.
//! Rust `String` is valid UTF-8 by construction and cannot contain unpaired
//! surrogates, so the function is the identity here; it is kept so call
//! sites port 1:1.

/// Removes unpaired Unicode surrogate code units. Identity in Rust: `String`
/// cannot contain them (see module docs).
pub fn sanitize_surrogates(text: &str) -> String {
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::sanitize_surrogates;

    #[test]
    fn preserves_valid_text_including_emoji() {
        assert_eq!(sanitize_surrogates("Hello 🙈 World"), "Hello 🙈 World");
        assert_eq!(sanitize_surrogates("plain text"), "plain text");
        assert_eq!(sanitize_surrogates(""), "");
    }
}
