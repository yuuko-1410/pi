//! Incremental streaming output tracker, port of `tools/output-accumulator.ts`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use super::truncate::{truncate_tail, TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};

pub struct OutputAccumulatorOptions {
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
    pub temp_file_prefix: Option<String>,
}

impl Default for OutputAccumulatorOptions {
    fn default() -> Self {
        Self {
            max_lines: None,
            max_bytes: None,
            temp_file_prefix: None,
        }
    }
}

pub struct OutputSnapshot {
    pub content: String,
    pub truncation: TruncationResult,
    pub full_output_path: Option<String>,
}

fn default_temp_file_path(prefix: &str) -> String {
    let id = super::bash_executor::random_hex_8();
    std::env::temp_dir().join(format!("{prefix}-{id}.log")).to_string_lossy().to_string()
}

/// Incrementally tracks streaming output with bounded memory: keeps a decoded
/// tail for display snapshots and opens a temp file when full output needs
/// preserving.
pub struct OutputAccumulator {
    max_lines: usize,
    max_bytes: usize,
    max_rolling_bytes: usize,
    temp_file_prefix: String,

    raw_chunks: Vec<Vec<u8>>,
    tail_text: String,
    tail_bytes: usize,
    tail_starts_at_line_boundary: bool,
    total_raw_bytes: usize,
    total_decoded_bytes: usize,
    completed_lines: usize,
    total_lines: usize,
    current_line_bytes: usize,
    has_open_line: bool,
    finished: bool,

    temp_file_path: Option<String>,
    temp_file: Option<File>,
}

impl OutputAccumulator {
    pub fn new(options: OutputAccumulatorOptions) -> Self {
        let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
        let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
        Self {
            max_lines,
            max_bytes,
            max_rolling_bytes: max_bytes.saturating_mul(2).max(1),
            temp_file_prefix: options.temp_file_prefix.unwrap_or_else(|| "pi-output".to_string()),
            raw_chunks: Vec::new(),
            tail_text: String::new(),
            tail_bytes: 0,
            tail_starts_at_line_boundary: true,
            total_raw_bytes: 0,
            total_decoded_bytes: 0,
            completed_lines: 0,
            total_lines: 0,
            current_line_bytes: 0,
            has_open_line: false,
            finished: false,
            temp_file_path: None,
            temp_file: None,
        }
    }

