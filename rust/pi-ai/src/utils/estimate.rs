//! Context token estimation, port of `packages/ai/src/utils/estimate.ts`.

use pi_protocol::Value;

use crate::types::{
    Content, ConstrainedSampling, Context, JsonSchemaAdditional, JsonSchemaObject, JsonSchemaValue, Message,
    StopReason, Tool, Usage,
};

pub const CHARS_PER_TOKEN: f64 = 4.0;
pub const ESTIMATED_IMAGE_CHARS: f64 = 4800.0;

#[derive(Clone, Debug, PartialEq)]
pub struct ContextUsageEstimate {
    /// Estimated total context tokens.
    pub tokens: f64,
    /// Tokens reported by the most recent applicable assistant usage block.
    pub usage_tokens: f64,
    /// Estimated tokens after the most recent applicable assistant usage block.
    pub trailing_tokens: f64,
    /// Index of the applicable message that provided usage, or None.
    pub last_usage_index: Option<usize>,
}

pub fn calculate_context_tokens(usage: &Usage) -> f64 {
    let total = usage.total_tokens;
    if total != 0.0 {
        total
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    }
}

/// Mirrors `safeJsonStringify`: serializes a value like `JSON.stringify`.
fn safe_json_stringify(value: &Value) -> String {
    json_stringify(value)
}

fn json_stringify(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Number(n) => js_number_string(*n),
        Value::String(s) => json_quote(s),
        Value::Bytes(_) => "{}".to_string(),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(json_stringify).collect();
            format!("[{}]", inner.join(","))
        }
        Value::Map(entries) => {
            let inner: Vec<String> = entries
                .iter()
                .map(|(key, value)| format!("{}:{}", json_quote(key), json_stringify(value)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
    }
}

fn json_quote(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 2);
    result.push('"');
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
    result.push('"');
    result
}

/// Mirrors JS `String(number)` as produced by JSON.stringify: integers below
/// 1e21 print without an exponent; other magnitudes use `e` notation.
fn js_number_string(n: f64) -> String {
    if n == n.trunc() && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        let abs = n.abs();
        if abs != 0.0 && (abs >= 1e21 || abs < 1e-6) {
            let exp = format!("{:e}", n);
            exp.replace('e', "e+").replace("e+-", "e-")
        } else {
            format!("{}", n)
        }
    }
}

/// Serializes a tool like `JSON.stringify` would (all declared fields).
fn tool_to_value(tool: &Tool) -> Value {
    let mut entries = vec![
        ("name".to_string(), Value::String(tool.name.clone())),
        ("description".to_string(), Value::String(tool.description.clone())),
        ("parameters".to_string(), schema_to_value(&tool.parameters)),
    ];
    match &tool.constrained_sampling {
        None => {}
        Some(ConstrainedSampling::Disabled) => entries.push(("constrainedSampling".to_string(), Value::Bool(false))),
        Some(ConstrainedSampling::Config(config)) => entries.push((
            "constrainedSampling".to_string(),
            constrained_sampling_to_value(config),
        )),
    }
    Value::Map(entries)
}

fn constrained_sampling_to_value(config: &crate::types::ConstrainedSamplingConfig) -> Value {
    match config {
        crate::types::ConstrainedSamplingConfig::JsonSchema { strict } => Value::Map(vec![
            ("type".to_string(), Value::String("json_schema".to_string())),
            ("strict".to_string(), Value::String(strict.clone())),
        ]),
        crate::types::ConstrainedSamplingConfig::Grammar { variants } => Value::Map(vec![
            ("type".to_string(), Value::String("grammar".to_string())),
            (
                "variants".to_string(),
                Value::Map(
                    variants
                        .iter()
                        .map(|(format, grammar)| (format.clone(), Value::String(grammar.clone())))
                        .collect(),
                ),
            ),
        ]),
    }
}

