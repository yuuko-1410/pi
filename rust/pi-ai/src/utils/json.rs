//! JSON parsing with repair and partial-parse support.
//!
//! Port of `packages/ai/src/utils/json-parse.ts`, including a faithful port
//! of the `partial-json` 0.1.7 `parseJSON` algorithm (used with
//! `Allow.ALL` by `parseStreamingJson`).
//!
//! Note on UTF-16: JS indexes strings by UTF-16 code units; `repairJson`
//! operates on those units. Rust `&str` is UTF-8; iterating `chars()` is
//! equivalent for all valid JSON (JSON text is Unicode scalar sequences), and
//! unpaired surrogates cannot occur in a Rust string, so the only divergence
//! is on inputs that cannot be represented.

use pi_protocol::Value;

const VALID_JSON_ESCAPES: [char; 9] = ['"', '\\', '/', 'b', 'f', 'n', 'r', 't', 'u'];

fn is_control_character(c: char) -> bool {
    let code = c as u32;
    code <= 0x1f
}

fn escape_control_character(c: char) -> String {
    match c {
        '\u{08}' => "\\b".to_string(),
        '\u{0c}' => "\\f".to_string(),
        '\n' => "\\n".to_string(),
        '\r' => "\\r".to_string(),
        '\t' => "\\t".to_string(),
        _ => format!("\\u{:04x}", c as u32),
    }
}

/// Repairs malformed JSON string literals by escaping raw control characters
/// inside strings and doubling backslashes before invalid escape characters.
pub fn repair_json(json: &str) -> String {
    let mut repaired = String::with_capacity(json.len());
    let mut in_string = false;

    let chars: Vec<char> = json.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];

        if !in_string {
            repaired.push(c);
            if c == '"' {
                in_string = true;
            }
            index += 1;
            continue;
        }

        if c == '"' {
            repaired.push(c);
            in_string = false;
            index += 1;
            continue;
        }

        if c == '\\' {
            let next = chars.get(index + 1);
            match next {
                None => {
                    repaired.push_str("\\\\");
                    index += 1;
                    continue;
                }
                Some('u') => {
                    let digits: String = chars[index + 2..index + 6].iter().collect();
                    if digits.len() == 4 && digits.chars().all(|d| d.is_ascii_hexdigit()) {
                        repaired.push_str("\\u");
                        repaired.push_str(&digits);
                        index += 6; // skip \ u d d d d
                        continue;
                    }
                }
                Some(next) if VALID_JSON_ESCAPES.contains(next) => {
                    repaired.push('\\');
                    repaired.push(*next);
                    index += 2;
                    continue;
                }
                Some(_) => {
                    repaired.push_str("\\\\");
                    index += 1;
                    continue;
                }
            }
        }

        if is_control_character(c) {
            repaired.push_str(&escape_control_character(c));
        } else {
            repaired.push(c);
        }
        index += 1;
    }

    repaired
}

pub fn parse_json_with_repair<T>(json: &str) -> Result<T, Box<dyn std::error::Error>>
where
    T: FromJson,
{
    match T::parse(json) {
        Ok(value) => Ok(value),
        Err(original) => {
            let repaired = repair_json(json);
            if repaired != json {
                T::parse(&repaired)
            } else {
                Err(original)
            }
        }
    }
}

/// Minimal JSON string -> Value parse used by this module (strict, matching
/// `JSON.parse` for the JSON value space).
pub trait FromJson {
    fn parse(json: &str) -> Result<Self, Box<dyn std::error::Error>>
    where
        Self: Sized;
}

impl FromJson for Value {
    fn parse(json: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let mut parser = StrictJsonParser {
            chars: json.chars().collect(),
            index: 0,
        };
        let value = parser.parse_any()?;
        parser.skip_blank();
        if parser.index != parser.chars.len() {
            return Err("Unexpected token".into());
        }
        Ok(value)
    }
}

