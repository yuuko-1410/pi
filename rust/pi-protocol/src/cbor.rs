//! Strict definite-length RFC 8949 subset CBOR.
//!
//! Rust port of `packages/protocol/src/cbor/*` with byte-identical behavior:
//! same limits, same error messages, same number encoding rules (all numbers
//! are f64, exactly like JS `number`).

use std::fmt;

pub const UINT32_BASE: f64 = 4_294_967_296.0;
pub const MAX_UINT32: u64 = 0xffff_ffff;
const MAX_CONFIGURED_DEPTH: u64 = 512;

/// Safe defaults for untrusted protocol payloads.
pub const DEFAULT_MAX_CBOR_BYTE_LENGTH: u64 = 16 * 1024 * 1024;
pub const DEFAULT_MAX_CBOR_CONTAINER_LENGTH: u64 = 1_000_000;
pub const DEFAULT_MAX_CBOR_DEPTH: u64 = 64;

const SAFE_INT_MAX: f64 = 9_007_199_254_740_991.0; // 2^53 - 1

/// Caller-provided limits, mirroring `CborOptions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CborOptions {
    /// Maximum encoded input/output bytes and maximum byte/text string length.
    pub max_byte_length: u64,
    /// Maximum number of elements in an array or entries in a map.
    pub max_container_length: u64,
    /// Maximum recursive item depth.
    pub max_depth: u64,
}

impl Default for CborOptions {
    fn default() -> Self {
        Self {
            max_byte_length: DEFAULT_MAX_CBOR_BYTE_LENGTH,
            max_container_length: DEFAULT_MAX_CBOR_CONTAINER_LENGTH,
            max_depth: DEFAULT_MAX_CBOR_DEPTH,
        }
    }
}

impl CborOptions {
    /// Mirror of `resolveOptions`: rejects out-of-range limits with a RangeError.
    pub fn resolve(
        max_byte_length: Option<u64>,
        max_container_length: Option<u64>,
        max_depth: Option<u64>,
    ) -> Result<Self, RangeError> {
        let max_byte_length = resolve_limit(
            "maxByteLength",
            max_byte_length,
            MAX_UINT32,
            DEFAULT_MAX_CBOR_BYTE_LENGTH,
        )?;
        let max_container_length = resolve_limit(
            "maxContainerLength",
            max_container_length,
            MAX_UINT32,
            DEFAULT_MAX_CBOR_CONTAINER_LENGTH,
        )?;
        let max_depth = resolve_limit("maxDepth", max_depth, MAX_CONFIGURED_DEPTH, DEFAULT_MAX_CBOR_DEPTH)?;
        Ok(Self {
            max_byte_length,
            max_container_length,
            max_depth,
        })
    }
}

fn resolve_limit(name: &str, value: Option<u64>, maximum: u64, default: u64) -> Result<u64, RangeError> {
    // JS also rejects non-integer, negative and non-safe values; the u64 type
    // rules those out at compile time, leaving only the upper bound.
    match value {
        Some(v) if v > maximum => Err(RangeError(format!("{name} must be an integer between 0 and {maximum}"))),
        Some(v) => Ok(v),
        None => Ok(default),
    }
}

/// A decoded or to-be-encoded CBOR value.
///
/// Mirrors the JS value space of the protocol: plain objects (insertion-ordered
/// string keys), arrays, and primitives. `Bytes` corresponds to `Uint8Array`.
/// Numbers are always f64, exactly like JS `number`.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Bytes(Vec<u8>),
    Array(Vec<Value>),
    /// Insertion-ordered string-keyed map, mirroring JS plain objects.
    Map(Vec<(String, Value)>),
}

