//! Validated framed protocol API.
//!
//! Rust port of `packages/protocol/src/codec.ts` with identical error
//! wrapping rules: parse errors surface unchanged, while CBOR/framing errors
//! are wrapped in `ProtocolValidationError` with bounded messages.

use std::fmt;

use crate::cbor::{decode_cbor, encode_cbor, CborOptions};
use crate::framing::{assert_complete_frame, encode_frame, DEFAULT_MAX_FRAME_LENGTH, FrameDecoder};
use crate::schemas::{parse_client_message as parse_client_value, parse_server_message as parse_server_value};
use crate::schemas::{ClientMessage, ServerMessage};

#[derive(Debug, Clone)]
pub struct ProtocolValidationError(pub String);

impl fmt::Display for ProtocolValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ProtocolValidationError {}

/// Mirrors `boundedErrorMessage`: keeps messages at most 500 chars, with the
/// same truncation marker as JS (`...` after 497 chars).
fn bounded_error_message(error: &dyn fmt::Display) -> String {
    let message = error.to_string();
    if message.chars().count() <= 500 {
        message
    } else {
        let mut end = 497;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &message[..end])
    }
}

/// Mirrors `isSupportedProtocolVersion`: version is intentionally an integer,
/// not a coercible string.
pub fn is_supported_protocol_version(version: f64) -> bool {
    version.fract() == 0.0 && version == crate::schemas::PROTOCOL_VERSION
}

/// Validates and encodes one complete length-prefixed client message.
pub fn encode_client_message(
    message: &ClientMessage,
    max_frame_length: Option<u64>,
) -> Result<Vec<u8>, ProtocolValidationError> {
    encode_protocol_message(message, "client", max_frame_length)
}

/// Validates and encodes one complete length-prefixed server message.
pub fn encode_server_message(
    message: &ServerMessage,
    max_frame_length: Option<u64>,
) -> Result<Vec<u8>, ProtocolValidationError> {
    encode_protocol_message(message, "server", max_frame_length)
}

fn encode_protocol_message<T: ToProtocolValue>(
    value: &T,
    kind: &str,
    options: Option<u64>,
) -> Result<Vec<u8>, ProtocolValidationError> {
    // JS validates the in-memory message before encoding (parse errors
    // surface unmodified). Mirror that by parsing our own serialization:
    // Rust models can still carry schema-invalid f64 values (e.g. a
    // fractional hello version) that must not reach the wire.
    let protocol_value = value.to_protocol_value();
    match kind {
        "client" => {
            parse_client_message(&protocol_value)?;
        }
        _ => {
            parse_server_message(&protocol_value)?;
        }
    }
    let max_frame_length = options.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
    let cbor_options = CborOptions {
        max_byte_length: max_frame_length,
        ..CborOptions::default()
    };
    let cbor = encode_cbor(&value.to_protocol_value(), &cbor_options)
        .map_err(|error| wrap_encode_error(kind, &error))?;
    let frame = encode_frame(&cbor).map_err(|error| wrap_encode_error(kind, &error))?;
    assert_complete_frame(&frame, Some(max_frame_length))
        .map_err(|error| wrap_encode_error(kind, &error))?;
    Ok(frame)
}

fn wrap_encode_error(kind: &str, error: &dyn fmt::Display) -> ProtocolValidationError {
    ProtocolValidationError(format!(
        "Unable to encode {kind} protocol message: {}",
        bounded_error_message(error)
    ))
}

trait ToProtocolValue {
    fn to_protocol_value(&self) -> crate::cbor::Value;
}

impl ToProtocolValue for ClientMessage {
    fn to_protocol_value(&self) -> crate::cbor::Value {
        ClientMessage::to_value(self)
    }
}

impl ToProtocolValue for ServerMessage {
    fn to_protocol_value(&self) -> crate::cbor::Value {
        ServerMessage::to_value(self)
    }
}

pub fn parse_client_message(value: &crate::cbor::Value) -> Result<ClientMessage, ProtocolValidationError> {
    parse_client_value(value)
        .map_err(|()| ProtocolValidationError("Invalid client protocol message".to_string()))
}

pub fn parse_server_message(value: &crate::cbor::Value) -> Result<ServerMessage, ProtocolValidationError> {
    parse_server_value(value)
        .map_err(|()| ProtocolValidationError("Invalid server protocol message".to_string()))
}

/// Incrementally decodes and validates framed client/server messages.
pub struct ValidatedMessageDecoder<T> {
    frames: FrameDecoder,
    kind: &'static str,
    max_frame_length: u64,
    failed: bool,
    _marker: std::marker::PhantomData<T>,
}

impl<T> ValidatedMessageDecoder<T> {
    pub fn new(kind: &'static str, max_frame_length: Option<u64>) -> Result<Self, crate::cbor::RangeError> {
        let max_frame_length = max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
        Ok(Self {
            frames: FrameDecoder::with_max_frame_length(max_frame_length)?,
            kind,
            max_frame_length,
            failed: false,
            _marker: std::marker::PhantomData,
        })
    }

    fn fail(&mut self, message: String) -> ProtocolValidationError {
        self.failed = true;
        ProtocolValidationError(message)
    }

    /// Pushes a chunk, returning decoded messages. On any error the decoder is
    /// permanently failed, mirroring JS.
    pub fn push(
        &mut self,
        chunk: &[u8],
        parse: fn(&crate::cbor::Value) -> Result<T, ProtocolValidationError>,
    ) -> Result<Vec<T>, ProtocolValidationError> {
        if self.failed {
            return Err(ProtocolValidationError(format!(
                "{} message decoder has failed",
                self.kind
            )));
        }
        let frames = self.frames.push(chunk).map_err(|error| {
            self.fail(format!(
                "Invalid {} protocol frame: {}",
                self.kind,
                bounded_error_message(&error)
            ))
        })?;
        let mut messages = Vec::with_capacity(frames.len());
        for frame in frames {
            let value = decode_cbor(
                &frame,
                &CborOptions {
                    max_byte_length: self.max_frame_length,
                    ..CborOptions::default()
                },
            )
            .map_err(|error| {
                self.fail(format!(
                    "Invalid {} protocol frame: {}",
                    self.kind,
                    bounded_error_message(&error)
                ))
            })?;
            match parse(&value) {
                Ok(message) => messages.push(message),
                // Parse errors surface unmodified (JS rethrows ProtocolValidationError).
                Err(error) => {
                    self.failed = true;
                    return Err(error);
                }
            }
        }
        Ok(messages)
    }

    pub fn end(&mut self) -> Result<(), ProtocolValidationError> {
        if self.failed {
            return Err(ProtocolValidationError(format!(
                "{} message decoder has failed",
                self.kind
            )));
        }
        self.frames.end().map_err(|error| {
            self.fail(format!(
                "Invalid {} protocol framing: {}",
                self.kind,
                bounded_error_message(&error)
            ))
        })
    }
}

pub type ClientMessageDecoder = ValidatedMessageDecoder<ClientMessage>;
pub type ServerMessageDecoder = ValidatedMessageDecoder<ServerMessage>;

impl ClientMessageDecoder {
    pub fn push_messages(&mut self, chunk: &[u8]) -> Result<Vec<ClientMessage>, ProtocolValidationError> {
        self.push(chunk, parse_client_message)
    }
}

impl ServerMessageDecoder {
    pub fn push_messages(&mut self, chunk: &[u8]) -> Result<Vec<ServerMessage>, ProtocolValidationError> {
        self.push(chunk, parse_server_message)
    }
}