struct StrictJsonParser {
    chars: Vec<char>,
    index: usize,
}

impl StrictJsonParser {
    fn skip_blank(&mut self) {
        while self.index < self.chars.len() && " \n\r\t".contains(self.chars[self.index]) {
            self.index += 1;
        }
    }

    fn parse_any(&mut self) -> Result<Value, Box<dyn std::error::Error>> {
        self.skip_blank();
        if self.index >= self.chars.len() {
            return Err("Unexpected end of input".into());
        }
        match self.chars[self.index] {
            '"' => self.parse_string().map(Value::String),
            '{' => self.parse_object(),
            '[' => self.parse_array(),
            't' => {
                self.expect("true")?;
                Ok(Value::Bool(true))
            }
            'f' => {
                self.expect("false")?;
                Ok(Value::Bool(false))
            }
            'n' => {
                self.expect("null")?;
                Ok(Value::Null)
            }
            _ => self.parse_number(),
        }
    }

    fn expect(&mut self, literal: &str) -> Result<(), Box<dyn std::error::Error>> {
        let rest: String = self.chars[self.index..].iter().collect();
        if rest.starts_with(literal) {
            self.index += literal.chars().count();
            Ok(())
        } else {
            Err("Unexpected token".into())
        }
    }

    fn parse_string(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        self.index += 1; // opening quote
        let mut result = String::new();
        loop {
            if self.index >= self.chars.len() {
                return Err("Unterminated string".into());
            }
            let c = self.chars[self.index];
            self.index += 1;
            match c {
                '"' => return Ok(result),
                '\\' => {
                    if self.index >= self.chars.len() {
                        return Err("Unterminated escape".into());
                    }
                    let escape = self.chars[self.index];
                    self.index += 1;
                    match escape {
                        '"' => result.push('"'),
                        '\\' => result.push('\\'),
                        '/' => result.push('/'),
                        'b' => result.push('\u{08}'),
                        'f' => result.push('\u{0c}'),
                        'n' => result.push('\n'),
                        'r' => result.push('\r'),
                        't' => result.push('\t'),
                        'u' => {
                            let digits: String = self.chars[self.index..self.index + 4].iter().collect();
                            self.index += 4;
                            let code = u32::from_str_radix(&digits, 16)
                                .map_err(|_| "Invalid unicode escape".to_string())?;
                            result.push(char::from_u32(code).ok_or("Invalid unicode escape")?);
                        }
                        _ => return Err("Invalid escape".into()),
                    }
                }
                c if (c as u32) < 0x20 => return Err("Control character in string".into()),
                c => result.push(c),
            }
        }
    }

    fn parse_number(&mut self) -> Result<Value, Box<dyn std::error::Error>> {
        let start = self.index;
        while self.index < self.chars.len() {
            let c = self.chars[self.index];
            if c.is_ascii_digit() || c == '-' || c == '+' || c == '.' || c == 'e' || c == 'E' {
                self.index += 1;
            } else {
                break;
            }
        }
        let text: String = self.chars[start..self.index].iter().collect();
        let number: f64 = text.parse().map_err(|_| "Invalid number".to_string())?;
        Ok(Value::Number(number))
    }

    fn parse_object(&mut self) -> Result<Value, Box<dyn std::error::Error>> {
        self.index += 1; // {
        let mut entries = Vec::new();
        self.skip_blank();
        if self.index < self.chars.len() && self.chars[self.index] == '}' {
            self.index += 1;
            return Ok(Value::Map(entries));
        }
        loop {
            self.skip_blank();
            if self.index >= self.chars.len() || self.chars[self.index] != '"' {
                return Err("Expected string key".into());
            }
            let key = self.parse_string()?;
            self.skip_blank();
            if self.index >= self.chars.len() || self.chars[self.index] != ':' {
                return Err("Expected colon".into());
            }
            self.index += 1;
            let value = self.parse_any()?;
            entries.push((key, value));
            self.skip_blank();
            if self.index < self.chars.len() && self.chars[self.index] == ',' {
                self.index += 1;
                continue;
            }
            if self.index < self.chars.len() && self.chars[self.index] == '}' {
                self.index += 1;
                return Ok(Value::Map(entries));
            }
            return Err("Expected ',' or '}'".into());
        }
    }

