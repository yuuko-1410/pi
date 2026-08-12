//! Edit tool, port of `tools/edit.ts`. TUI render components are skipped
//! (rendering happens in interactive mode); the execute logic and details
//! (diff, patch, firstChangedLine) are ported.

use std::fs;

use pi_protocol::Value;

use super::edit_diff::{
    apply_edits_to_normalized_content, detect_line_ending, generate_diff_string, generate_unified_patch,
    normalize_to_lf, restore_line_endings, strip_bom, Edit,
};
use super::path_utils::resolve_to_cwd;
use super::file_mutation_queue::with_file_mutation_queue;

pub const EDIT_TOOL_SYSTEM_PROMPT_CONTRIBUTION_SNIPPET: &str =
    "Make precise file edits with exact text replacement, including multiple disjoint edits in one call";
pub const EDIT_TOOL_SYSTEM_PROMPT_GUIDELINES: [&str; 4] = [
    "Use edit for precise changes (edits[].oldText must match exactly)",
    "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls",
    "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.",
    "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.",
];

/// Pluggable operations for the edit tool.
pub trait EditOperations {
    fn read_file(&self, path: &str) -> Result<String, String>;
    fn write_file(&self, path: &str, content: &str) -> Result<(), String>;
    fn access(&self, path: &str) -> Result<(), String>;
}

pub struct LocalEditOperations;

impl EditOperations for LocalEditOperations {
    fn read_file(&self, path: &str) -> Result<String, String> {
        fs::read_to_string(path).map_err(|error| error.to_string())
    }
    fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        fs::write(path, content).map_err(|error| error.to_string())
    }
    fn access(&self, path: &str) -> Result<(), String> {
        fs::metadata(path).map(|_| ()).map_err(|error| error.to_string())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EditToolDetails {
    pub diff: String,
    pub patch: String,
    pub first_changed_line: Option<usize>,
}

pub struct EditToolOptions {
    pub operations: Box<dyn EditOperations>,
}

impl Default for EditToolOptions {
    fn default() -> Self {
        Self {
            operations: Box::new(LocalEditOperations),
        }
    }
}

/// Normalize tool input: edits may arrive as a JSON string, or as legacy
/// oldText/newText fields merged into edits.
pub fn prepare_edit_arguments(input: Value) -> Value {
    let Some(mut entries) = input.as_map().map(|map| map.to_vec()) else {
        return input;
    };
    // Parse a string edits field.
    if let Some((_, Value::String(text))) = entries.iter().find(|(k, _)| k == "edits") {
        if let Ok(parsed) = pi_ai::utils::json::parse_json_with_repair::<Value>(text) {
            if matches!(parsed, Value::Array(_)) {
                if let Some(slot) = entries.iter_mut().find(|(k, _)| k == "edits") {
                    slot.1 = parsed;
                }
            }
        }
    }
    // Merge legacy oldText/newText into edits.
    let old_text = entries.iter().find(|(k, _)| k == "oldText").and_then(|(_, v)| v.as_str());
    let new_text = entries.iter().find(|(k, _)| k == "newText").and_then(|(_, v)| v.as_str());
    if let (Some(old_text), Some(new_text)) = (old_text, new_text) {
        let mut edits: Vec<Value> = entries
            .iter()
            .find(|(k, _)| k == "edits")
            .and_then(|(_, v)| v.as_array())
            .map(|array| array.to_vec())
            .unwrap_or_default();
        edits.push(Value::Map(vec![
            ("oldText".to_string(), Value::String(old_text.to_string())),
            ("newText".to_string(), Value::String(new_text.to_string())),
        ]));
        entries.retain(|(k, _)| k != "oldText" && k != "newText");
        entries.push(("edits".to_string(), Value::Array(edits)));
    }
    Value::Map(entries)
}

pub fn validate_edit_input(input: &Value) -> Result<(String, Vec<Edit>), String> {
    let entries = input.as_map().unwrap_or_default();
    let path = entries
        .iter()
        .find(|(k, _)| k == "path")
        .and_then(|(_, v)| v.as_str())
        .ok_or_else(|| "path is required".to_string())?
        .to_string();
    let edits = entries
        .iter()
        .find(|(k, _)| k == "edits")
        .and_then(|(_, v)| v.as_array())
        .ok_or_else(|| "edits is required".to_string())?;
    let mut parsed_edits = Vec::new();
    for edit in edits {
        let edit_entries = edit.as_map().unwrap_or_default();
        let old_text = edit_entries
            .iter()
            .find(|(k, _)| k == "oldText")
            .and_then(|(_, v)| v.as_str())
            .ok_or_else(|| "each edit requires oldText".to_string())?
            .to_string();
        let new_text = edit_entries
            .iter()
            .find(|(k, _)| k == "newText")
            .and_then(|(_, v)| v.as_str())
            .unwrap_or("")
            .to_string();
        parsed_edits.push(Edit { old_text, new_text });
    }
    if parsed_edits.is_empty() {
        return Err("edits must not be empty".to_string());
    }
    Ok((path, parsed_edits))
}

/// Execute the edit tool (sync analog).
pub fn execute_edit(
    cwd: &str,
    input: &Value,
    operations: &dyn EditOperations,
) -> Result<(Vec<pi_ai::types::Content>, Option<EditToolDetails>), String> {
    let prepared = prepare_edit_arguments(input.clone());
    let (path, edits) = validate_edit_input(&prepared)?;
    let absolute_path = resolve_to_cwd(&path, cwd);

    with_file_mutation_queue(&absolute_path, || {
        // Check if the file exists.
        operations.access(&absolute_path).map_err(|error| {
            format!("Could not edit file: {path}. {error}.")
        })?;

        // Read the file.
        let raw_content = operations.read_file(&absolute_path)?;
        let (bom, content) = strip_bom(&raw_content);
        let original_ending = detect_line_ending(&content);
        let normalized_content = normalize_to_lf(&content);
        let applied = apply_edits_to_normalized_content(&normalized_content, &edits, &path)?;

        let final_content = format!("{bom}{}", restore_line_endings(&applied.new_content, original_ending));
        operations.write_file(&absolute_path, &final_content)?;

        let (diff, first_changed_line) = generate_diff_string(&applied.base_content, &applied.new_content, 4);
        let patch = generate_unified_patch(&path, &applied.base_content, &applied.new_content, 4);

        let text = format!("Successfully replaced {} block(s) in {path}.", edits.len());
        Ok((
            vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
                text,
                text_signature: None,
            })],
            Some(EditToolDetails {
                diff,
                patch,
                first_changed_line,
            }),
        ))
    })
}

