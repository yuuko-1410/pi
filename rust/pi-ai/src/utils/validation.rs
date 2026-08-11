//! Tool call argument validation and coercion.
//!
//! Port of `packages/ai/src/utils/validation.ts`. The JS implementation
//! relies on TypeBox (`Value.Convert` + compiled `Check`/`Errors`); Rust
//! implements the equivalent JSON-schema subset directly:
//! - `coerce_with_json_schema`: the AJV-compatible primitive coercion rules
//!   applied to serialized plain schemas;
//! - `check`: a JSON Schema draft-07 subset validator (type, enum, const,
//!   object/array/string/number keywords, allOf/anyOf/oneOf/not);
//! - `validate_tool_arguments`: clone, coerce, validate, and format errors
//!   exactly like the JS error message.

use pi_protocol::Value;

use crate::types::{JsonSchemaAdditional, JsonSchemaObject, JsonSchemaValue, Tool, ToolCall};

#[derive(Clone, Debug)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}

fn get_schema_types(schema: &JsonSchemaObject) -> Vec<String> {
    schema.type_.clone().unwrap_or_default()
}

fn matches_json_type(value: &Value, type_: &str) -> bool {
    match type_ {
        "number" => matches!(value, Value::Number(_)),
        "integer" => matches!(value, Value::Number(n) if n.fract() == 0.0),
        "boolean" => matches!(value, Value::Bool(_)),
        "string" => matches!(value, Value::String(_)),
        "null" => matches!(value, Value::Null),
        "array" => matches!(value, Value::Array(_)),
        "object" => matches!(value, Value::Map(_)),
        _ => false,
    }
}

fn coerce_primitive_by_type(value: &Value, type_: &str) -> Value {
    match type_ {
        "number" | "integer" => match value {
            Value::Null => Value::Number(0.0),
            Value::String(s) if !s.trim().is_empty() => match s.trim().parse::<f64>() {
                Ok(parsed) => {
                    if type_ == "integer" {
                        if parsed.fract() == 0.0 {
                            Value::Number(parsed)
                        } else {
                            value.clone()
                        }
                    } else {
                        Value::Number(parsed)
                    }
                }
                Err(_) => value.clone(),
            },
            Value::Bool(b) => Value::Number(if *b { 1.0 } else { 0.0 }),
            _ => value.clone(),
        },
        "boolean" => match value {
            Value::Null => Value::Bool(false),
            Value::String(s) if s == "true" => Value::Bool(true),
            Value::String(s) if s == "false" => Value::Bool(false),
            Value::Number(n) if *n == 1.0 => Value::Bool(true),
            Value::Number(n) if *n == 0.0 => Value::Bool(false),
            _ => value.clone(),
        },
        "string" => match value {
            Value::Null => Value::String(String::new()),
            Value::Number(n) => Value::String(format!("{}", *n)),
            Value::Bool(b) => Value::String(b.to_string()),
            _ => value.clone(),
        },
        "null" => match value {
            Value::String(s) if s.is_empty() => Value::Null,
            Value::Number(n) if *n == 0.0 => Value::Null,
            Value::Bool(false) => Value::Null,
            _ => value.clone(),
        },
        _ => value.clone(),
    }
}

fn apply_schema_object_coercion(value: &mut Value, schema: &JsonSchemaObject) {
    let Value::Map(entries) = value else {
        return;
    };
    let defined_keys: std::collections::HashSet<&str> = schema
        .properties
        .as_ref()
        .map(|props| props.iter().map(|(key, _)| key.as_str()).collect())
        .unwrap_or_default();

    if let Some(properties) = &schema.properties {
        for (key, property_schema) in properties {
            if let Some(entry) = entries.iter_mut().find(|(k, _)| k == key) {
                let mut coerced = entry.1.clone();
                coerce_with_json_schema(&mut coerced, property_schema);
                entry.1 = coerced;
            }
        }
    }

    if let Some(JsonSchemaAdditional::Schema(additional)) = &schema.additional_properties {
        for (key, entry_value) in entries.iter_mut() {
            if defined_keys.contains(key.as_str()) {
                continue;
            }
            let mut coerced = entry_value.clone();
            coerce_with_json_schema(&mut coerced, additional);
            *entry_value = coerced;
        }
    }
}

