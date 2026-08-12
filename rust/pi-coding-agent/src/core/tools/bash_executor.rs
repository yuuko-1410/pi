//! Bash command execution with streaming and cancellation, port of
//! `core/bash-executor.ts`. Synchronous: the operation runs on the calling
//! thread; cancellation is cooperative via a cancel flag checked by the
//! operations implementation.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use super::truncate::{truncate_tail, TruncationOptions, DEFAULT_MAX_BYTES};

/// Streaming hooks for a bash execution, mirroring the JS BashOperations.
pub trait BashOperations {
    /// Execute a command; invoke on_data with raw chunks as they arrive.
    fn exec(
        &self,
        command: &str,
        cwd: &str,
        on_data: &mut dyn FnMut(&[u8]),
        cancelled: &AtomicBool,
    ) -> Result<BashExecResult, String>;
}

pub struct BashExecResult {
    pub exit_code: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct BashResult {
    /// Combined stdout + stderr output (sanitized, possibly truncated).
    pub output: String,
    /// Process exit code (None if killed/cancelled).
    pub exit_code: Option<i64>,
    pub cancelled: bool,
    pub truncated: bool,
    /// Temp file containing full output when truncation occurred.
    pub full_output_path: Option<String>,
}

/// Random hex string for temp file names (8 bytes, JS randomBytes).
pub fn random_hex_8() -> String {
    let mut bytes = [0u8; 8];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = file.read_exact(&mut bytes);
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Sanitize raw output: strip ANSI escapes and binary garbage, drop CRs.
/// ponytail: ANSI stripping reuses pi-ai's sanitize helper when available;
/// here a minimal CSI/OSC stripper suffices for bash output.
pub fn sanitize_chunk(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    crate::utils::basics::strip_ansi(&text).replace('\r', "")
}



/// Execute a bash command via the operations, streaming and truncating
/// output. Mirror of executeBashWithOperations.
pub fn execute_bash_with_operations(
    command: &str,
    cwd: &str,
    operations: &dyn BashOperations,
    on_chunk: Option<&mut dyn FnMut(&str)>,
    cancelled_flag: &AtomicBool,
) -> Result<BashResult, String> {
    let max_output_bytes = DEFAULT_MAX_BYTES * 2;

    let mut output_chunks: Vec<String> = Vec::new();
    let mut output_bytes = 0usize;
    let mut temp_file_path: Option<String> = None;
    let mut total_bytes = 0usize;

    let mut on_chunk_mut = on_chunk;
    let mut on_data = |data: &[u8]| {
        total_bytes += data.len();
        let text = sanitize_chunk(data);

        if total_bytes > DEFAULT_MAX_BYTES && temp_file_path.is_none() {
            ensure_temp_file(&mut temp_file_path, &output_chunks);
        }

        if let Some(path) = &temp_file_path {
            append_to_temp(path, &text);
        }

        output_chunks.push(text.clone());
        output_bytes += text.len();
        while output_bytes > max_output_bytes && output_chunks.len() > 1 {
            let removed = output_chunks.remove(0);
            output_bytes -= removed.len();
        }

        if let Some(on_chunk) = on_chunk_mut.as_mut() {
            on_chunk(&text);
        }
    };

    let exec_result = operations.exec(command, cwd, &mut on_data, cancelled_flag);
    let cancelled = cancelled_flag.load(Ordering::SeqCst);

    let full_output = output_chunks.join("");
    let truncation = truncate_tail(&full_output, TruncationOptions::default());
    if truncation.truncated {
        ensure_temp_file(&mut temp_file_path, &output_chunks);
    }

    let result = match exec_result {
        Ok(result) => BashResult {
            output: if truncation.truncated { truncation.content } else { full_output },
            exit_code: if cancelled { None } else { result.exit_code },
            cancelled,
            truncated: truncation.truncated,
            full_output_path: temp_file_path,
        },
        Err(error) => {
            if cancelled {
                BashResult {
                    output: if truncation.truncated { truncation.content } else { full_output },
                    exit_code: None,
                    cancelled: true,
                    truncated: truncation.truncated,
                    full_output_path: temp_file_path,
                }
            } else {
                return Err(error);
            }
        }
    };
    Ok(result)
}

fn ensure_temp_file(temp_file_path: &mut Option<String>, chunks: &[String]) {
    if temp_file_path.is_some() {
        return;
    }
    let path = std::env::temp_dir().join(format!("pi-bash-{}.log", random_hex_8()));
    let path = path.to_string_lossy().to_string();
    let mut stream = OpenOptions::new()
        .write(true)
        .create(true)
        .open(&path)
        .expect("create bash temp file");
    for chunk in chunks {
        let _ = stream.write_all(chunk.as_bytes());
    }
    *temp_file_path = Some(path);
}

fn append_to_temp(path: &str, text: &str) {
    if let Ok(mut stream) = OpenOptions::new().append(true).open(path) {
        let _ = stream.write_all(text.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeOps {
        chunks: Vec<String>,
        exit_code: Option<i64>,
    }

    impl BashOperations for FakeOps {
        fn exec(
            &self,
            _command: &str,
            _cwd: &str,
            on_data: &mut dyn FnMut(&[u8]),
            _cancelled: &AtomicBool,
        ) -> Result<BashExecResult, String> {
            for chunk in &self.chunks {
                on_data(chunk.as_bytes());
            }
            Ok(BashExecResult {
                exit_code: self.exit_code,
            })
        }
    }

    #[test]
    fn executes_and_streams() {
        let ops = FakeOps {
            chunks: vec!["hello\n".to_string(), "\u{1b}[31mworld\u{1b}[0m\n".to_string()],
            exit_code: Some(0),
        };
        let cancelled = AtomicBool::new(false);
        let mut streamed = String::new();
        let streamed_ref = &mut streamed;
        let mut on_chunk = move |chunk: &str| streamed_ref.push_str(chunk);
        let result =
            execute_bash_with_operations("cmd", "/tmp", &ops, Some(&mut on_chunk), &cancelled).unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.cancelled);
        assert!(!result.truncated);
        assert_eq!(result.output, "hello\nworld\n");
        assert_eq!(streamed, "hello\nworld\n");
        assert!(result.full_output_path.is_none());
    }

    #[test]
    fn cancelled_reports_undefined_exit_code() {
        let ops = FakeOps {
            chunks: vec!["out".to_string()],
            exit_code: Some(0),
        };
        let cancelled = AtomicBool::new(true);
        let result = execute_bash_with_operations("cmd", "/tmp", &ops, None, &cancelled).unwrap();
        assert!(result.cancelled);
        assert_eq!(result.exit_code, None);
    }

    #[test]
    fn truncation_writes_temp_file() {
        let big = "x".repeat(DEFAULT_MAX_BYTES * 2);
        let ops = FakeOps {
            chunks: vec![big.clone()],
            exit_code: Some(0),
        };
        let cancelled = AtomicBool::new(false);
        let result = execute_bash_with_operations("cmd", "/tmp", &ops, None, &cancelled).unwrap();
        assert!(result.truncated);
        let path = result.full_output_path.expect("temp file on truncation");
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.len(), big.len());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn random_hex_8_is_16_chars() {
        assert_eq!(random_hex_8().len(), 16);
        assert_ne!(random_hex_8(), random_hex_8());
    }
}
