//! Port of `packages/ai/src/utils/hash.ts`.
//!
//! The JS implementation iterates UTF-16 code units (`charCodeAt`) with
//! 32-bit wrapping arithmetic (`Math.imul`, `>>>`) and emits base-36.

// JS Math.imul constants as i32 (unsigned bit patterns).
const IMUL_A: i32 = 2654435761u32 as i32;
const IMUL_B: i32 = 1597334677u32 as i32;
const IMUL_C: i32 = 2246822507u32 as i32;
const IMUL_D: i32 = 3266489909u32 as i32;

fn to_base36(value: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_string();
    }
    let mut value = value;
    let mut result = Vec::new();
    while value > 0 {
        result.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    result.reverse();
    String::from_utf8(result).expect("base36 digits are ASCII")
}

/// Fast deterministic hash to shorten long strings.
pub fn short_hash(str: &str) -> String {
    let mut h1: i32 = -559_038_737; // 0xdeadbeef as i32
    let mut h2: i32 = 1_103_547_991; // 0x41c6ce57 as i32
    for unit in str.encode_utf16() {
        let ch = unit as i32;
        h1 = (h1 ^ ch).wrapping_mul(IMUL_A);
        h2 = (h2 ^ ch).wrapping_mul(IMUL_B);
    }
    h1 = (h1 ^ (h1 as u32 >> 16) as i32)
        .wrapping_mul(IMUL_C)
        ^ (h2 ^ (h2 as u32 >> 13) as i32).wrapping_mul(IMUL_D);
    h2 = (h2 ^ (h2 as u32 >> 16) as i32)
        .wrapping_mul(IMUL_C)
        ^ (h1 ^ (h1 as u32 >> 13) as i32).wrapping_mul(IMUL_D);
    format!("{}{}", to_base36(h2 as u32), to_base36(h1 as u32))
}

#[cfg(test)]
mod tests {
    use super::short_hash;

    #[test]
    fn is_deterministic_and_matches_js_shape() {
        // Verified against the JS implementation: short_hash("hello") ==
        // "1h6qa0qrowduu", short_hash("") == "k4n83c7h0j2b".
        assert_eq!(short_hash("hello"), "1h6qa0qrowduu");
        assert_eq!(short_hash("hello"), short_hash("hello"));
        assert_eq!(short_hash(""), "k4n83c7h0j2b");
        assert_eq!(short_hash(""), short_hash(""));
        // Unicode: surrogate pairs are hashed per UTF-16 code unit, like JS.
        let emoji = "🙈";
        assert_eq!(emoji.encode_utf16().count(), 2);
        assert_eq!(short_hash(emoji), "kphsz0153ms3q");
    }
}