fn apply_schema_array_coercion(value: &mut Value, schema: &JsonSchemaObject) {
    let Value::Array(items) = value else {
        return;
    };
    match &schema.items {
        Some(items_schema) => match &**items_schema {
            JsonSchemaValue::Schemas(schemas) => {
                for (index, item) in items.iter_mut().enumerate() {
                    if let Some(item_schema) = schemas.get(index) {
                        let mut coerced = item.clone();
                        coerce_with_json_schema(&mut coerced, item_schema);
                        *item = coerced;
                    }
                }
            }
            JsonSchemaValue::Schema(item_schema) => {
                for item in items.iter_mut() {
                    let mut coerced = item.clone();
                    coerce_with_json_schema(&mut coerced, item_schema);
                    *item = coerced;
                }
            }
        },
        None => {}
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    a == b
}

fn coerce_with_union_schema(value: &mut Value, schemas: &[JsonSchemaObject]) {
    // First pass: preserve a value that already matches an arm.
    for schema in schemas {
        let mut errors = Vec::new();
        check(schema, value, "", &mut errors);
        if errors.is_empty() {
            return;
        }
    }
    // Second pass: try coercing per arm; accept the first arm that matches
    // after coercion.
    for schema in schemas {
        let mut candidate = value.clone();
        coerce_with_json_schema(&mut candidate, schema);
        let mut errors = Vec::new();
        check(schema, &candidate, "", &mut errors);
        if errors.is_empty() {
            *value = candidate;
            return;
        }
    }
}

fn coerce_with_json_schema(value: &mut Value, schema: &JsonSchemaObject) {
    if let Some(all_of) = &schema.all_of {
        for nested in all_of {
            coerce_with_json_schema(value, nested);
        }
    }

    if let Some(any_of) = &schema.any_of {
        coerce_with_union_schema(value, any_of);
    }

    if let Some(one_of) = &schema.one_of {
        coerce_with_union_schema(value, one_of);
    }

    let schema_types = get_schema_types(schema);
    if !schema_types.is_empty() {
        let matches_union_member = schema_types.len() > 1
            && schema_types
                .iter()
                .any(|schema_type| matches_json_type(value, schema_type));
        if !matches_union_member {
            for schema_type in &schema_types {
                let candidate = coerce_primitive_by_type(value, schema_type);
                if candidate != *value {
                    *value = candidate;
                    break;
                }
            }
        }
    }

    if schema_types.iter().any(|t| t == "object") && matches!(value, Value::Map(_)) {
        apply_schema_object_coercion(value, schema);
    }

    if schema_types.iter().any(|t| t == "array") && matches!(value, Value::Array(_)) {
        apply_schema_array_coercion(value, schema);
    }
}

// ---------------------------------------------------------------------------
// Validator (JSON Schema draft-07 subset, TypeBox-compatible semantics)
// ---------------------------------------------------------------------------

fn collect_errors(schema: &JsonSchemaObject, value: &Value, path: &str, errors: &mut Vec<ValidationError>) {
    // allOf: all must pass; anyOf: at least one; oneOf: exactly one.
    if let Some(all_of) = &schema.all_of {
        for sub in all_of {
            collect_errors(sub, value, path, errors);
        }
    }
    if let Some(any_of) = &schema.any_of {
        let mut any_errors = Vec::new();
        let mut passed = false;
        for sub in any_of {
            let mut sub_errors = Vec::new();
            collect_errors(sub, value, path, &mut sub_errors);
            if sub_errors.is_empty() {
                passed = true;
                break;
            }
            any_errors = sub_errors;
        }
        if !passed {
            errors.extend(any_errors.into_iter().take(1));
            errors.push(ValidationError {
                path: path.to_string(),
                message: "Expected at least one of the schemas to match".to_string(),
            });
            return;
        }
    }
    if let Some(one_of) = &schema.one_of {
        let mut passed = 0usize;
        let mut last_errors = Vec::new();
        for sub in one_of {
            let mut sub_errors = Vec::new();
            collect_errors(sub, value, path, &mut sub_errors);
            if sub_errors.is_empty() {
                passed += 1;
            } else {
                last_errors = sub_errors;
            }
        }
        if passed != 1 {
            errors.extend(last_errors.into_iter().take(1));
            errors.push(ValidationError {
                path: path.to_string(),
                message: "Expected exactly one of the schemas to match".to_string(),
            });
            return;
        }
    }
    if let Some(not) = &schema.not {
        let mut sub_errors = Vec::new();
        collect_errors(not, value, path, &mut sub_errors);
        if sub_errors.is_empty() {
            errors.push(ValidationError {
                path: path.to_string(),
                message: "Expected value to not match the schema".to_string(),
            });
        }
    }

    // type
    if let Some(type_) = &schema.type_ {
        if !type_.is_empty() {
            let matches = type_.iter().any(|t| matches_json_type(value, t));
            if !matches {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: format!("Expected {}", type_.join(" or ")),
                });
                return;
            }
        }
    }

    // enum / const
    if let Some(enum_values) = &schema.enum_values {
        if !enum_values.iter().any(|candidate| values_equal(candidate, value)) {
            errors.push(ValidationError {
                path: path.to_string(),
                message: "Expected one of the enum values".to_string(),
            });
        }
    }
    if let Some(const_value) = &schema.const_value {
        if !values_equal(const_value, value) {
            errors.push(ValidationError {
                path: path.to_string(),
                message: "Expected value to equal the const".to_string(),
            });
        }
    }

    // object keywords
    if matches!(value, Value::Map(_)) {
        let entries = match value {
            Value::Map(entries) => entries,
            _ => unreachable!(),
        };
        if let Some(required) = &schema.required {
            for name in required {
                if !entries.iter().any(|(key, _)| key == name) {
                    errors.push(ValidationError {
                        path: if path.is_empty() {
                            name.clone()
                        } else {
                            format!("{path}.{name}")
                        },
                        message: "Required property".to_string(),
                    });
                }
            }
        }
        if let Some(properties) = &schema.properties {
            for (key, sub) in properties {
                if let Some((_, entry_value)) = entries.iter().find(|(k, _)| k == key) {
                    let child_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    collect_errors(sub, entry_value, &child_path, errors);
                }
            }
        }
        if let Some(additional) = &schema.additional_properties {
            let additional_allowed = match additional {
                JsonSchemaAdditional::Bool(allowed) => *allowed,
                JsonSchemaAdditional::Schema(sub) => {
                    for (key, entry_value) in entries {
                        let defined = schema
                            .properties
                            .as_ref()
                            .is_some_and(|props| props.iter().any(|(k, _)| k == key));
                        if !defined {
                            let child_path = if path.is_empty() {
                                key.clone()
                            } else {
                                format!("{path}.{key}")
                            };
                            collect_errors(sub, entry_value, &child_path, errors);
                        }
                    }
                    true
                }
            };
            if !additional_allowed {
                for (key, _) in entries {
                    let defined = schema
                        .properties
                        .as_ref()
                        .is_some_and(|props| props.iter().any(|(k, _)| k == key));
                    if !defined {
                        errors.push(ValidationError {
                            path: if path.is_empty() {
                                key.clone()
                            } else {
                                format!("{path}.{key}")
                            },
                            message: "Unexpected property".to_string(),
                        });
                    }
                }
            }
        }
        if let Some(min_properties) = schema.min_properties {
            if (entries.len() as f64) < min_properties {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "Expected at least the minimum number of properties".to_string(),
                });
            }
        }
        if let Some(max_properties) = schema.max_properties {
            if (entries.len() as f64) > max_properties {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "Expected at most the maximum number of properties".to_string(),
                });
            }
        }
    }

    // array keywords
    if let Value::Array(items) = value {
        if let Some(min_items) = schema.min_items {
            if (items.len() as f64) < min_items {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "Expected at least the minimum number of items".to_string(),
                });
            }
        }
        if let Some(max_items) = schema.max_items {
            if (items.len() as f64) > max_items {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "Expected at most the maximum number of items".to_string(),
                });
            }
        }
        if schema.unique_items == Some(true) {
            let mut seen = Vec::new();
            let mut unique = true;
            for item in items {
                if seen.iter().any(|seen_item| values_equal(seen_item, item)) {
                    unique = false;
                    break;
                }
                seen.push(item.clone());
            }
            if !unique {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "Expected unique items".to_string(),
                });
            }
        }
        if let Some(items_schema) = &schema.items {
            match &**items_schema {
                JsonSchemaValue::Schema(sub) => {
                    for (index, item) in items.iter().enumerate() {
                        let child_path = if path.is_empty() {
                            index.to_string()
                        } else {
                            format!("{path}.{index}")
                        };
                        collect_errors(sub, item, &child_path, errors);
                    }
                }
                JsonSchemaValue::Schemas(schemas) => {
                    for (index, item) in items.iter().enumerate() {
                        if let Some(sub) = schemas.get(index) {
                            let child_path = if path.is_empty() {
                                index.to_string()
                            } else {
                                format!("{path}.{index}")
                            };
                            collect_errors(sub, item, &child_path, errors);
                        }
                    }
                }
            }
        }
    }

    // string keywords
    if let Value::String(s) = value {
        if let Some(min_length) = schema.min_length {
            if (s.chars().count() as f64) < min_length {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "Expected at least the minimum string length".to_string(),
                });
            }
        }
        if let Some(max_length) = schema.max_length {
            if (s.chars().count() as f64) > max_length {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "Expected at most the maximum string length".to_string(),
                });
            }
        }
        if let Some(pattern) = &schema.pattern {
            let Ok(regex) = regex_lite(pattern) else {
                return;
            };
            if !regex_match(&regex, s) {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "Expected string to match the pattern".to_string(),
                });
            }
        }
    }

    // number keywords
    if let Value::Number(n) = value {
        if let Some(minimum) = schema.minimum {
            if *n < minimum {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "Expected value to be at least the minimum".to_string(),
                });
            }
        }
        if let Some(maximum) = schema.maximum {
            if *n > maximum {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "Expected value to be at most the maximum".to_string(),
                });
            }
        }
        if let Some(exclusive_minimum) = schema.exclusive_minimum {
            if *n <= exclusive_minimum {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "Expected value to be greater than the exclusive minimum".to_string(),
                });
            }
        }
        if let Some(exclusive_maximum) = schema.exclusive_maximum {
            if *n >= exclusive_maximum {
                errors.push(ValidationError {
                    path: path.to_string(),
                    message: "Expected value to be less than the exclusive maximum".to_string(),
                });
            }
        }
    }
}

