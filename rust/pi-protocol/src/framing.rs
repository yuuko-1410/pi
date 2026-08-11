//! Incremental length-prefixed binary framing.
//!
//! Rust port of `packages/protocol/src/framing.ts` with identical semantics:
//! 4-byte big-endian unsigned length prefix, 64 KiB payload blocks, and the
//! same error states and messages.

use std::fmt;

use crate::cbor::RangeError;

const FRAME_HEADER_LENGTH: usize = 4;
const MAX_UINT32: u64 = 0xffff_ffff;
const PAYLOAD_BLOCK_SIZE: usize = 64 * 1024;

/// Default upper bound for one framed CBOR payload.
pub const DEFAULT_MAX_FRAME_LENGTH: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct FrameError(pub String);

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for FrameError {}

fn resolve_max_frame_length(value: Option<u64>) -> Result<u64, RangeError> {
    match value {
        Some(v) if v > MAX_UINT32 => Err(RangeError(format!(
            "maxFrameLength must be an integer between 0 and {MAX_UINT32}"
        ))),
        Some(v) => Ok(v),
        None => Ok(DEFAULT_MAX_FRAME_LENGTH),
    }
}

/// Prefixes a payload with its unsigned 32-bit big-endian byte length.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, RangeError> {
    if payload.len() as u64 > MAX_UINT32 {
        return Err(RangeError(
            "Frame payload exceeds the unsigned 32-bit length limit".to_string(),
        ));
    }
    let length = payload.len();
    let mut frame = Vec::with_capacity(FRAME_HEADER_LENGTH + length);
    frame.extend_from_slice(&(length as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Validates that bytes contain exactly one complete frame within the configured limit.
pub fn assert_complete_frame(frame: &[u8], options: Option<u64>) -> Result<(), FrameError> {
    if frame.len() < FRAME_HEADER_LENGTH {
        return Err(FrameError(
            "Frame does not contain a complete length prefix".to_string(),
        ));
    }
    let length = u32::from_be_bytes(frame[..FRAME_HEADER_LENGTH].try_into().expect("4 bytes")) as u64;
    let max_frame_length = resolve_max_frame_length(options).map_err(|e| FrameError(e.0))?;
    if length > max_frame_length {
        return Err(FrameError(format!(
            "Frame length {length} exceeds configured limit of {max_frame_length}"
        )));
    }
    if frame.len() as u64 != FRAME_HEADER_LENGTH as u64 + length {
        return Err(FrameError(
            "Frame must contain exactly one complete payload".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecoderState {
    Open,
    Ended,
    Failed,
}

/// Incrementally splits arbitrary byte chunks into length-prefixed payloads.
#[derive(Debug)]
pub struct FrameDecoder {
    header: [u8; FRAME_HEADER_LENGTH],
    header_length: usize,
    max_frame_length: u64,
    payload_blocks: Vec<Vec<u8>>,
    current_payload_block_length: usize,
    expected_payload_length: Option<u64>,
    payload_length: u64,
    state: DecoderState,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self {
            header: [0; FRAME_HEADER_LENGTH],
            header_length: 0,
            max_frame_length: DEFAULT_MAX_FRAME_LENGTH,
            payload_blocks: Vec::new(),
            current_payload_block_length: 0,
            expected_payload_length: None,
            payload_length: 0,
            state: DecoderState::Open,
        }
    }

    /// Constructor mirroring `new FrameDecoder({ maxFrameLength })`.
    pub fn with_max_frame_length(max_frame_length: u64) -> Result<Self, RangeError> {
        resolve_max_frame_length(Some(max_frame_length))?;
        let mut decoder = Self::new();
        decoder.max_frame_length = max_frame_length;
        Ok(decoder)
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
        if self.state == DecoderState::Ended {
            return Err(FrameError("Frame decoder has ended".to_string()));
        }
        if self.state == DecoderState::Failed {
            return Err(FrameError("Frame decoder has failed".to_string()));
        }

        let mut frames: Vec<Vec<u8>> = Vec::new();
        let mut chunk_offset = 0;
        while chunk_offset < chunk.len() {
            if self.expected_payload_length.is_none() {
                let header_bytes = (FRAME_HEADER_LENGTH - self.header_length).min(chunk.len() - chunk_offset);
                self.header[self.header_length..self.header_length + header_bytes]
                    .copy_from_slice(&chunk[chunk_offset..chunk_offset + header_bytes]);
                self.header_length += header_bytes;
                chunk_offset += header_bytes;
                if self.header_length < FRAME_HEADER_LENGTH {
                    continue;
                }

                let frame_length = u32::from_be_bytes(self.header) as u64;
                self.header_length = 0;
                if frame_length > self.max_frame_length {
                    return Err(self.fail(format!(
                        "Frame length {frame_length} exceeds configured limit of {}",
                        self.max_frame_length
                    )));
                }
                if frame_length == 0 {
                    frames.push(Vec::new());
                    continue;
                }
                self.expected_payload_length = Some(frame_length);
                self.payload_blocks.clear();
                self.current_payload_block_length = 0;
                self.payload_length = 0;
            }

            let expected_payload_length = self.expected_payload_length.expect("set above");
            while chunk_offset < chunk.len() && self.payload_length < expected_payload_length {
                if self.current_payload_block_length == 0
                    || self.current_payload_block_length
                        == self.payload_blocks.last().map_or(0, |block| block.len())
                {
                    let block_size =
                        (PAYLOAD_BLOCK_SIZE as u64).min(expected_payload_length - self.payload_length) as usize;
                    self.payload_blocks.push(vec![0u8; block_size]);
                    self.current_payload_block_length = 0;
                }
                let block = self.payload_blocks.last_mut().expect("pushed above");
                let payload_bytes = (block.len() - self.current_payload_block_length).min(chunk.len() - chunk_offset);
                block[self.current_payload_block_length..self.current_payload_block_length + payload_bytes]
                    .copy_from_slice(&chunk[chunk_offset..chunk_offset + payload_bytes]);
                self.current_payload_block_length += payload_bytes;
                self.payload_length += payload_bytes as u64;
                chunk_offset += payload_bytes;
            }
            if self.payload_length == expected_payload_length {
                if self.payload_blocks.len() == 1 {
                    // Single block: move it out without copying, mirroring JS
                    // frames.push(this.payloadBlocks[0]).
                    frames.push(self.payload_blocks.pop().expect("one block"));
                } else {
                    let payload = self.payload_blocks.concat();
                    frames.push(payload);
                }
                self.payload_blocks.clear();
                self.current_payload_block_length = 0;
                self.expected_payload_length = None;
                self.payload_length = 0;
            }
        }
        Ok(frames)
    }

    pub fn end(&mut self) -> Result<(), FrameError> {
        if self.state == DecoderState::Ended {
            return Err(FrameError("Frame decoder has ended".to_string()));
        }
        if self.state == DecoderState::Failed {
            return Err(FrameError("Frame decoder has failed".to_string()));
        }
        if self.header_length != 0 || self.expected_payload_length.is_some() {
            return Err(self.fail("Truncated frame at end of stream".to_string()));
        }
        self.state = DecoderState::Ended;
        Ok(())
    }

    fn fail(&mut self, message: String) -> FrameError {
        self.state = DecoderState::Failed;
        self.header_length = 0;
        self.payload_blocks.clear();
        self.current_payload_block_length = 0;
        self.expected_payload_length = None;
        self.payload_length = 0;
        FrameError(message)
    }
}
