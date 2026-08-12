//! Stdin buffering, port of `packages/tui/src/stdin-buffer.ts`.
//!
//! Buffers input and emits complete sequences (partial escape sequences
//! arriving across chunks are accumulated). The JS EventEmitter is replaced
//! by callbacks; the flush timeout is a caller responsibility (process
//! returns buffered remainder via `buffer`).

use std::sync::{Arc, Mutex};

const ESC: &str = "\x1b";
const BRACKETED_PASTE_START: &str = "\x1b[200~";
const BRACKETED_PASTE_END: &str = "\x1b[201~";

#[derive(Clone, Copy, Debug, PartialEq)]
enum SequenceStatus {
    Complete,
    Incomplete,
    NotEscape,
}

fn is_complete_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with(ESC) {
        return SequenceStatus::NotEscape;
    }
    if data.len() == 1 {
        return SequenceStatus::Incomplete;
    }
    let after_esc = &data[1..];

    if after_esc.starts_with('[') {
        if after_esc.starts_with("[M") {
            // Old-style mouse needs ESC[M + 3 bytes = 6 total.
            return if data.len() >= 6 {
                SequenceStatus::Complete
            } else {
                SequenceStatus::Incomplete
            };
        }
        return is_complete_csi_sequence(data);
    }
    if after_esc.starts_with(']') {
        return is_complete_osc_sequence(data);
    }
    if after_esc.starts_with('P') {
        return is_complete_dcs_sequence(data);
    }
    if after_esc.starts_with('_') {
        return is_complete_apc_sequence(data);
    }
    if after_esc.starts_with('O') {
        return if after_esc.len() >= 2 {
            SequenceStatus::Complete
        } else {
            SequenceStatus::Incomplete
        };
    }
    if after_esc.len() == 1 {
        return SequenceStatus::Complete;
    }
    SequenceStatus::Complete
}

fn is_complete_csi_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with(&format!("{ESC}[")) {
        return SequenceStatus::Complete;
    }
    if data.len() < 3 {
        return SequenceStatus::Incomplete;
    }
    let payload = &data[2..];
    let last_char = payload.chars().last().unwrap();
    let last_code = last_char as u32;

    if (0x40..=0x7E).contains(&last_code) {
        if payload.starts_with('<') {
            // SGR mouse: <digits;digits;digits[Mm]
            let mouse_match = payload.len() > 1
                && matches!(last_char, 'M' | 'm')
                && payload[1..payload.len() - 1].split(';').count() == 3
                && payload[1..payload.len() - 1]
                    .split(';')
                    .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
            if mouse_match {
                return SequenceStatus::Complete;
            }
            return SequenceStatus::Incomplete;
        }
        return SequenceStatus::Complete;
    }
    SequenceStatus::Incomplete
}

fn is_complete_osc_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with(&format!("{ESC}]")) {
        return SequenceStatus::Complete;
    }
    if data.ends_with(&format!("{ESC}\\")) || data.ends_with('\x07') {
        SequenceStatus::Complete
    } else {
        SequenceStatus::Incomplete
    }
}

fn is_complete_dcs_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with(&format!("{ESC}P")) {
        return SequenceStatus::Complete;
    }
    if data.ends_with(&format!("{ESC}\\")) {
        SequenceStatus::Complete
    } else {
        SequenceStatus::Incomplete
    }
}

fn is_complete_apc_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with(&format!("{ESC}_")) {
        return SequenceStatus::Complete;
    }
    if data.ends_with(&format!("{ESC}\\")) {
        SequenceStatus::Complete
    } else {
        SequenceStatus::Incomplete
    }
}

fn parse_unmodified_kitty_printable_codepoint(sequence: &str) -> Option<u32> {
    let rest = sequence.strip_prefix("\x1b[")?.strip_suffix('u')?;
    let mut parts = rest.split([':', ';']);
    let codepoint = parts.next()?.parse::<u32>().ok()?;
    if codepoint >= 32 {
        Some(codepoint)
    } else {
        None
    }
}

