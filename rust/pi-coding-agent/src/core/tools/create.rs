//! Built-in tool factories, port of `core/tools/index.ts` +
//! `core/tools/{read,bash,edit,write,grep,find,ls}.ts` definition creation.
//!
//! ponytail: tool results return the text output as the sole message content
//! (JS returns a { content, details } object; details are dropped), and the
//! bash/edit/write/grep/find/ls definitions keep their parameters. Each
//! execute closure resolves args from the JSON value by key.

use std::sync::Arc;

use pi_ai::types::{Content, TextContent};
use pi_protocol::Value;

use super::bash::LocalBashOperations;
use super::edit::LocalEditOperations;
use crate::core::extensions::types::ToolDefinition;
use super::read::{execute_read, LocalReadOperations};
use super::write::LocalWriteOperations;

pub type ToolResult = Result<Value, String>;

fn text_result(content: &[Content]) -> Value {
    let mut output = String::new();
    for block in content {
        match block {
            Content::Text(TextContent { text, .. }) => {
                output.push_str(text);
                output.push('\n');
            }
            Content::Thinking(t) => {
                output.push_str(&t.thinking);
                output.push('\n');
            }
            _ => {}
        }
    }
    Value::String(output.trim_end_matches('\n').to_string())
}

fn get_str(args: &Value, key: &str) -> Option<String> {
    args.as_map()?
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_str())
        .map(|s| s.to_string())
}

fn get_num(args: &Value, key: &str) -> Option<f64> {
    args.as_map()?
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_number())
}

fn get_bool(args: &Value, key: &str) -> bool {
    args.as_map()
        .and_then(|entries| entries.iter().find(|(k, _)| k == key))
        .and_then(|(_, v)| v.as_bool())
        .unwrap_or(false)
}

fn json_schema(object: Value) -> Value {
    object
}

/// Read tool definition. Description mirrors the JS createReadToolDefinition.
pub fn create_read_tool(cwd: &str) -> ToolDefinition {
    let cwd = cwd.to_string();
    ToolDefinition {
        name: "read".into(),
        description: "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.".into(),
        parameters: Some(json_schema(Value::Map(vec![
            ("type".into(), Value::String("object".into())),
            (
                "properties".into(),
                Value::Map(vec![
                    (
                        "path".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("string".into())),
                            ("description".into(), Value::String("Path to the file to read (relative or absolute)".into())),
                        ]),
                    ),
                    (
                        "offset".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("number".into())),
                            ("description".into(), Value::String("Line number to start reading from (1-indexed)".into())),
                        ]),
                    ),
                    (
                        "limit".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("number".into())),
                            ("description".into(), Value::String("Maximum number of lines to read".into())),
                        ]),
                    ),
                ]),
            ),
            ("required".into(), Value::Array(vec![Value::String("path".into())])),
        ]))),
        execute: Arc::new(move |_tool_call_id, args, _state| {
            let path = get_str(&args, "path").ok_or_else(|| "Missing required argument: path".to_string())?;
            let ops = LocalReadOperations;
            let (content, _details) =
                execute_read(&cwd, &path, get_num(&args, "offset"), get_num(&args, "limit"), &ops)?;
            Ok(text_result(&content))
        }),
    }
}

/// Bash tool definition.
pub fn create_bash_tool(cwd: &str) -> ToolDefinition {
    let cwd = cwd.to_string();
    ToolDefinition {
        name: "bash".into(),
        description: "Execute bash commands (ls, grep, find, etc.)".into(),
        parameters: Some(json_schema(Value::Map(vec![
            ("type".into(), Value::String("object".into())),
            (
                "properties".into(),
                Value::Map(vec![
                    (
                        "command".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("string".into())),
                            ("description".into(), Value::String("Bash command to execute".into())),
                        ]),
                    ),
                    (
                        "timeout".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("number".into())),
                            ("description".into(), Value::String("Timeout in seconds (optional, no default timeout)".into())),
                        ]),
                    ),
                ]),
            ),
            ("required".into(), Value::Array(vec![Value::String("command".into())])),
        ]))),
        execute: Arc::new(move |_tool_call_id, args, _state| {
            let command = get_str(&args, "command").ok_or_else(|| "Missing required argument: command".to_string())?;
            let ops = LocalBashOperations::new(None);
            let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (content, _details) =
                super::bash::execute_bash_tool(&cwd, &command, get_num(&args, "timeout"), &ops, &cancelled)?;
            Ok(text_result(&content))
        }),
    }
}