    fn parse_array(&mut self) -> Result<Value, Box<dyn std::error::Error>> {
        self.index += 1; // [
        let mut items = Vec::new();
        self.skip_blank();
        if self.index < self.chars.len() && self.chars[self.index] == ']' {
            self.index += 1;
            return Ok(Value::Array(items));
        }
        loop {
            let value = self.parse_any()?;
            items.push(value);
            self.skip_blank();
            if self.index < self.chars.len() && self.chars[self.index] == ',' {
                self.index += 1;
                continue;
            }
            if self.index < self.chars.len() && self.chars[self.index] == ']' {
                self.index += 1;
                return Ok(Value::Array(items));
            }
            return Err("Expected ',' or ']'".into());
        }
    }
}

// ---------------------------------------------------------------------------
// partial-json 0.1.7 algorithm (Allow.ALL semantics)
// ---------------------------------------------------------------------------

struct PartialParser {
    chars: Vec<char>,
    index: usize,
    length: usize,
}

impl PartialParser {
    fn mark_partial(&self, msg: &str) -> PartialJsonError {
        PartialJsonError(format!("{msg} at position {}", self.index))
    }

    fn malformed(&self, msg: &str) -> MalformedJsonError {
        MalformedJsonError(format!("{msg} at position {}", self.index))
    }

    fn parse_any(&mut self) -> Result<Value, ParseError> {
        self.skip_blank();
        if self.index >= self.length {
            return Err(ParseError::Partial(self.mark_partial("Unexpected end of input")));
        }
        let c = self.chars[self.index];
        match c {
            '"' => self.parse_string(),
            '{' => self.parse_object(),
            '[' => self.parse_array(),
            _ => {
                let rest: String = self.chars[self.index..].iter().collect();
                if rest.starts_with("null") {
                    self.index += 4;
                    return Ok(Value::Null);
                }
                if rest.starts_with("true") {
                    self.index += 4;
                    return Ok(Value::Bool(true));
                }
                if rest.starts_with("false") {
                    self.index += 5;
                    return Ok(Value::Bool(false));
                }
                if rest.starts_with("Infinity") {
                    self.index += 8;
                    return Ok(Value::Number(f64::INFINITY));
                }
                if rest.starts_with("-Infinity") {
                    self.index += 9;
                    return Ok(Value::Number(f64::NEG_INFINITY));
                }
                if rest.starts_with("NaN") {
                    self.index += 3;
                    return Ok(Value::Number(f64::NAN));
                }
                self.parse_number()
            }
        }
    }

    fn parse_string(&mut self) -> Result<Value, ParseError> {
        let start = self.index;
        let mut escape = false;
        self.index += 1; // skip initial quote
        while self.index < self.length
            && (self.chars[self.index] != '"' || (escape && self.chars[self.index - 1] == '\\'))
        {
            escape = self.chars[self.index] == '\\' && !escape;
            self.index += 1;
        }
        if self.index < self.length && self.chars[self.index] == '"' {
            // Consume the closing quote (the JS arithmetic skips the last
            // backslash only when it was counted as an escape).
            let slice: String = self.chars[start..self.index].iter().collect();
            self.index += 1;
            let mut candidate = format!("{slice}\"");
            if escape {
                // JS: substring(start, ++index - Number(escape)) — drop the
                // trailing backslash when it was an unpaired escape.
                candidate = format!("{}\"", &slice[..slice.len() - 1]);
            }
            return parse_json_strict(&candidate).map_err(|e| ParseError::Malformed(MalformedJsonError(e)));
        }
        // Unterminated string: allow partial by closing the quote.
        let slice: String = self.chars[start..self.index].iter().collect();
        let mut candidate = format!("{slice}\"");
        if escape && slice.ends_with('\\') {
            // Invalid escape sequence fallback: cut at the last backslash.
            if let Some(last) = slice.rfind('\\') {
                candidate = format!("{}\"", &slice[..last]);
            }
        }
        parse_json_strict(&candidate).map_err(|_| ParseError::Partial(self.mark_partial("Unterminated string literal")))
    }