/// Split an accumulated buffer into complete sequences.
pub fn extract_complete_sequences(buffer: &str) -> (Vec<String>, String) {
    let mut sequences: Vec<String> = Vec::new();
    let mut pos = 0;
    let bytes = buffer.as_bytes();

    while pos < bytes.len() {
        let remaining = &buffer[pos..];

        if remaining.starts_with(ESC) {
            let mut seq_end = 1;
            let mut extracted = false;
            while seq_end <= remaining.len() {
                let candidate = &remaining[..seq_end];
                match is_complete_sequence(candidate) {
                    SequenceStatus::Complete => {
                        // WezTerm escape-key handling: "\x1b\x1b" followed by a
                        // new escape introducer emits only the first ESC.
                        if candidate == "\x1b\x1b" {
                            let next_char = remaining[seq_end..].chars().next();
                            if matches!(next_char, Some('[') | Some(']') | Some('O') | Some('P') | Some('_')) {
                                sequences.push(ESC.to_string());
                                pos += 1;
                                extracted = true;
                                break;
                            }
                        }
                        sequences.push(candidate.to_string());
                        pos += seq_end;
                        extracted = true;
                        break;
                    }
                    SequenceStatus::Incomplete => {
                        seq_end += 1;
                    }
                    SequenceStatus::NotEscape => {
                        sequences.push(candidate.to_string());
                        pos += seq_end;
                        extracted = true;
                        break;
                    }
                }
            }
            if !extracted {
                return (sequences, remaining.to_string());
            }
        } else {
            // Not an escape sequence: take a single character.
            let char = remaining.chars().next().unwrap();
            sequences.push(char.to_string());
            pos += char.len_utf8();
        }
    }

    (sequences, String::new())
}

/// Event emitted by the stdin buffer.
#[derive(Clone, Debug, PartialEq)]
pub enum StdinEvent {
    Data(String),
    Paste(String),
}

pub struct StdinBufferOptions {
    pub timeout: Option<f64>,
}

impl Default for StdinBufferOptions {
    fn default() -> Self {
        Self { timeout: Some(10.0) }
    }
}

/// Buffers stdin input and emits complete sequences.
pub struct StdinBuffer {
    buffer: String,
    timeout_ms: f64,
    paste_mode: bool,
    paste_buffer: String,
    pending_kitty_printable_codepoint: Option<u32>,
}

impl StdinBuffer {
    pub fn new(options: StdinBufferOptions) -> Self {
        Self {
            buffer: String::new(),
            timeout_ms: options.timeout.unwrap_or(10.0),
            paste_mode: false,
            paste_buffer: String::new(),
            pending_kitty_printable_codepoint: None,
        }
    }

    /// Feed input data; returns the events produced by this chunk.
    /// Unfinished buffered data stays internal (flush() returns it).
    pub fn process(&mut self, data: &[u8]) -> Vec<StdinEvent> {
        let mut events: Vec<StdinEvent> = Vec::new();

        // High-byte conversion: single byte > 127 becomes ESC + (byte - 128).
        let str: String = if data.len() == 1 && data[0] > 127 {
            format!("{ESC}{}", (data[0] - 128) as char)
        } else {
            String::from_utf8_lossy(data).to_string()
        };

        if str.is_empty() && self.buffer.is_empty() {
            events.push(StdinEvent::Data(String::new()));
            return events;
        }

        self.buffer.push_str(&str);

        if self.paste_mode {
            self.paste_buffer.push_str(&self.buffer);
            self.buffer.clear();
            if let Some(end_index) = self.paste_buffer.find(BRACKETED_PASTE_END) {
                let pasted_content = self.paste_buffer[..end_index].to_string();
                let remaining = self.paste_buffer[end_index + BRACKETED_PASTE_END.len()..].to_string();
                self.paste_mode = false;
                self.paste_buffer.clear();
                self.pending_kitty_printable_codepoint = None;
                events.push(StdinEvent::Paste(pasted_content));
                if !remaining.is_empty() {
                    events.extend(self.process(remaining.as_bytes()));
                }
            }
            return events;
        }

        if let Some(start_index) = self.buffer.find(BRACKETED_PASTE_START) {
            if start_index > 0 {
                let before_paste = self.buffer[..start_index].to_string();
                let (sequences, _) = extract_complete_sequences(&before_paste);
                for sequence in sequences {
                    self.emit_data_sequence(&sequence, &mut events);
                }
            }
            self.pending_kitty_printable_codepoint = None;
            self.buffer = self.buffer[start_index + BRACKETED_PASTE_START.len()..].to_string();
            self.paste_mode = true;
            self.paste_buffer = self.buffer.clone();
            self.buffer.clear();
            if let Some(end_index) = self.paste_buffer.find(BRACKETED_PASTE_END) {
                let pasted_content = self.paste_buffer[..end_index].to_string();
                let remaining = self.paste_buffer[end_index + BRACKETED_PASTE_END.len()..].to_string();
                self.paste_mode = false;
                self.paste_buffer.clear();
                self.pending_kitty_printable_codepoint = None;
                events.push(StdinEvent::Paste(pasted_content));
                if !remaining.is_empty() {
                    events.extend(self.process(remaining.as_bytes()));
                }
            }
            return events;
        }

        let (sequences, remainder) = extract_complete_sequences(&self.buffer);
        self.buffer = remainder;
        for sequence in sequences {
            self.emit_data_sequence(&sequence, &mut events);
        }
        events
    }