/// Edit tool definition.
pub fn create_edit_tool(cwd: &str) -> ToolDefinition {
    let cwd = cwd.to_string();
    ToolDefinition {
        name: "edit".into(),
        description: "Make precise file edits with exact text replacement, including multiple disjoint edits in one call".into(),
        parameters: Some(json_schema(Value::Map(vec![
            ("type".into(), Value::String("object".into())),
            (
                "properties".into(),
                Value::Map(vec![
                    (
                        "path".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("string".into())),
                            ("description".into(), Value::String("Path to the file to edit (relative or absolute)".into())),
                        ]),
                    ),
                    (
                        "edits".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("array".into())),
                            ("description".into(), Value::String("One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits.".into())),
                            (
                                "items".into(),
                                Value::Map(vec![
                                    ("type".into(), Value::String("object".into())),
                                    (
                                        "properties".into(),
                                        Value::Map(vec![
                                            (
                                                "oldText".into(),
                                                Value::Map(vec![
                                                    ("type".into(), Value::String("string".into())),
                                                    ("description".into(), Value::String("Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call.".into())),
                                                ]),
                                            ),
                                            (
                                                "newText".into(),
                                                Value::Map(vec![
                                                    ("type".into(), Value::String("string".into())),
                                                    ("description".into(), Value::String("Replacement text for this targeted edit.".into())),
                                                ]),
                                            ),
                                        ]),
                                    ),
                                    ("required".into(), Value::Array(vec![Value::String("oldText".into()), Value::String("newText".into())])),
                                ]),
                            ),
                        ]),
                    ),
                ]),
            ),
            ("required".into(), Value::Array(vec![Value::String("path".into()), Value::String("edits".into())])),
        ]))),
        execute: Arc::new(move |_tool_call_id, args, _state| {
            let ops = LocalEditOperations;
            let (content, _details) = super::edit::execute_edit(&cwd, &args, &ops)?;
            Ok(text_result(&content))
        }),
    }
}

/// Write tool definition.
pub fn create_write_tool(cwd: &str) -> ToolDefinition {
    let cwd = cwd.to_string();
    ToolDefinition {
        name: "write".into(),
        description: "Create or overwrite files".into(),
        parameters: Some(json_schema(Value::Map(vec![
            ("type".into(), Value::String("object".into())),
            (
                "properties".into(),
                Value::Map(vec![
                    (
                        "path".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("string".into())),
                            ("description".into(), Value::String("Path to the file to write (relative or absolute)".into())),
                        ]),
                    ),
                    (
                        "content".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("string".into())),
                            ("description".into(), Value::String("Content to write to the file".into())),
                        ]),
                    ),
                ]),
            ),
            ("required".into(), Value::Array(vec![Value::String("path".into()), Value::String("content".into())])),
        ]))),
        execute: Arc::new(move |_tool_call_id, args, _state| {
            let path = get_str(&args, "path").ok_or_else(|| "Missing required argument: path".to_string())?;
            let content = get_str(&args, "content").ok_or_else(|| "Missing required argument: content".to_string())?;
            let ops = LocalWriteOperations;
            let (content_blocks, _details) = super::write::execute_write(&cwd, &path, &content, &ops)?;
            Ok(text_result(&content_blocks))
        }),
    }
}

/// Grep tool definition.
pub fn create_grep_tool(cwd: &str) -> ToolDefinition {
    let cwd = cwd.to_string();
    ToolDefinition {
        name: "grep".into(),
        description: "Search file contents for patterns (respects .gitignore)".into(),
        parameters: Some(json_schema(Value::Map(vec![
            ("type".into(), Value::String("object".into())),
            (
                "properties".into(),
                Value::Map(vec![
                    (
                        "pattern".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("string".into())),
                            ("description".into(), Value::String("Search pattern (regex or literal string)".into())),
                        ]),
                    ),
                    (
                        "path".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("string".into())),
                            ("description".into(), Value::String("Directory or file to search (default: current directory)".into())),
                        ]),
                    ),
                    (
                        "glob".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("string".into())),
                            ("description".into(), Value::String("Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'".into())),
                        ]),
                    ),
                    (
                        "ignoreCase".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("boolean".into())),
                            ("description".into(), Value::String("Case-insensitive search (default: false)".into())),
                        ]),
                    ),
                    (
                        "literal".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("boolean".into())),
                            ("description".into(), Value::String("Treat pattern as literal string instead of regex (default: false)".into())),
                        ]),
                    ),
                    (
                        "context".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("number".into())),
                            ("description".into(), Value::String("Number of lines to show before and after each match (default: 0)".into())),
                        ]),
                    ),
                    (
                        "limit".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("number".into())),
                            ("description".into(), Value::String("Maximum number of matches to return (default: 100)".into())),
                        ]),
                    ),
                ]),
            ),
            ("required".into(), Value::Array(vec![Value::String("pattern".into())])),
        ]))),
        execute: Arc::new(move |_tool_call_id, args, _state| {
            let pattern = get_str(&args, "pattern").ok_or_else(|| "Missing required argument: pattern".to_string())?;
            let (content, _details) = super::grep::execute_grep_tool(
                &cwd,
                &pattern,
                get_str(&args, "path").as_deref(),
                get_str(&args, "glob").as_deref(),
                get_bool(&args, "ignoreCase"),
                get_bool(&args, "literal"),
                get_num(&args, "context"),
                get_num(&args, "limit"),
            )?;
            Ok(text_result(&content))
        }),
    }
}