pub fn edit_tool_parameters() -> Value {
    Value::Map(vec![
        (
            "path".to_string(),
            Value::Map(vec![("description".to_string(), Value::String("Path to the file to edit".to_string()))]),
        ),
        (
            "edits".to_string(),
            Value::Map(vec![(
                "description".to_string(),
                Value::String("Array of edits, each with oldText and newText".to_string()),
            )]),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(content: &str) -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("pi-edit-{}-{n}.txt", std::process::id()));
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().to_string()
    }

    fn edit_input(path: &str, old_text: &str, new_text: &str) -> Value {
        Value::Map(vec![
            ("path".to_string(), Value::String(path.to_string())),
            (
                "edits".to_string(),
                Value::Array(vec![Value::Map(vec![
                    ("oldText".to_string(), Value::String(old_text.to_string())),
                    ("newText".to_string(), Value::String(new_text.to_string())),
                ])]),
            ),
        ])
    }

    #[test]
    fn applies_edit_and_returns_diff() {
        let path = temp_file("line1\nline2\nline3");
        let (content, details) = execute_edit("/tmp", &edit_input(&path, "line2", "changed"), &LocalEditOperations)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "line1\nchanged\nline3"
        );
        assert_eq!(content.len(), 1);
        let details = details.unwrap();
        assert!(details.diff.contains("-2 line2"));
        assert!(details.diff.contains("+2 changed"));
        assert!(details.patch.starts_with("--- "));
        assert_eq!(details.first_changed_line, Some(2));
    }

    #[test]
    fn legacy_old_new_text_merged() {
        let input = Value::Map(vec![
            ("path".to_string(), Value::String("/tmp/x.txt".to_string())),
            ("oldText".to_string(), Value::String("a".to_string())),
            ("newText".to_string(), Value::String("b".to_string())),
        ]);
        let prepared = prepare_edit_arguments(input);
        let (path, edits) = validate_edit_input(&prepared).unwrap();
        assert_eq!(path, "/tmp/x.txt");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].old_text, "a");
        assert_eq!(edits[0].new_text, "b");
    }

    #[test]
    fn edits_as_json_string() {
        let input = Value::Map(vec![
            ("path".to_string(), Value::String("/tmp/x.txt".to_string())),
            (
                "edits".to_string(),
                Value::String(r#"[{"oldText":"a","newText":"b"}]"#.to_string()),
            ),
        ]);
        let prepared = prepare_edit_arguments(input);
        let (_, edits) = validate_edit_input(&prepared).unwrap();
        assert_eq!(edits.len(), 1);
    }

    #[test]
    fn missing_file_errors() {
        let input = edit_input("/definitely/not/here.txt", "a", "b");
        let error = execute_edit("/tmp", &input, &LocalEditOperations).unwrap_err();
        assert!(error.contains("Could not edit file"));
    }

    #[test]
    fn preserves_line_endings_and_bom() {
        let path = temp_file("\u{FEFF}one\r\ntwo\r\n");
        execute_edit("/tmp", &edit_input(&path, "two", "TWO"), &LocalEditOperations).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "\u{FEFF}one\r\nTWO\r\n");
    }
}