/// Serializes a JSON schema object like `JSON.stringify` of the TypeBox
/// object: only present fields, `type` as string or array.
fn schema_to_value(schema: &JsonSchemaObject) -> Value {
    let mut entries = Vec::new();
    if let Some(type_) = &schema.type_ {
        entries.push((
            "type".to_string(),
            if type_.len() == 1 {
                Value::String(type_[0].clone())
            } else {
                Value::Array(type_.iter().map(|t| Value::String(t.clone())).collect())
            },
        ));
    }
    if let Some(properties) = &schema.properties {
        entries.push((
            "properties".to_string(),
            Value::Map(
                properties
                    .iter()
                    .map(|(key, sub)| (key.clone(), schema_to_value(sub)))
                    .collect(),
            ),
        ));
    }
    if let Some(items) = &schema.items {
        entries.push((
            "items".to_string(),
            match &**items {
                JsonSchemaValue::Schema(sub) => schema_to_value(sub),
                JsonSchemaValue::Schemas(list) => {
                    Value::Array(list.iter().map(schema_to_value).collect())
                }
            },
        ));
    }
    if let Some(additional) = &schema.additional_properties {
        entries.push((
            "additionalProperties".to_string(),
            match additional {
                JsonSchemaAdditional::Bool(b) => Value::Bool(*b),
                JsonSchemaAdditional::Schema(sub) => schema_to_value(sub),
            },
        ));
    }
    if let Some(all_of) = &schema.all_of {
        entries.push(("allOf".to_string(), Value::Array(all_of.iter().map(schema_to_value).collect())));
    }
    if let Some(any_of) = &schema.any_of {
        entries.push(("anyOf".to_string(), Value::Array(any_of.iter().map(schema_to_value).collect())));
    }
    if let Some(one_of) = &schema.one_of {
        entries.push(("oneOf".to_string(), Value::Array(one_of.iter().map(schema_to_value).collect())));
    }
    if let Some(enum_values) = &schema.enum_values {
        entries.push(("enum".to_string(), Value::Array(enum_values.clone())));
    }
    if let Some(default) = &schema.default {
        entries.push(("default".to_string(), default.clone()));
    }
    Value::Map(entries)
}

fn estimate_text_and_image_content_chars(content: &[Content]) -> f64 {
    let mut chars = 0.0;
    for block in content {
        match block {
            Content::Text(text) => chars += text.text.chars().count() as f64,
            Content::Image(_) => chars += ESTIMATED_IMAGE_CHARS,
            _ => {}
        }
    }
    chars
}

pub fn estimate_text_tokens(text: &str) -> f64 {
    (text.chars().count() as f64 / CHARS_PER_TOKEN).ceil()
}

pub fn estimate_text_and_image_content_tokens(content: &[Content]) -> f64 {
    (estimate_text_and_image_content_chars(content) / CHARS_PER_TOKEN).ceil()
}

pub fn estimate_message_tokens(message: &Message) -> f64 {
    match message {
        Message::User(user) => match &user.content {
            crate::types::UserMessageContent::Text(text) => estimate_text_tokens(text),
            crate::types::UserMessageContent::Blocks(content) => {
                estimate_text_and_image_content_tokens(content)
            }
        },
        Message::ToolResult(tool) => estimate_text_and_image_content_tokens(&tool.content),
        Message::Assistant(assistant) => {
            let mut chars = 0.0;
            for block in &assistant.content {
                match block {
                    Content::Text(text) => chars += text.text.chars().count() as f64,
                    Content::Thinking(thinking) => chars += thinking.thinking.chars().count() as f64,
                    Content::ToolCall(tool_call) => {
                        chars += tool_call.name.chars().count() as f64;
                        chars += safe_json_stringify(&tool_call.arguments).chars().count() as f64;
                    }
                    Content::Image(_) => {}
                }
            }
            (chars / CHARS_PER_TOKEN).ceil()
        }
    }
}

fn message_timestamp(message: &Message) -> f64 {
    match message {
        Message::User(user) => user.timestamp,
        Message::Assistant(assistant) => assistant.timestamp,
        Message::ToolResult(tool) => tool.timestamp,
    }
}

