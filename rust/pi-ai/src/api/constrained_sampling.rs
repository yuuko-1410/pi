//! Constrained sampling helpers, port of
//! `packages/ai/src/api/constrained-sampling.ts`.

use crate::types::{ConstrainedSampling, ConstrainedSamplingConfig, Tool};

#[derive(Clone, Debug, PartialEq)]
pub struct GrammarConstrainedSampling {
    pub format: String,
    pub definition: String,
    pub input_property: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GrammarToolInputJsonBuffer {
    pub input: String,
    pub started: bool,
    pub closed: bool,
}

/// Mirrors `getGrammarToolInput`: reads the input property as a string.
pub fn get_grammar_tool_input(
    tool_name: &str,
    arguments: &pi_protocol::Value,
    input_property: &str,
) -> Result<String, String> {
    match arguments {
        pi_protocol::Value::Map(entries) => {
            let value = entries
                .iter()
                .find(|(key, _)| key == input_property)
                .map(|(_, value)| value);
            match value {
                Some(pi_protocol::Value::String(input)) => Ok(input.clone()),
                _ => Err(format!(
                    "Grammar tool call \"{tool_name}\" requires argument \"{input_property}\" to be a string."
                )),
            }
        }
        _ => Err(format!(
            "Grammar tool call \"{tool_name}\" requires argument \"{input_property}\" to be a string."
        )),
    }
}

/// Mirrors `appendGrammarToolInputJsonDelta`: returns the delta to emit, or
/// None when there is nothing to emit yet.
pub fn append_grammar_tool_input_json_delta(
    buffer: &mut GrammarToolInputJsonBuffer,
    input_property: &str,
    next_input: &str,
    close: bool,
) -> Result<Option<String>, String> {
    if buffer.closed {
        if close && next_input == buffer.input {
            return Ok(None);
        }
        return Err(format!(
            "grammar tool input for property \"{input_property}\" changed after it was closed"
        ));
    }
    if !next_input.starts_with(&buffer.input) {
        return Err(format!(
            "grammar tool input for property \"{input_property}\" changed non-monotonically"
        ));
    }

    let input_delta = &next_input[buffer.input.len()..];
    if !close && input_delta.is_empty() {
        return Ok(None);
    }

    let mut delta = String::new();
    if !buffer.started {
        delta.push_str(&format!("{{\"{input_property}\":\""));
        buffer.started = true;
    }
    delta.push_str(&json_escape_inside_string(input_delta));
    buffer.input = next_input.to_string();

    if close {
        delta.push_str("\"}");
        buffer.closed = true;
    }
    Ok(Some(delta))
}

/// JSON.stringify(s).slice(1, -1): escapes the string as JSON and strips the
/// surrounding quotes.
fn json_escape_inside_string(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            '\u{08}' => result.push_str("\\b"),
            '\u{0c}' => result.push_str("\\f"),
            c if (c as u32) < 0x20 => result.push_str(&format!("\\u{:04x}", c as u32)),
            c => result.push(c),
        }
    }
    result
}

fn infer_grammar_input_property(tool: &Tool) -> Result<String, String> {
    let schema = &tool.parameters;
    let Some(type_) = &schema.type_ else {
        return Err("grammar constrained sampling requires an object parameter schema".to_string());
    };
    if type_.len() != 1 || type_[0] != "object" {
        return Err("grammar constrained sampling requires an object parameter schema".to_string());
    }
    let Some(required) = &schema.required else {
        return Err("grammar constrained sampling requires exactly one required string property".to_string());
    };
    if required.len() != 1 {
        return Err("grammar constrained sampling requires exactly one required string property".to_string());
    }
    let input_property = required[0].clone();

    let Some(properties) = &schema.properties else {
        return Err(format!(
            "grammar constrained sampling requires a properties entry for {input_property}"
        ));
    };
    let Some((_, property_schema)) = properties.iter().find(|(key, _)| key == &input_property) else {
        return Err(format!(
            "grammar constrained sampling requires a properties entry for {input_property}"
        ));
    };
    let Some(property_type) = &property_schema.type_ else {
        return Err(format!(
            "grammar constrained sampling property {input_property} must have type string"
        ));
    };
    if property_type.len() != 1 || property_type[0] != "string" {
        return Err(format!(
            "grammar constrained sampling property {input_property} must have type string"
        ));
    }
    Ok(input_property)
}

