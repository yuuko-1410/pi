//! Write tool, port of `tools/write.ts`.

use std::fs;
use std::path::Path;

use pi_protocol::Value;

use super::file_mutation_queue::with_file_mutation_queue;
use super::path_utils::resolve_to_cwd;

pub const WRITE_TOOL_SYSTEM_PROMPT_CONTRIBUTION_SNIPPET: &str = "Write file contents";

/// Pluggable operations for the write tool.
pub trait WriteOperations {
    fn mkdir(&self, path: &str) -> Result<(), String>;
    fn write_file(&self, path: &str, content: &str) -> Result<(), String>;
}

pub struct LocalWriteOperations;

impl WriteOperations for LocalWriteOperations {
    fn mkdir(&self, path: &str) -> Result<(), String> {
        fs::create_dir_all(path).map_err(|error| error.to_string())
    }
    fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        fs::write(path, content).map_err(|error| error.to_string())
    }
}

/// Execute the write tool (sync analog).
pub fn execute_write(
    cwd: &str,
    path: &str,
    content: &str,
    operations: &dyn WriteOperations,
) -> Result<(Vec<pi_ai::types::Content>, Option<Value>), String> {
    let absolute_path = resolve_to_cwd(path, cwd);
    let dir = Path::new(&absolute_path)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_default();

    with_file_mutation_queue(&absolute_path, || {
        // Create parent directories if needed.
        operations.mkdir(&dir)?;

        // Write the file contents.
        operations.write_file(&absolute_path, content)?;

        let text = format!("Successfully wrote {} bytes to {path}", content.len());
        Ok((
            vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
                text,
                text_signature: None,
            })],
            None,
        ))
    })
}

pub fn write_tool_parameters() -> Value {
    Value::Map(vec![
        (
            "path".to_string(),
            Value::Map(vec![(
                "description".to_string(),
                Value::String("Path to the file to write (relative or absolute)".to_string()),
            )]),
        ),
        (
            "content".to_string(),
            Value::Map(vec![(
                "description".to_string(),
                Value::String("Content to write to the file".to_string()),
            )]),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_file_and_creates_dirs() {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-write-{}-{n}", std::process::id()));
        let path = dir.join("sub").join("file.txt");
        let (content, _) = execute_write(
            "/tmp",
            &path.to_string_lossy(),
            "hello",
            &LocalWriteOperations,
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        assert!(matches!(&content[0], pi_ai::types::Content::Text(text) if text.text == "Successfully wrote 5 bytes to ".to_string() + &path.to_string_lossy()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn byte_count_uses_content_length() {
        let (content, _) = execute_write("/tmp", "/tmp/pi-write-test-out.txt", "abcde", &LocalWriteOperations)
            .unwrap();
        assert!(matches!(&content[0], pi_ai::types::Content::Text(text) if text.text.contains("5 bytes")));
        let _ = std::fs::remove_file("/tmp/pi-write-test-out.txt");
    }
}