impl Value {
    pub fn as_map(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Map(entries) => Some(entries),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CborError(pub String);

impl fmt::Display for CborError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for CborError {}

#[derive(Debug, Clone)]
pub struct RangeError(pub String);

impl fmt::Display for RangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for RangeError {}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

struct Writer {
    buffer: Vec<u8>,
    max_byte_length: u64,
}

impl Writer {
    fn new(max_byte_length: u64) -> Self {
        Self {
            buffer: Vec::with_capacity(256.min(max_byte_length as usize).max(1)),
            max_byte_length,
        }
    }

    fn ensure_capacity(&mut self, additional: usize) -> Result<(), CborError> {
        let required = self.buffer.len() as u64 + additional as u64;
        if required > self.max_byte_length {
            return Err(CborError(format!(
                "CBOR byte length exceeds configured limit of {}",
                self.max_byte_length
            )));
        }
        self.buffer.reserve(additional);
        Ok(())
    }

    fn write_byte(&mut self, value: u8) -> Result<(), CborError> {
        self.ensure_capacity(1)?;
        self.buffer.push(value);
        Ok(())
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), CborError> {
        self.ensure_capacity(bytes.len())?;
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    fn write_u16(&mut self, value: u16) -> Result<(), CborError> {
        self.ensure_capacity(2)?;
        self.buffer.extend_from_slice(&value.to_be_bytes());
        Ok(())
    }

    fn write_u32(&mut self, value: u32) -> Result<(), CborError> {
        self.ensure_capacity(4)?;
        self.buffer.extend_from_slice(&value.to_be_bytes());
        Ok(())
    }

    fn write_u64(&mut self, value: u64) -> Result<(), CborError> {
        self.ensure_capacity(8)?;
        self.buffer.extend_from_slice(&value.to_be_bytes());
        Ok(())
    }

    fn write_float64(&mut self, value: f64) -> Result<(), CborError> {
        self.ensure_capacity(9)?;
        self.buffer.push(0xfb);
        self.buffer.extend_from_slice(&value.to_be_bytes());
        Ok(())
    }
}

fn write_argument(writer: &mut Writer, major_type: u8, value: u64) -> Result<(), CborError> {
    let prefix = major_type << 5;
    if value < 24 {
        writer.write_byte(prefix | value as u8)
    } else if value <= 0xff {
        writer.write_byte(prefix | 24)?;
        writer.write_byte(value as u8)
    } else if value <= 0xffff {
        writer.write_byte(prefix | 25)?;
        writer.write_u16(value as u16)
    } else if value <= MAX_UINT32 {
        writer.write_byte(prefix | 26)?;
        writer.write_u32(value as u32)
    } else {
        writer.write_byte(prefix | 27)?;
        writer.write_u64(value)
    }
}

fn is_safe_integer(value: f64) -> bool {
    // Mirrors Number.isInteger: NaN and infinities fail (fract() is NaN).
    value.fract() == 0.0 && value.abs() <= SAFE_INT_MAX
}

fn encode_text(writer: &mut Writer, value: &str, options: &CborOptions) -> Result<(), CborError> {
    let bytes = value.as_bytes();
    if bytes.len() as u64 > options.max_byte_length {
        return Err(CborError(format!(
            "CBOR text string length exceeds configured limit of {}",
            options.max_byte_length
        )));
    }
    // JS round-trips through TextEncoder/TextDecoder to reject lone surrogates;
    // Rust `String` is valid UTF-8 by construction, so no check is needed.
    write_argument(writer, 3, bytes.len() as u64)?;
    writer.write_bytes(bytes)
}

fn encode_value(
    writer: &mut Writer,
    value: &Value,
    options: &CborOptions,
    depth: u64,
) -> Result<(), CborError> {
    if depth > options.max_depth {
        return Err(CborError(format!(
            "CBOR nesting depth exceeds configured limit of {}",
            options.max_depth
        )));
    }
    match value {
        Value::Null => writer.write_byte(0xf6),
        Value::Bool(true) => writer.write_byte(0xf5),
        Value::Bool(false) => writer.write_byte(0xf4),
        Value::Number(n) => {
            if !n.is_finite() {
                return Err(CborError("CBOR numbers must be finite".to_string()));
            }
            if n.fract() == 0.0 && !(*n == 0.0 && n.is_sign_negative()) {
                // Number.isInteger(value) && !Object.is(value, -0)
                if !is_safe_integer(*n) {
                    return Err(CborError("CBOR integers must be safe JavaScript integers".to_string()));
                }
                if *n >= 0.0 {
                    write_argument(writer, 0, *n as u64)
                } else {
                    write_argument(writer, 1, (-1.0 - n) as u64)
                }
            } else {
                writer.write_float64(*n)
            }
        }
        Value::String(s) => encode_text(writer, s, options),
        Value::Bytes(bytes) => {
            if bytes.len() as u64 > options.max_byte_length {
                return Err(CborError(format!(
                    "CBOR byte string length exceeds configured limit of {}",
                    options.max_byte_length
                )));
            }
            write_argument(writer, 2, bytes.len() as u64)?;
            writer.write_bytes(bytes)
        }
        Value::Array(items) => {
            if items.len() as u64 > options.max_container_length {
                return Err(CborError(format!(
                    "CBOR array length exceeds configured limit of {}",
                    options.max_container_length
                )));
            }
            write_argument(writer, 4, items.len() as u64)?;
            for item in items {
                encode_value(writer, item, options, depth + 1)?;
            }
            Ok(())
        }
        Value::Map(entries) => {
            if entries.len() as u64 > options.max_container_length {
                return Err(CborError(format!(
                    "CBOR map length exceeds configured limit of {}",
                    options.max_container_length
                )));
            }
            write_argument(writer, 5, entries.len() as u64)?;
            for (key, entry_value) in entries {
                encode_text(writer, key, options)?;
                encode_value(writer, entry_value, options, depth + 1)?;
            }
            Ok(())
        }
    }
}

/// Encodes the protocol's strict, definite-length RFC 8949 subset.
pub fn encode_cbor(value: &Value, options: &CborOptions) -> Result<Vec<u8>, CborError> {
    let mut writer = Writer::new(options.max_byte_length);
    encode_value(&mut writer, value, options, 0)?;
    Ok(writer.buffer)
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
    options: &'a CborOptions,
}

impl<'a> Reader<'a> {
    fn read_item(&mut self, depth: u64) -> Result<Value, CborError> {
        if depth > self.options.max_depth {
            return Err(CborError(format!(
                "CBOR nesting depth exceeds configured limit of {}",
                self.options.max_depth
            )));
        }
        let initial = self.read_byte()?;
        let major_type = initial >> 5;
        let additional_information = initial & 0x1f;

        match major_type {
            0 => Ok(Value::Number(self.read_argument(additional_information)? as f64)),
            1 => {
                let argument = self.read_argument(additional_information)?;
                // -1 - argument must be a safe integer: argument <= 2^53 - 2
                if argument > 9_007_199_254_740_990 {
                    // 2^53 - 2: -1 - argument must stay within safe integers
                    return Err(CborError("Decoded CBOR integer is outside the safe range".to_string()));
                }
                Ok(Value::Number(-1.0 - argument as f64))
            }
            2 => {
                let length = self.read_length(additional_information, "byte string")?;
                Ok(Value::Bytes(self.read_bytes(length)?.to_vec()))
            }
            3 => {
                let length = self.read_length(additional_information, "text string")?;
                let bytes = self.read_bytes(length)?;
                match std::str::from_utf8(bytes) {
                    Ok(text) => Ok(Value::String(text.to_string())),
                    Err(_) => Err(CborError("CBOR text string contains invalid UTF-8".to_string())),
                }
            }
            4 => {
                let length = self.read_length(additional_information, "array")?;
                // Grow incrementally like JS instead of preallocating by the
                // declared length: a truncated hostile wire must not trigger
                // a huge allocation before the truncation error is raised.
                let mut result = Vec::new();
                for _ in 0..length {
                    result.push(self.read_item(depth + 1)?);
                }
                Ok(Value::Array(result))
            }
            5 => {
                let length = self.read_length(additional_information, "map")?;
                let mut result: Vec<(String, Value)> = Vec::new();
                let mut keys: std::collections::HashSet<String> = std::collections::HashSet::new();
                for _ in 0..length {
                    let key = self.read_item(depth + 1)?;
                    let key = match key {
                        Value::String(s) => s,
                        _ => return Err(CborError("CBOR map keys must be strings".to_string())),
                    };
                    if !keys.insert(key.clone()) {
                        return Err(CborError("CBOR map contains a duplicate key".to_string()));
                    }
                    result.push((key, self.read_item(depth + 1)?));
                }
                Ok(Value::Map(result))
            }
            6 => Err(CborError("CBOR tags are not supported".to_string())),
            7 => self.read_simple(additional_information),
            _ => Err(CborError("Malformed CBOR major type".to_string())),
        }
    }

    fn read_simple(&mut self, additional_information: u8) -> Result<Value, CborError> {
        match additional_information {
            20 => Ok(Value::Bool(false)),
            21 => Ok(Value::Bool(true)),
            22 => Ok(Value::Null),
            27 => {
                let bytes = self.read_bytes(8)?;
                let value = f64::from_be_bytes(bytes.try_into().expect("8 bytes"));
                if !value.is_finite() {
                    return Err(CborError("Decoded CBOR number must be finite".to_string()));
                }
                if value.fract() == 0.0 && !is_safe_integer(value) {
                    return Err(CborError("Decoded CBOR integer is outside the safe range".to_string()));
                }
                Ok(Value::Number(value))
            }
            31 => Err(CborError("CBOR break marker is not supported".to_string())),
            _ => Err(CborError("Unsupported CBOR simple value or floating-point width".to_string())),
        }
    }

    fn read_length(&mut self, additional_information: u8, kind: &str) -> Result<u64, CborError> {
        if additional_information == 31 {
            return Err(CborError(format!("Indefinite-length CBOR {kind}s are not supported")));
        }
        let length = self.read_argument(additional_information)?;
        let limit = if kind == "array" || kind == "map" {
            self.options.max_container_length
        } else {
            self.options.max_byte_length
        };
        if length > limit {
            return Err(CborError(format!("CBOR {kind} length exceeds configured limit of {limit}")));
        }
        Ok(length)
    }

    fn read_argument(&mut self, additional_information: u8) -> Result<u64, CborError> {
        if additional_information < 24 {
            return Ok(additional_information as u64);
        }
        match additional_information {
            24 => Ok(self.read_byte()? as u64),
            25 => {
                let bytes = self.read_bytes(2)?;
                Ok(u16::from_be_bytes(bytes.try_into().expect("2 bytes")) as u64)
            }
            26 => {
                let bytes = self.read_bytes(4)?;
                Ok(u32::from_be_bytes(bytes.try_into().expect("4 bytes")) as u64)
            }
            27 => {
                let high = self.read_argument(26)?;
                let low = self.read_argument(26)?;
                if high > 0x1f_ffff {
                    return Err(CborError("Decoded CBOR integer or length is outside the safe range".to_string()));
                }
                Ok(high * UINT32_BASE as u64 + low)
            }
            31 => Err(CborError("Indefinite-length CBOR items are not supported".to_string())),
            _ => Err(CborError("Malformed CBOR additional information".to_string())),
        }
    }

    fn read_byte(&mut self) -> Result<u8, CborError> {
        if self.offset >= self.bytes.len() {
            return Err(CborError("Truncated CBOR payload".to_string()));
        }
        let value = self.bytes[self.offset];
        self.offset += 1;
        Ok(value)
    }

    fn read_bytes(&mut self, length: u64) -> Result<&'a [u8], CborError> {
        let end = self
            .offset
            .checked_add(length as usize)
            .ok_or_else(|| CborError("Truncated CBOR payload".to_string()))?;
        if end > self.bytes.len() {
            return Err(CborError("Truncated CBOR payload".to_string()));
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }
}

/// Decodes exactly one item from the protocol's strict RFC 8949 subset.
pub fn decode_cbor(bytes: &[u8], options: &CborOptions) -> Result<Value, CborError> {
    if bytes.len() as u64 > options.max_byte_length {
        return Err(CborError(format!(
            "CBOR byte length exceeds configured limit of {}",
            options.max_byte_length
        )));
    }
    let mut reader = Reader {
        bytes,
        offset: 0,
        options,
    };
    let value = reader.read_item(0)?;
    if reader.offset != bytes.len() {
        return Err(CborError("CBOR payload contains trailing data".to_string()));
    }
    Ok(value)
}
