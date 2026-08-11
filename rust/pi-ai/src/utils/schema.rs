//! Port of `packages/ai/src/utils/typebox-helpers.ts` (StringEnum).

use crate::types::JsonSchemaObject;
use pi_protocol::Value;

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