    fn emit_data_sequence(&mut self, sequence: &str, events: &mut Vec<StdinEvent>) {
        let raw_codepoint = if sequence.chars().count() == 1 {
            sequence.chars().next().map(|c| c as u32)
        } else {
            None
        };
        if let Some(raw_codepoint) = raw_codepoint {
            if Some(raw_codepoint) == self.pending_kitty_printable_codepoint {
                self.pending_kitty_printable_codepoint = None;
                return;
            }
        }
        self.pending_kitty_printable_codepoint = parse_unmodified_kitty_printable_codepoint(sequence);
        events.push(StdinEvent::Data(sequence.to_string()));
    }

    /// Flush any remaining buffered data as a single sequence (the JS
    /// timeout flush).
    pub fn flush(&mut self) -> Vec<String> {
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let sequences = vec![self.buffer.clone()];
        self.buffer.clear();
        self.pending_kitty_printable_codepoint = None;
        sequences
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.paste_mode = false;
        self.paste_buffer.clear();
        self.pending_kitty_printable_codepoint = None;
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_chars_emit_immediately() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        let events = buffer.process(b"ab");
        assert_eq!(
            events,
            vec![StdinEvent::Data("a".to_string()), StdinEvent::Data("b".to_string())]
        );
        assert!(buffer.buffer().is_empty());
    }

    #[test]
    fn escape_split_across_chunks() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        let events = buffer.process(b"\x1b");
        assert!(events.is_empty());
        assert_eq!(buffer.buffer(), "\x1b");
        let events = buffer.process(b"[A");
        assert_eq!(events, vec![StdinEvent::Data("\x1b[A".to_string())]);
    }

    #[test]
    fn meta_key_single_char() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        let events = buffer.process(b"\x1bx");
        assert_eq!(events, vec![StdinEvent::Data("\x1bx".to_string())]);
    }

    #[test]
    fn csi_complete_and_incomplete() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        let events = buffer.process(b"\x1b[1;5D");
        assert_eq!(events, vec![StdinEvent::Data("\x1b[1;5D".to_string())]);

        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        buffer.process(b"\x1b[1");
        assert_eq!(buffer.buffer(), "\x1b[1");
    }

    #[test]
    fn osc_requires_terminator() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        buffer.process(b"\x1b]8;;http://x");
        assert!(!buffer.buffer().is_empty());
        let events = buffer.process(b"\x07");
        assert_eq!(events, vec![StdinEvent::Data("\x1b]8;;http://x\x07".to_string())]);
    }

    #[test]
    fn bracketed_paste() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        let events = buffer.process(b"\x1b[200~pasted \x1b[201~");
        assert_eq!(events, vec![StdinEvent::Paste("pasted ".to_string())]);
    }

    #[test]
    fn paste_split_across_chunks() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        buffer.process(b"\x1b[200~hello");
        assert!(buffer.buffer().is_empty());
        let events = buffer.process(b" world\x1b[201~tail");
        assert!(events.contains(&StdinEvent::Paste("hello world".to_string())));
        // The remaining text is processed char by char.
        assert!(events.contains(&StdinEvent::Data("t".to_string())));
    }

    #[test]
    fn mouse_sgr_sequence() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        let events = buffer.process(b"\x1b[<35;20;5m");
        assert_eq!(events, vec![StdinEvent::Data("\x1b[<35;20;5m".to_string())]);
    }

    #[test]
    fn high_byte_conversion() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        // byte 0x81 -> ESC + 0x01
        let events = buffer.process(&[0x81]);
        assert_eq!(events, vec![StdinEvent::Data("\x1b\x01".to_string())]);
    }

    #[test]
    fn double_escape_before_sequence() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        // "\x1b\x1b[A": first ESC emitted alone, then the CSI completes.
        let events = buffer.process(b"\x1b\x1b[A");
        assert_eq!(
            events,
            vec![StdinEvent::Data("\x1b".to_string()), StdinEvent::Data("\x1b[A".to_string())]
        );
    }

    #[test]
    fn kitty_printable_dedup() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        let events = buffer.process(b"\x1b[97u");
        assert_eq!(events, vec![StdinEvent::Data("\x1b[97u".to_string())]);
        // A raw 'a' right after the kitty printable is suppressed.
        let events = buffer.process(b"a");
        assert!(events.is_empty());
        let events = buffer.process(b"b");
        assert_eq!(events, vec![StdinEvent::Data("b".to_string())]);
    }

    #[test]
    fn flush_returns_remainder() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        buffer.process(b"\x1b[1");
        let flushed = buffer.flush();
        assert_eq!(flushed, vec!["\x1b[1".to_string()]);
        assert!(buffer.flush().is_empty());
    }

    #[test]
    fn clear_resets_state() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        buffer.process(b"\x1b[200~partial");
        buffer.clear();
        assert!(buffer.buffer().is_empty());
        let events = buffer.process(b"plain");
        assert!(!events.is_empty());
    }
}
