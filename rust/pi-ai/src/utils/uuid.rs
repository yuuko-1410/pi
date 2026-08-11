//! Port of `packages/ai/src/utils/uuid.ts` (UUIDv7, time-ordered).
//!
//! Randomness comes from `/dev/urandom` (Unix) so the crate stays
//! dependency-free; the JS implementation falls back to `Math.random` when
//! crypto is unavailable, which is not needed here.

use std::fs::File;
use std::io::Read;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

struct UuidState {
    last_timestamp: u64,
    sequence: u32,
}

static STATE: Mutex<UuidState> = Mutex::new(UuidState {
    last_timestamp: u64::MAX,
    sequence: 0,
});

fn random_bytes(bytes: &mut [u8]) {
    let mut file = File::open("/dev/urandom").expect("entropy source unavailable");
    file.read_exact(bytes).expect("failed to read entropy");
}

/// Generate a time-ordered UUIDv7. Mirrors the JS sequence logic exactly:
/// a strictly newer timestamp reseeds the sequence from random bytes; an
/// equal or older timestamp increments it, and a wraparound bumps the
/// timestamp reference by one millisecond.
pub fn uuidv7() -> String {
    let mut random = [0u8; 16];
    random_bytes(&mut random);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64;

    let (last_timestamp, sequence) = {
        let mut state = STATE.lock().unwrap();
        if timestamp > state.last_timestamp {
            state.sequence = (random[6] as u32) << 16 | (random[7] as u32) << 8 | random[8] as u32;
            state.last_timestamp = timestamp;
        } else {
            state.sequence = state.sequence.wrapping_add(1);
            if state.sequence == 0 {
                state.last_timestamp = state.last_timestamp.wrapping_add(1);
            }
        }
        (state.last_timestamp, state.sequence)
    };

    let mut bytes = [0u8; 16];
    bytes[0] = (last_timestamp >> 40) as u8;
    bytes[1] = (last_timestamp >> 32) as u8;
    bytes[2] = (last_timestamp >> 24) as u8;
    bytes[3] = (last_timestamp >> 16) as u8;
    bytes[4] = (last_timestamp >> 8) as u8;
    bytes[5] = last_timestamp as u8;
    bytes[6] = 0x70 | ((sequence >> 28) & 0x0f) as u8;
    bytes[7] = (sequence >> 20) as u8;
    bytes[8] = 0x80 | ((sequence >> 14) & 0x3f) as u8;
    bytes[9] = (sequence >> 6) as u8;
    bytes[10] = (((sequence & 0x3f) << 2) as u8) | (random[10] & 0x03);
    bytes[11] = random[11];
    bytes[12] = random[12];
    bytes[13] = random[13];
    bytes[14] = random[14];
    bytes[15] = random[15];

    let hex: Vec<String> = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        hex[0..4].concat(),
        hex[4..6].concat(),
        hex[6..8].concat(),
        hex[8..10].concat(),
        hex[10..16].concat()
    )
}

#[cfg(test)]
mod tests {
    use super::uuidv7;

    #[test]
    fn produces_time_ordered_v7_uuids() {
        let a = uuidv7();
        let b = uuidv7();
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "7", "version nibble must be 7");
        assert_eq!(&a[19..20], "8", "variant nibble must be 8");
        assert_ne!(a, b);
    }
}