    fn parse_object(&mut self) -> Result<Value, ParseError> {
        self.index += 1; // skip initial brace
        self.skip_blank();
        let mut entries: Vec<(String, Value)> = Vec::new();
        loop {
            if self.index < self.length && self.chars[self.index] == '}' {
                self.index += 1;
                return Ok(Value::Map(entries));
            }
            self.skip_blank();
            if self.index >= self.length {
                return Ok(Value::Map(entries)); // partial object allowed
            }
            let key = match self.parse_string() {
                Ok(Value::String(key)) => key,
                _ => return Err(ParseError::Partial(self.mark_partial("Expected string key"))),
            };
            self.skip_blank();
            if self.index >= self.length || self.chars[self.index] != ':' {
                return Err(ParseError::Partial(self.mark_partial("Expected ':'")));
            }
            self.index += 1; // skip colon
            let value = match self.parse_any() {
                Ok(value) => value,
                Err(_) => return Ok(Value::Map(entries)), // partial object allowed
            };
            entries.push((key, value));
            self.skip_blank();
            if self.index < self.length && self.chars[self.index] == ',' {
                self.index += 1; // skip comma
            }
        }
    }

    fn parse_array(&mut self) -> Result<Value, ParseError> {
        self.index += 1; // skip initial bracket
        let mut items = Vec::new();
        loop {
            if self.index < self.length && self.chars[self.index] == ']' {
                self.index += 1;
                return Ok(Value::Array(items));
            }
            match self.parse_any() {
                Ok(value) => items.push(value),
                Err(_) => return Ok(Value::Array(items)), // partial array allowed
            }
            self.skip_blank();
            if self.index < self.length && self.chars[self.index] == ',' {
                self.index += 1; // skip comma
            }
        }
    }

    fn parse_number(&mut self) -> Result<Value, ParseError> {
        let start = self.index;
        if self.chars[self.index] == '-' {
            self.index += 1;
        }
        while self.index < self.length && !",]}".contains(self.chars[self.index]) {
            self.index += 1;
        }
        let slice: String = self.chars[start..self.index].iter().collect();
        if slice == "-" {
            return Err(ParseError::Partial(self.mark_partial("Not sure what '-' is")));
        }
        match parse_json_number(&slice) {
            Ok(number) => Ok(number),
            Err(_) => {
                // Fallback: cut at the last 'e' (partial exponent).
                if let Some(last_e) = slice.rfind(['e', 'E']) {
                    if let Ok(number) = parse_json_number(&slice[..last_e]) {
                        return Ok(number);
                    }
                }
                Err(ParseError::Malformed(self.malformed("Invalid number")))
            }
        }
    }

    fn skip_blank(&mut self) {
        while self.index < self.length && " \n\r\t".contains(self.chars[self.index]) {
            self.index += 1;
        }
    }
}

fn parse_json_strict(json: &str) -> Result<Value, String> {
    let mut parser = StrictJsonParser {
        chars: json.chars().collect(),
        index: 0,
    };
    let value = parser.parse_any().map_err(|_| "parse failed".to_string())?;
    parser.skip_blank();
    if parser.index != parser.chars.len() {
        return Err("trailing data".to_string());
    }
    Ok(value)
}

fn parse_json_number(text: &str) -> Result<Value, String> {
    let number: f64 = text.parse().map_err(|_| "invalid number".to_string())?;
    Ok(Value::Number(number))
}