/// Find tool definition.
pub fn create_find_tool(cwd: &str) -> ToolDefinition {
    let cwd = cwd.to_string();
    ToolDefinition {
        name: "find".into(),
        description: "Find files by glob pattern (respects .gitignore)".into(),
        parameters: Some(json_schema(Value::Map(vec![
            ("type".into(), Value::String("object".into())),
            (
                "properties".into(),
                Value::Map(vec![
                    (
                        "pattern".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("string".into())),
                            ("description".into(), Value::String("Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'".into())),
                        ]),
                    ),
                    (
                        "path".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("string".into())),
                            ("description".into(), Value::String("Directory to search in (default: current directory)".into())),
                        ]),
                    ),
                    (
                        "limit".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("number".into())),
                            ("description".into(), Value::String("Maximum number of results (default: 1000)".into())),
                        ]),
                    ),
                ]),
            ),
            ("required".into(), Value::Array(vec![Value::String("pattern".into())])),
        ]))),
        execute: Arc::new(move |_tool_call_id, args, _state| {
            let pattern = get_str(&args, "pattern").ok_or_else(|| "Missing required argument: pattern".to_string())?;
            let (content, _details) =
                super::find::execute_find_tool(&cwd, &pattern, get_str(&args, "path").as_deref(), get_num(&args, "limit"))?;
            Ok(text_result(&content))
        }),
    }
}

/// Ls tool definition.
pub fn create_ls_tool(cwd: &str) -> ToolDefinition {
    let cwd = cwd.to_string();
    ToolDefinition {
        name: "ls".into(),
        description: "List directory contents".into(),
        parameters: Some(json_schema(Value::Map(vec![
            ("type".into(), Value::String("object".into())),
            (
                "properties".into(),
                Value::Map(vec![
                    (
                        "path".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("string".into())),
                            ("description".into(), Value::String("Directory to list (default: current directory)".into())),
                        ]),
                    ),
                    (
                        "limit".into(),
                        Value::Map(vec![
                            ("type".into(), Value::String("number".into())),
                            ("description".into(), Value::String("Maximum number of entries to return (default: 500)".into())),
                        ]),
                    ),
                ]),
            ),
            ("required".into(), Value::Array(vec![])),
        ]))),
        execute: Arc::new(move |_tool_call_id, args, _state| {
            let (content, _details) =
                super::ls::execute_ls_tool(&cwd, get_str(&args, "path").as_deref(), get_num(&args, "limit"))?;
            Ok(text_result(&content))
        }),
    }
}

/// The default built-in tool set (read, bash, edit, write), matching
/// createCodingTools.
pub fn create_coding_tools(cwd: &str) -> Vec<ToolDefinition> {
    vec![
        create_read_tool(cwd),
        create_bash_tool(cwd),
        create_edit_tool(cwd),
        create_write_tool(cwd),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_tools_are_defined() {
        let tools = create_coding_tools("/tmp");
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, vec!["read", "bash", "edit", "write"]);
        for tool in &tools {
            assert!(tool.parameters.is_some());
        }
    }

    #[test]
    fn read_tool_executes() {
        let dir = std::env::temp_dir().join(format!("pi-tool-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "line1\nline2\n").unwrap();
        let tool = create_read_tool(dir.to_string_lossy().as_ref());
        let args = Value::Map(vec![("path".into(), Value::String(path.to_string_lossy().to_string()))]);
        let result = (tool.execute)("call-1", args, None).unwrap();
        let text = result.as_str().unwrap();
        assert!(text.contains("line1"));
        assert!(text.contains("line2"));
    }

    #[test]
    fn write_tool_executes() {
        let dir = std::env::temp_dir().join(format!("pi-tool-write-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = create_write_tool(dir.to_string_lossy().as_ref());
        let args = Value::Map(vec![
            ("path".into(), Value::String("out.txt".into())),
            ("content".into(), Value::String("hello".into())),
        ]);
        (tool.execute)("call-1", args, None).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("out.txt")).unwrap(), "hello");
    }

    #[test]
    fn missing_required_arg_errors() {
        let tool = create_read_tool("/tmp");
        let result = (tool.execute)("call-1", Value::Map(vec![]), None);
        assert!(result.is_err());
    }
}