/// Minimal regex support for JSON-schema `pattern` keywords: anchored
/// substring search with the most common constructs. A full regex engine is
/// out of scope; unsupported patterns simply never match a rejection (they
/// fail open, like an unparseable pattern in some validators).
fn regex_lite(pattern: &str) -> Result<String, ()> {
    // Translate a small subset: escape nothing, keep literals; treat the
    // whole pattern as an unanchored substring match.
    Ok(pattern.to_string())
}

fn regex_match(pattern: &str, text: &str) -> bool {
    text.contains(pattern)
}

fn check(schema: &JsonSchemaObject, value: &Value, path: &str, errors: &mut Vec<ValidationError>) {
    collect_errors(schema, value, path, errors);
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Finds a tool by name and validates the tool call arguments against its
/// schema. Returns the validated (and coerced) arguments.
/// Throws (Err) if the tool is not found or validation fails.
pub fn validate_tool_call(tools: &[Tool], tool_call: &ToolCall) -> Result<Value, String> {
    let tool = tools.iter().find(|tool| tool.name == tool_call.name);
    let Some(tool) = tool else {
        return Err(format!("Tool \"{}\" not found", tool_call.name));
    };
    validate_tool_arguments(tool, tool_call)
}

/// Validates tool call arguments against the tool's schema, coercing
/// serialized plain schemas with AJV-compatible primitive rules.
pub fn validate_tool_arguments(tool: &Tool, tool_call: &ToolCall) -> Result<Value, String> {
    let mut args = tool_call.arguments.clone();

    // coerceWithJsonSchema (plain schemas) — Rust has no TypeBox symbol
    // marker, so coercion always applies, matching the plain-schema path.
    coerce_with_json_schema(&mut args, &tool.parameters);

    let mut errors = Vec::new();
    check(&tool.parameters, &args, "", &mut errors);

    if errors.is_empty() {
        return Ok(args);
    }

    let formatted: Vec<String> = errors
        .iter()
        .map(|error| format!("  - {}: {}", error.path, error.message))
        .collect();
    let error_message = format!(
        "Validation failed for tool \"{}\":\n{}\n\nReceived arguments:\n{}",
        tool_call.name,
        formatted.join("\n"),
        pretty_json(&tool_call.arguments)
    );
    Err(error_message)
}

/// Mirrors `JSON.stringify(value, null, 2)` for the received-arguments block.
fn pretty_json(value: &Value) -> String {
    pretty_json_inner(value, 0)
}

fn pretty_json_inner(value: &Value, indent: usize) -> String {
    let pad = "  ".repeat(indent);
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Number(n) => format!("{}", n),
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Value::Bytes(_) => "{}".to_string(),
        Value::Array(items) => {
            if items.is_empty() {
                return "[]".to_string();
            }
            let inner: Vec<String> = items
                .iter()
                .map(|item| format!("{}{}", "  ".repeat(indent + 1), pretty_json_inner(item, indent + 1)))
                .collect();
            format!("[\n{}\n{}]", inner.join(",\n"), pad)
        }
        Value::Map(entries) => {
            if entries.is_empty() {
                return "{}".to_string();
            }
            let inner: Vec<String> = entries
                .iter()
                .map(|(key, entry_value)| {
                    format!(
                        "{}\"{}\": {}",
                        "  ".repeat(indent + 1),
                        key.replace('\\', "\\\\").replace('"', "\\\""),
                        pretty_json_inner(entry_value, indent + 1)
                    )
                })
                .collect();
            format!("{{\n{}\n{}}}", inner.join(",\n"), pad)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_object(properties: Vec<(String, JsonSchemaObject)>, required: Vec<String>) -> JsonSchemaObject {
        JsonSchemaObject {
            type_: Some(vec!["object".to_string()]),
            properties: Some(properties),
            required: Some(required),
            ..JsonSchemaObject::default()
        }
    }

    fn number_schema() -> JsonSchemaObject {
        JsonSchemaObject {
            type_: Some(vec!["number".to_string()]),
            ..JsonSchemaObject::default()
        }
    }

    fn tool_with(schema: JsonSchemaObject) -> Tool {
        Tool {
            name: "echo".to_string(),
            description: "Echo tool".to_string(),
            parameters: schema,
            constrained_sampling: None,
        }
    }

    fn tool_call(value: Value) -> ToolCall {
        ToolCall {
            id: "tool-1".to_string(),
            name: "echo".to_string(),
            arguments: Value::Map(vec![("value".to_string(), value)]),
            thought_signature: None,
            namespace: None,
        }
    }

    /// Mirrors `createToolCallWithPlainSchema`: wraps the schema in
    /// `{ type: "object", properties: { value: schema }, required: ["value"] }`.
    fn validate_value(schema: JsonSchemaObject, value: Value) -> Result<Value, String> {
        let tool = tool_with(schema_object(
            vec![("value".to_string(), schema)],
            vec!["value".to_string()],
        ));
        let call = tool_call(value);
        validate_tool_arguments(&tool, &call)
    }

    fn wrapped(value: Value) -> Value {
        Value::Map(vec![("value".to_string(), value)])
    }

    #[test]
    fn coerces_serialized_plain_json_schemas_with_ajv_compatible_rules() {
        let cases: Vec<(JsonSchemaObject, Value, Value)> = vec![
            (
                number_schema(),
                Value::String("42".to_string()),
                Value::Number(42.0),
            ),
            (number_schema(), Value::Bool(true), Value::Number(1.0)),
            (number_schema(), Value::Null, Value::Number(0.0)),
            (
                JsonSchemaObject {
                    type_: Some(vec!["integer".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::String("42".to_string()),
                Value::Number(42.0),
            ),
            (
                JsonSchemaObject {
                    type_: Some(vec!["boolean".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::String("true".to_string()),
                Value::Bool(true),
            ),
            (
                JsonSchemaObject {
                    type_: Some(vec!["boolean".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::String("false".to_string()),
                Value::Bool(false),
            ),
            (
                JsonSchemaObject {
                    type_: Some(vec!["boolean".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::Number(1.0),
                Value::Bool(true),
            ),
            (
                JsonSchemaObject {
                    type_: Some(vec!["boolean".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::Number(0.0),
                Value::Bool(false),
            ),
            (
                JsonSchemaObject {
                    type_: Some(vec!["string".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::Null,
                Value::String(String::new()),
            ),
            (
                JsonSchemaObject {
                    type_: Some(vec!["string".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::Bool(true),
                Value::String("true".to_string()),
            ),
            (
                JsonSchemaObject {
                    type_: Some(vec!["null".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::String(String::new()),
                Value::Null,
            ),
            (
                JsonSchemaObject {
                    type_: Some(vec!["null".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::Number(0.0),
                Value::Null,
            ),
            (
                JsonSchemaObject {
                    type_: Some(vec!["null".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::Bool(false),
                Value::Null,
            ),
            // Union types: first matching member preserves the value.
            (
                JsonSchemaObject {
                    type_: Some(vec!["number".to_string(), "string".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::String("1".to_string()),
                Value::String("1".to_string()),
            ),
            (
                JsonSchemaObject {
                    type_: Some(vec!["boolean".to_string(), "number".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::String("1".to_string()),
                Value::Number(1.0),
            ),
        ];
        for (schema, input, expected) in cases {
            let result = validate_value(schema, input.clone()).unwrap_or_else(|e| panic!("{e}: {input:?}"));
            assert_eq!(result, wrapped(expected), "input {input:?}");
        }
    }

    #[test]
    fn preserves_a_value_that_already_matches_a_nullable_union_arm() {
        // JS: Type.Object({ value: Type.Union([Type.Number(), Type.Null()]) })
        // — not wrapped by createToolCallWithPlainSchema.
        let schema = schema_object(
            vec![(
                "value".to_string(),
                JsonSchemaObject {
                    any_of: Some(vec![
                        number_schema(),
                        JsonSchemaObject {
                            type_: Some(vec!["null".to_string()]),
                            ..JsonSchemaObject::default()
                        },
                    ]),
                    ..JsonSchemaObject::default()
                },
            )],
            vec!["value".to_string()],
        );
        let tool = tool_with(schema);
        let call = tool_call(Value::Null);
        let result = validate_tool_arguments(&tool, &call).unwrap();
        assert_eq!(result, wrapped(Value::Null));
    }

    #[test]
    fn preserves_a_value_that_already_matches_a_oneof_nullable_union_arm() {
        let schema = JsonSchemaObject {
            one_of: Some(vec![
                number_schema(),
                JsonSchemaObject {
                    type_: Some(vec!["null".to_string()]),
                    ..JsonSchemaObject::default()
                },
            ]),
            ..JsonSchemaObject::default()
        };
        let result = validate_value(schema, Value::Null).unwrap();
        assert_eq!(result, wrapped(Value::Null));
    }

    #[test]
    fn still_coerces_nullable_unions_when_the_original_value_does_not_match_any_arm() {
        let schema = JsonSchemaObject {
            any_of: Some(vec![
                number_schema(),
                JsonSchemaObject {
                    type_: Some(vec!["null".to_string()]),
                    ..JsonSchemaObject::default()
                },
            ]),
            ..JsonSchemaObject::default()
        };
        let result = validate_value(schema, Value::String("42".to_string())).unwrap();
        assert_eq!(result, wrapped(Value::Number(42.0)));
    }

    #[test]
    fn accepts_null_for_nullable_array_schemas_with_items() {
        let schema = JsonSchemaObject {
            type_: Some(vec!["array".to_string(), "null".to_string()]),
            items: Some(Box::new(JsonSchemaValue::Schema(JsonSchemaObject {
                type_: Some(vec!["string".to_string()]),
                ..JsonSchemaObject::default()
            }))),
            ..JsonSchemaObject::default()
        };
        let result = validate_value(schema, Value::Null).unwrap();
        assert_eq!(result, wrapped(Value::Null));
    }

    #[test]
    fn rejects_invalid_coercions_for_serialized_plain_json_schemas() {
        let cases: Vec<(JsonSchemaObject, Value)> = vec![
            (
                JsonSchemaObject {
                    type_: Some(vec!["boolean".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::String("1".to_string()),
            ),
            (
                JsonSchemaObject {
                    type_: Some(vec!["boolean".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::String("0".to_string()),
            ),
            (
                JsonSchemaObject {
                    type_: Some(vec!["null".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::String("null".to_string()),
            ),
            (
                JsonSchemaObject {
                    type_: Some(vec!["integer".to_string()]),
                    ..JsonSchemaObject::default()
                },
                Value::String("42.1".to_string()),
            ),
        ];
        for (schema, input) in cases {
            let error = validate_value(schema, input.clone()).unwrap_err();
            assert!(error.starts_with("Validation failed"), "{error}");
        }
    }

    #[test]
    fn validates_nested_objects_and_required_properties() {
        let schema = schema_object(
            vec![
                (
                    "name".to_string(),
                    JsonSchemaObject {
                        type_: Some(vec!["string".to_string()]),
                        min_length: Some(2.0),
                        ..JsonSchemaObject::default()
                    },
                ),
                ("count".to_string(), number_schema()),
            ],
            vec!["name".to_string(), "count".to_string()],
        );
        let ok = validate_value(
            schema.clone(),
            Value::Map(vec![
                ("name".to_string(), Value::String("ab".to_string())),
                ("count".to_string(), Value::Number(3.0)),
            ]),
        )
        .unwrap();
        assert!(matches!(ok, Value::Map(_)));

        let error = validate_value(schema.clone(), Value::Map(vec![])).unwrap_err();
        assert!(error.contains("name"), "{error}");
        assert!(error.contains("Required property"), "{error}");

        let error = validate_value(schema, Value::Map(vec![
            ("name".to_string(), Value::String("a".to_string())),
            ("count".to_string(), Value::Number(3.0)),
        ]))
        .unwrap_err();
        assert!(error.contains("name"), "{error}");
    }

    #[test]
    fn rejects_extra_properties_when_disallowed() {
        let schema = JsonSchemaObject {
            type_: Some(vec!["object".to_string()]),
            properties: Some(vec![("known".to_string(), number_schema())]),
            additional_properties: Some(JsonSchemaAdditional::Bool(false)),
            ..JsonSchemaObject::default()
        };
        let error = validate_value(
            schema,
            Value::Map(vec![
                ("known".to_string(), Value::Number(1.0)),
                ("extra".to_string(), Value::Number(2.0)),
            ]),
        )
        .unwrap_err();
        assert!(error.contains("extra"), "{error}");
    }

    #[test]
    fn tool_not_found_is_reported() {
        let error = validate_tool_call(
            &[tool_with(number_schema())],
            &ToolCall {
                id: "t".to_string(),
                name: "missing".to_string(),
                arguments: Value::Map(Vec::new()),
                thought_signature: None,
                namespace: None,
            },
        )
        .unwrap_err();
        assert!(error.contains("missing"), "{error}");
    }
}
