//! Port of `packages/ai/src/utils/typebox-helpers.ts` (StringEnum).

use crate::types::{JsonSchemaObject, JsonSchemaValue};
use pi_protocol::Value;
use pi_protocol::Value as JsonValue;

/// Creates a string enum schema compatible with Google's API and other
/// providers that don't support anyOf/const patterns. Mirrors the runtime
/// shape produced by TypeBox: `{ type: "string", enum: [...], description?,
/// default? }`.
pub fn string_enum(values: &[&str], description: Option<&str>, default: Option<&str>) -> JsonSchemaObject {
    JsonSchemaObject {
        type_: Some(vec!["string".to_string()]),
        description: description.map(|value| value.to_string()),
        enum_values: Some(
            values
                .iter()
                .map(|value| Value::String(value.to_string()))
                .collect(),
        ),
        default: default.map(|value| Value::String(value.to_string())),
        ..JsonSchemaObject::default()
    }
}

/// Convert a JSON Schema value (as produced by the tool definition builders)
/// into the JsonSchemaObject runtime representation. Handles type,
/// description, properties, required, items, and enum.
pub fn json_value_to_schema(value: &pi_protocol::Value) -> JsonSchemaObject {
    match value {
        pi_protocol::Value::Map(entries) => {
            let mut type_: Option<Vec<String>> = None;
            let mut description: Option<String> = None;
            let mut properties: Vec<(String, JsonSchemaObject)> = Vec::new();
            let mut required: Option<Vec<String>> = None;
            let mut items: Option<Box<JsonSchemaValue>> = None;
            let mut enum_values: Option<Vec<JsonValue>> = None;
            for (key, value) in entries {
                match key.as_str() {
                    "type" => {
                        if let Some(t) = value.as_str() {
                            type_ = Some(vec![t.to_string()]);
                        }
                    }
                    "description" => {
                        description = value.as_str().map(|s| s.to_string());
                    }
                    "properties" => {
                        if let Some(map) = value.as_map() {
                            properties = map
                                .iter()
                                .map(|(key, value)| (key.clone(), json_value_to_schema(value)))
                                .collect();
                        }
                    }
                    "required" => {
                        if let Some(array) = value.as_array() {
                            let list: Vec<String> = array
                                .iter()
                                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                                .collect();
                            if !list.is_empty() {
                                required = Some(list);
                            }
                        }
                    }
                    "items" => {
                        items = Some(Box::new(JsonSchemaValue::Schema(json_value_to_schema(value))));
                    }
                    "enum" => {
                        if let Some(array) = value.as_array() {
                            enum_values = Some(array.to_vec());
                        }
                    }
                    _ => {}
                }
            }
            JsonSchemaObject {
                type_,
                description,
                properties: if properties.is_empty() {
                    None
                } else {
                    Some(properties)
                },
                required,
                items,
                enum_values,
                ..Default::default()
            }
        }
        pi_protocol::Value::Array(items) => JsonSchemaObject {
            items: items
                .first()
                .map(|item| Box::new(JsonSchemaValue::Schema(json_value_to_schema(item)))),
            ..Default::default()
        },
        _ => JsonSchemaObject::default(),
    }
}