/// Mirrors `resolveJsonSchemaStrictSampling`.
pub fn resolve_json_schema_strict_sampling(
    tool: &Tool,
    supports_strict_mode: bool,
) -> Result<Option<bool>, String> {
    let Some(ConstrainedSampling::Config(config)) = &tool.constrained_sampling else {
        return Ok(None);
    };
    let ConstrainedSamplingConfig::JsonSchema { strict } = config else {
        return Ok(None);
    };

    if supports_strict_mode {
        return Ok(Some(true));
    }
    if strict == "require" {
        return Err(format!(
            "Tool \"{}\" requires JSON-schema constrained sampling, but strict tools are unsupported.",
            tool.name
        ));
    }
    Ok(None)
}

/// Mirrors `resolveGrammarConstrainedSampling`.
pub fn resolve_grammar_constrained_sampling(
    tool: &Tool,
    supports_openai_grammar_tools: bool,
) -> Result<Option<GrammarConstrainedSampling>, String> {
    let Some(ConstrainedSampling::Config(config)) = &tool.constrained_sampling else {
        return Ok(None);
    };
    let ConstrainedSamplingConfig::Grammar { variants } = config else {
        return Ok(None);
    };

    if !supports_openai_grammar_tools {
        return Ok(None);
    }

    let lark_definition = variants
        .iter()
        .find(|(format, _)| format == "openai_lark")
        .map(|(_, definition)| definition.clone());
    let regex_definition = variants
        .iter()
        .find(|(format, _)| format == "openai_regex")
        .map(|(_, definition)| definition.clone());
    let has_lark_definition = lark_definition
        .as_ref()
        .is_some_and(|definition| !definition.trim().is_empty());
    let has_regex_definition = regex_definition
        .as_ref()
        .is_some_and(|definition| !definition.trim().is_empty());
    if !has_lark_definition && !has_regex_definition {
        return Err(format!(
            "Tool \"{}\" cannot use grammar constrained sampling: no supported grammar variant was provided.",
            tool.name
        ));
    }

    let (format, definition) = if has_lark_definition {
        ("lark".to_string(), lark_definition.expect("checked above"))
    } else {
        ("regex".to_string(), regex_definition.expect("checked above"))
    };
    match infer_grammar_input_property(tool) {
        Ok(input_property) => Ok(Some(GrammarConstrainedSampling {
            format,
            definition,
            input_property,
        })),
        Err(message) => Err(format!(
            "Tool \"{}\" cannot use grammar constrained sampling: {message}.",
            tool.name
        )),
    }
}

/// Mirrors `createGrammarToolInputProperties` (insertion-ordered Vec).
pub fn create_grammar_tool_input_properties(
    tools: Option<&[Tool]>,
    supports_openai_grammar_tools: bool,
) -> Vec<(String, String)> {
    let mut properties = Vec::new();
    for tool in tools.unwrap_or(&[]) {
        if let Ok(Some(grammar)) = resolve_grammar_constrained_sampling(tool, supports_openai_grammar_tools) {
            properties.push((tool.name.clone(), grammar.input_property));
        }
    }
    properties
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::Value;

    #[test]
    fn appends_grammar_tool_input_deltas() {
        let mut buffer = GrammarToolInputJsonBuffer::default();
        let delta = append_grammar_tool_input_json_delta(&mut buffer, "input", "hel", false).unwrap();
        assert_eq!(delta, Some("{\"input\":\"hel".to_string()));
        let delta = append_grammar_tool_input_json_delta(&mut buffer, "input", "hello", true).unwrap();
        assert_eq!(delta, Some("lo\"}".to_string()));
        assert!(buffer.closed);
        // Closed: same input with close is a no-op.
        let delta = append_grammar_tool_input_json_delta(&mut buffer, "input", "hello", true).unwrap();
        assert_eq!(delta, None);
        // Non-monotonic change errors.
        let mut buffer2 = GrammarToolInputJsonBuffer::default();
        append_grammar_tool_input_json_delta(&mut buffer2, "input", "abc", false).unwrap();
        assert!(append_grammar_tool_input_json_delta(&mut buffer2, "input", "abd", false).is_err());
    }

    #[test]
    fn grammar_input_requires_string() {
        let ok = get_grammar_tool_input(
            "tool",
            &Value::Map(vec![("input".to_string(), Value::String("x".to_string()))]),
            "input",
        );
        assert_eq!(ok, Ok("x".to_string()));
        let err = get_grammar_tool_input("tool", &Value::Map(vec![]), "input").unwrap_err();
        assert!(err.contains("requires argument"), "{err}");
    }
}