fn get_last_assistant_usage_info(messages: &[Message]) -> Option<(Usage, usize)> {
    let mut latest_prefix_timestamp = f64::NEG_INFINITY;
    let mut usage_info: Option<(Usage, usize)> = None;

    for (i, message) in messages.iter().enumerate() {
        if let Message::Assistant(assistant) = message {
            let usage_applies_to_prefix = assistant.timestamp >= latest_prefix_timestamp;
            if usage_applies_to_prefix
                && assistant.stop_reason != StopReason::Aborted
                && assistant.stop_reason != StopReason::Error
                && calculate_context_tokens(&assistant.usage) > 0.0
            {
                usage_info = Some((assistant.usage.clone(), i));
            }
        }
        latest_prefix_timestamp = latest_prefix_timestamp.max(message_timestamp(message));
    }

    usage_info
}

fn estimate_messages(messages: &[Message]) -> ContextUsageEstimate {
    if let Some((usage, index)) = get_last_assistant_usage_info(messages) {
        let usage_tokens = calculate_context_tokens(&usage);
        let mut trailing_tokens = 0.0;
        for message in &messages[index + 1..] {
            trailing_tokens += estimate_message_tokens(message);
        }
        return ContextUsageEstimate {
            tokens: usage_tokens + trailing_tokens,
            usage_tokens,
            trailing_tokens,
            last_usage_index: Some(index),
        };
    }

    let mut tokens = 0.0;
    for message in messages {
        tokens += estimate_message_tokens(message);
    }
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0.0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}

fn estimate_tools_tokens(tools: &[&Tool]) -> f64 {
    if tools.is_empty() {
        return 0.0;
    }
    let serialized: Vec<Value> = tools.iter().map(|tool| tool_to_value(tool)).collect();
    estimate_text_tokens(&safe_json_stringify(&Value::Array(serialized)))
}

pub enum ContextOrMessages<'a> {
    Context(&'a Context),
    Messages(&'a [Message]),
}

pub fn estimate_context_tokens(context: ContextOrMessages<'_>) -> ContextUsageEstimate {
    let messages = match context {
        ContextOrMessages::Messages(messages) => return estimate_messages(messages),
        ContextOrMessages::Context(context) => &context.messages,
    };

    let estimate = estimate_messages(messages);
    if let Some(last_usage_index) = estimate.last_usage_index {
        let mut added_names = std::collections::HashSet::new();
        for message in &messages[last_usage_index + 1..] {
            if let Message::ToolResult(tool) = message {
                for name in tool.added_tool_names.iter().flatten() {
                    added_names.insert(name.clone());
                }
            }
        }
        let filtered: Vec<&Tool> = match context {
            ContextOrMessages::Context(context) => context
                .tools
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .filter(|tool| added_names.contains(&tool.name))
                .collect(),
            ContextOrMessages::Messages(_) => Vec::new(),
        };
        let added_tool_tokens = estimate_tools_tokens(&filtered);
        return ContextUsageEstimate {
            tokens: estimate.tokens + added_tool_tokens,
            usage_tokens: estimate.usage_tokens,
            trailing_tokens: estimate.trailing_tokens + added_tool_tokens,
            last_usage_index: estimate.last_usage_index,
        };
    }

    let prefix_tokens = match context {
        ContextOrMessages::Context(context) => {
            let system = context
                .system_prompt
                .as_deref()
                .map(estimate_text_tokens)
                .unwrap_or(0.0);
            let tools: Vec<&Tool> = context.tools.as_deref().unwrap_or(&[]).iter().collect();
            system + estimate_tools_tokens(&tools)
        }
        ContextOrMessages::Messages(_) => 0.0,
    };

    ContextUsageEstimate {
        tokens: estimate.tokens + prefix_tokens,
        usage_tokens: estimate.usage_tokens,
        trailing_tokens: estimate.trailing_tokens + prefix_tokens,
        last_usage_index: estimate.last_usage_index,
    }
}