    pub fn append(&mut self, data: &[u8]) {
        if self.finished {
            panic!("Cannot append to a finished output accumulator");
        }
        self.total_raw_bytes += data.len();
        self.append_decoded_text(&String::from_utf8_lossy(data));

        if self.temp_file.is_some() || self.should_use_temp_file() {
            self.ensure_temp_file();
            if let Some(temp_file) = self.temp_file.as_mut() {
                let _ = temp_file.write_all(data);
            }
        } else if !data.is_empty() {
            self.raw_chunks.push(data.to_vec());
        }
    }

    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.should_use_temp_file() {
            self.ensure_temp_file();
        }
    }

    pub fn snapshot(&mut self, persist_if_truncated: bool) -> OutputSnapshot {
        let tail_truncation = truncate_tail(&self.get_snapshot_text(), TruncationOptions {
            max_lines: Some(self.max_lines),
            max_bytes: Some(self.max_bytes),
        });
        let truncated = self.total_lines > self.max_lines || self.total_decoded_bytes > self.max_bytes;
        let truncated_by = if truncated {
            Some(
                tail_truncation
                    .truncated_by
                    .unwrap_or(if self.total_decoded_bytes > self.max_bytes { "bytes" } else { "lines" }),
            )
        } else {
            None
        };
        let mut truncation = tail_truncation;
        truncation.truncated = truncated;
        truncation.truncated_by = truncated_by;
        truncation.total_lines = self.total_lines;
        truncation.total_bytes = self.total_decoded_bytes;
        truncation.max_lines = self.max_lines;
        truncation.max_bytes = self.max_bytes;

        if persist_if_truncated && truncation.truncated {
            self.ensure_temp_file();
        }

        OutputSnapshot {
            content: truncation.content.clone(),
            truncation,
            full_output_path: self.temp_file_path.clone(),
        }
    }

    pub fn get_last_line_bytes(&self) -> usize {
        self.current_line_bytes
    }

    fn append_decoded_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let bytes = text.len();
        self.total_decoded_bytes += bytes;
        self.tail_text.push_str(text);
        self.tail_bytes += bytes;
        if self.tail_bytes > self.max_rolling_bytes * 2 {
            self.trim_tail();
        }

        let mut newlines = 0usize;
        let mut last_newline = None;
        for (index, _) in text.match_indices('\n') {
            newlines += 1;
            last_newline = Some(index);
        }
        if newlines == 0 {
            self.current_line_bytes += bytes;
            self.has_open_line = true;
        } else {
            self.completed_lines += newlines;
            let tail = &text[last_newline.unwrap() + 1..];
            self.current_line_bytes = tail.len();
            self.has_open_line = !tail.is_empty();
        }
        self.total_lines = self.completed_lines + if self.has_open_line { 1 } else { 0 };
    }

    fn trim_tail(&mut self) {
        let bytes = self.tail_text.as_bytes();
        if bytes.len() <= self.max_rolling_bytes {
            self.tail_bytes = bytes.len();
            return;
        }
        let mut start = bytes.len() - self.max_rolling_bytes;
        while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
            start += 1;
        }
        self.tail_starts_at_line_boundary = if start == 0 {
            self.tail_starts_at_line_boundary
        } else {
            bytes[start - 1] == 0x0a
        };
        self.tail_text = String::from_utf8_lossy(&bytes[start..]).to_string();
        self.tail_bytes = self.tail_text.len();
    }

    fn get_snapshot_text(&self) -> String {
        if self.tail_starts_at_line_boundary {
            return self.tail_text.clone();
        }
        match self.tail_text.find('\n') {
            Some(first_newline) => self.tail_text[first_newline + 1..].to_string(),
            None => self.tail_text.clone(),
        }
    }

    fn should_use_temp_file(&self) -> bool {
        self.total_raw_bytes > self.max_bytes
            || self.total_decoded_bytes > self.max_bytes
            || self.total_lines > self.max_lines
    }

    fn ensure_temp_file(&mut self) {
        if self.temp_file_path.is_some() {
            return;
        }
        let path: PathBuf = default_temp_file_path(&self.temp_file_prefix).into();
        let file = OpenOptions::new().write(true).create(true).open(&path).expect("create output temp file");
        self.temp_file_path = Some(path.to_string_lossy().to_string());
        let mut file = file;
        for chunk in &self.raw_chunks {
            let _ = file.write_all(chunk);
        }
        self.raw_chunks.clear();
        self.temp_file = Some(file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_output_stays_in_memory() {
        let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
        accumulator.append(b"hello\n");
        accumulator.append(b"world\n");
        accumulator.finish();
        let snapshot = accumulator.snapshot(false);
        // JS returns the original content untouched when not truncated.
        assert_eq!(snapshot.content, "hello\nworld\n");
        assert!(!snapshot.truncation.truncated);
        assert!(snapshot.full_output_path.is_none());
    }

    #[test]
    fn line_counting() {
        let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
        accumulator.append(b"a\nb\nc");
        assert_eq!(accumulator.get_last_line_bytes(), 1);
        accumulator.finish();
        let snapshot = accumulator.snapshot(false);
        assert_eq!(snapshot.truncation.total_lines, 3);
    }

    #[test]
    fn truncation_persists_temp_file() {
        let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions {
            max_lines: Some(2),
            max_bytes: None,
            temp_file_prefix: Some("pi-test-output".to_string()),
        });
        accumulator.append(b"line1\nline2\nline3\nline4");
        accumulator.finish();
        let snapshot = accumulator.snapshot(true);
        assert!(snapshot.truncation.truncated);
        assert!(snapshot.full_output_path.is_some());
        let content = std::fs::read_to_string(snapshot.full_output_path.unwrap()).unwrap();
        assert_eq!(content, "line1\nline2\nline3\nline4");
    }

    #[test]
    fn partial_last_line_counts() {
        let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
        accumulator.append(b"a\nb");
        accumulator.finish();
        let snapshot = accumulator.snapshot(false);
        assert_eq!(snapshot.truncation.total_lines, 2);
        assert_eq!(snapshot.content, "a\nb");
    }

    #[test]
    fn append_after_finish_panics() {
        let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
        accumulator.finish();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| accumulator.append(b"x")));
        assert!(result.is_err());
    }
}