#[derive(Debug)]
pub struct PartialJsonError(pub String);

#[derive(Debug)]
pub struct MalformedJsonError(pub String);

#[allow(dead_code)]
enum ParseError {
    Partial(PartialJsonError),
    Malformed(MalformedJsonError),
}

/// Parses a (possibly incomplete) JSON string with `Allow.ALL` semantics.
pub fn parse_partial_json(json: &str) -> Result<Value, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Err("empty input".to_string());
    }
    let mut parser = PartialParser {
        chars: trimmed.chars().collect(),
        index: 0,
        length: trimmed.chars().count(),
    };
    parser.parse_any().map_err(|_| "parse failed".to_string())
}

/// Attempts to parse potentially incomplete JSON during streaming. Always
/// returns a valid object, even if the JSON is incomplete.
pub fn parse_streaming_json(partial_json: Option<&str>) -> Value {
    let Some(partial_json) = partial_json else {
        return Value::Map(Vec::new());
    };
    if partial_json.trim().is_empty() {
        return Value::Map(Vec::new());
    }

    if let Ok(value) = parse_json_with_repair::<Value>(partial_json) {
        return value;
    }
    if let Ok(result) = parse_partial_json(partial_json) {
        return result;
    }
    if let Ok(result) = parse_partial_json(&repair_json(partial_json)) {
        return result;
    }
    Value::Map(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_json_escapes_raw_control_characters() {
        assert_eq!(repair_json("\"a\nb\""), "\"a\\nb\"");
        assert_eq!(repair_json("\"tab\there\""), "\"tab\\there\"");
        assert_eq!(repair_json("\"a\u{0001}b\""), "\"a\\u0001b\"");
    }

    #[test]
    fn repair_json_doubles_invalid_escapes() {
        assert_eq!(repair_json(r#""a\qb""#), r#""a\\qb""#);
        // Valid escapes are kept.
        assert_eq!(repair_json(r#""a\nb""#), r#""a\nb""#);
        assert_eq!(repair_json(r#""\u0041""#), r#""\u0041""#);
        // Trailing lone backslash.
        assert_eq!(repair_json(r#""a\"#), r#""a\\"#);
    }

    #[test]
    fn parse_json_with_repair_recovers() {
        let value = parse_json_with_repair::<Value>("{\"a\": \"x\n\"}").unwrap();
        assert_eq!(value, Value::Map(vec![("a".to_string(), Value::String("x\n".to_string()))]));
    }

    #[test]
    fn parse_streaming_json_handles_partial_input() {
        assert_eq!(parse_streaming_json(None), Value::Map(Vec::new()));
        assert_eq!(parse_streaming_json(Some("  ")), Value::Map(Vec::new()));
        // Complete JSON passes through.
        let value = parse_streaming_json(Some("{\"a\": 1}"));
        assert_eq!(value, Value::Map(vec![("a".to_string(), Value::Number(1.0))]));
        // Partial object returns what was parsed.
        let value = parse_streaming_json(Some("{\"a\": 1, \"b\": [1, 2"));
        assert_eq!(
            value,
            Value::Map(vec![
                ("a".to_string(), Value::Number(1.0)),
                ("b".to_string(), Value::Array(vec![Value::Number(1.0), Value::Number(2.0)])),
            ])
        );
        // Partial string.
        let value = parse_streaming_json(Some("{\"a\": \"unterminated"));
        assert_eq!(value, Value::Map(vec![("a".to_string(), Value::String("unterminated".to_string()))]));
        // Garbage falls back to an empty object.
        assert_eq!(parse_streaming_json(Some("###")), Value::Map(Vec::new()));
    }

    #[test]
    fn parse_streaming_json_handles_repairable_partial() {
        let value = parse_streaming_json(Some("{\"a\": \"x\n\"}"));
        assert_eq!(value, Value::Map(vec![("a".to_string(), Value::String("x\n".to_string()))]));
    }
}
