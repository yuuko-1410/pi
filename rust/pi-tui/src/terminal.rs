//! Terminal I/O, port of `packages/tui/src/terminal.ts`.
//!
//! Differences: raw mode is toggled via the `stty` command (no termios
//! dependency); the write log uses the PI_TUI_WRITE_LOG env var; the native
//! Windows console helper and `process.stdout.resize` are not applicable.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::keys::set_kitty_protocol_active;
#[allow(unused_imports)]
use crate::stdin_buffer::{StdinBuffer, StdinBufferOptions, StdinEvent};

const TERMINAL_PROGRESS_ACTIVE_SEQUENCE: &str = "\x1b]9;4;3\x07";
const TERMINAL_PROGRESS_CLEAR_SEQUENCE: &str = "\x1b]9;4;0\x07";
const NATIVE_SHIFT_ENTER_SEQUENCE: &str = "\x1b[13;2u";
const KITTY_KEYBOARD_PROTOCOL_QUERY: &str = "\x1b[>7u\x1b[?u\x1b[c";

#[derive(Clone, Debug, PartialEq)]
pub enum KeyboardProtocolNegotiationSequence {
    KittyFlags { flags: u32 },
    DeviceAttributes,
}

pub fn parse_keyboard_protocol_negotiation_sequence(
    sequence: &str,
) -> Option<KeyboardProtocolNegotiationSequence> {
    if let Some(rest) = sequence.strip_prefix("\x1b[?").and_then(|rest| rest.strip_suffix('u')) {
        if let Ok(flags) = rest.parse::<u32>() {
            return Some(KeyboardProtocolNegotiationSequence::KittyFlags { flags });
        }
    }
    if sequence.starts_with("\x1b[?") && sequence.ends_with('c') && sequence[3..sequence.len() - 1]
        .chars()
        .all(|c| c.is_ascii_digit() || c == ';')
    {
        return Some(KeyboardProtocolNegotiationSequence::DeviceAttributes);
    }
    None
}

#[cfg(test)]
fn is_keyboard_protocol_negotiation_sequence_prefix(sequence: &str) -> bool {
    sequence == "\x1b["
        || (sequence.starts_with("\x1b[?") && sequence[3..].chars().all(|c| c.is_ascii_digit() || c == ';'))
}

pub fn is_apple_terminal_session() -> bool {
    std::env::var("TERM_PROGRAM").as_deref() == Ok("Apple_Terminal")
}

pub fn normalize_native_shift_enter_input(
    data: &str,
    should_detect_native_shift_enter: bool,
    is_shift_pressed: bool,
) -> String {
    if should_detect_native_shift_enter && data == "\r" && is_shift_pressed {
        NATIVE_SHIFT_ENTER_SEQUENCE.to_string()
    } else {
        data.to_string()
    }
}

pub fn normalize_apple_terminal_input(data: &str, is_apple_terminal: bool, is_shift_pressed: bool) -> String {
    normalize_native_shift_enter_input(data, is_apple_terminal, is_shift_pressed)
}

/// Minimal terminal interface for TUI.
pub trait Terminal: Send + Sync {
    fn start(&mut self, on_input: Arc<dyn Fn(&str) + Send + Sync>);
    fn stop(&mut self);
    fn write(&mut self, data: &str);
    fn columns(&self) -> usize;
    fn rows(&self) -> usize;
    fn kitty_protocol_active(&self) -> bool;
    fn move_by(&mut self, lines: isize);
    fn hide_cursor(&mut self);
    fn show_cursor(&mut self);
    fn clear_line(&mut self);
    fn clear_from_cursor(&mut self);
    fn clear_screen(&mut self);
    fn set_title(&mut self, title: &str);
    fn set_progress(&mut self, active: bool);
}

struct RawMode {
    active: bool,
}

impl RawMode {
    fn enable(&mut self) {
        if self.active {
            return;
        }
        let _ = std::process::Command::new("stty")
            .arg("raw")
            .arg("-echo")
            .status();
        self.active = true;
    }

    fn disable(&mut self) {
        if !self.active {
            return;
        }
        let _ = std::process::Command::new("stty")
            .arg("sane")
            .status();
        self.active = false;
    }
}

/// Real terminal using stdin/stdout.
pub struct ProcessTerminal {
    kitty_protocol_active: AtomicBool,
    modify_other_keys_active: AtomicBool,
    keyboard_protocol_pushed: bool,
    keyboard_protocol_negotiation_buffer: String,
    input_handler: Mutex<Option<Arc<dyn Fn(&str) + Send + Sync>>>,
    raw_mode: Mutex<RawMode>,
    write_log_path: String,
    progress_active: bool,
    stop_requested: Arc<AtomicBool>,
}

impl ProcessTerminal {
    pub fn new() -> Self {
        let write_log_path = std::env::var("PI_TUI_WRITE_LOG").unwrap_or_default();
        Self {
            kitty_protocol_active: AtomicBool::new(false),
            modify_other_keys_active: AtomicBool::new(false),
            keyboard_protocol_pushed: false,
            keyboard_protocol_negotiation_buffer: String::new(),
            input_handler: Mutex::new(None),
            raw_mode: Mutex::new(RawMode { active: false }),
            write_log_path,
            progress_active: false,
            stop_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn modify_other_keys_active(&self) -> bool {
        self.modify_other_keys_active.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    fn handle_negotiation_sequence(
        &mut self,
        negotiation: Option<KeyboardProtocolNegotiationSequence>,
    ) -> bool {
        match negotiation {
            None => false,
            Some(sequence) => {
                self.clear_negotiation_buffer();
                match sequence {
                    KeyboardProtocolNegotiationSequence::KittyFlags { flags } => {
                        if flags != 0 {
                            self.disable_modify_other_keys();
                            if !self.kitty_protocol_active.load(Ordering::SeqCst) {
                                self.kitty_protocol_active.store(true, Ordering::SeqCst);
                                set_kitty_protocol_active(true);
                            }
                        } else {
                            self.enable_modify_other_keys();
                        }
                    }
                    KeyboardProtocolNegotiationSequence::DeviceAttributes => {
                        if !self.kitty_protocol_active.load(Ordering::SeqCst) {
                            self.enable_modify_other_keys();
                        }
                    }
                }
                true
            }
        }
    }


    fn clear_negotiation_buffer(&mut self) {
        self.keyboard_protocol_negotiation_buffer.clear();
    }

    #[allow(dead_code)]
    fn forward_input_sequence(&self, sequence: &str) {
        let handler = self.input_handler.lock().unwrap().clone();
        if let Some(handler) = handler {
            let should_detect_native_shift_enter =
                sequence == "\r" && (is_apple_terminal_session() || cfg!(windows));
            let input = normalize_native_shift_enter_input(
                sequence,
                should_detect_native_shift_enter,
                should_detect_native_shift_enter && false, // native modifier detection stubbed
            );
            handler(&input);
        }
    }

    fn enable_modify_other_keys(&mut self) {
        if self.kitty_protocol_active.load(Ordering::SeqCst) || self.modify_other_keys_active.load(Ordering::SeqCst) {
            return;
        }
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\x1b[>4;2m");
        let _ = stdout.flush();
        self.modify_other_keys_active.store(true, Ordering::SeqCst);
    }

    fn disable_modify_other_keys(&mut self) {
        if !self.modify_other_keys_active.load(Ordering::SeqCst) {
            return;
        }
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\x1b[>4;0m");
        let _ = stdout.flush();
        self.modify_other_keys_active.store(false, Ordering::SeqCst);
    }

}

impl Terminal for ProcessTerminal {
    fn start(&mut self, on_input: Arc<dyn Fn(&str) + Send + Sync>) {
        *self.input_handler.lock().unwrap() = Some(on_input);
        self.raw_mode.lock().unwrap().enable();

        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\x1b[?2004h"); // bracketed paste
        let _ = stdout.flush();

        // Query Kitty protocol.
        self.keyboard_protocol_pushed = true;
        let _ = stdout.write_all(KITTY_KEYBOARD_PROTOCOL_QUERY.as_bytes());
        let _ = stdout.flush();

        // Reader thread.
        let stop = self.stop_requested.clone();
        let input_handler = Arc::new(Mutex::new(self.input_handler.lock().unwrap().clone()));
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
            let mut chunk = [0u8; 4096];
            while !stop.load(Ordering::SeqCst) {
                match stdin.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        let events = buffer.process(&chunk[..count]);
                        for event in events {
                            match event {
                                StdinEvent::Data(sequence) => {
                                    // Kitty negotiation detection.
                                    if sequence.starts_with("\x1b[") {
                                        if let Some(negotiation) =
                                            parse_keyboard_protocol_negotiation_sequence(&sequence)
                                        {
                                            if let KeyboardProtocolNegotiationSequence::KittyFlags { flags } = negotiation {
                                                if flags != 0 {
                                                    set_kitty_protocol_active(true);
                                                }
                                            }
                                            continue;
                                        }
                                    }
                                    if let Some(handler) = input_handler.lock().unwrap().clone() {
                                        handler(&sequence);
                                    }
                                }
                                StdinEvent::Paste(content) => {
                                    if let Some(handler) = input_handler.lock().unwrap().clone() {
                                        handler(&format!("\x1b[200~{content}\x1b[201~"));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    fn stop(&mut self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\x1b[?2004l");
        if self.progress_active {
            let _ = stdout.write_all(TERMINAL_PROGRESS_CLEAR_SEQUENCE.as_bytes());
        }
        if self.keyboard_protocol_pushed || self.kitty_protocol_active.load(Ordering::SeqCst) {
            let _ = stdout.write_all(b"\x1b[<u");
            set_kitty_protocol_active(false);
        }
        let _ = stdout.flush();
        self.raw_mode.lock().unwrap().disable();
        *self.input_handler.lock().unwrap() = None;
    }

    fn write(&mut self, data: &str) {
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(data.as_bytes());
        let _ = stdout.flush();
        if !self.write_log_path.is_empty() {
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&self.write_log_path) {
                let _ = file.write_all(data.as_bytes());
            }
        }
    }

    fn columns(&self) -> usize {
        if let Ok(columns) = std::env::var("COLUMNS") {
            if let Ok(columns) = columns.parse() {
                return columns;
            }
        }
        80
    }

    fn rows(&self) -> usize {
        if let Ok(rows) = std::env::var("LINES") {
            if let Ok(rows) = rows.parse() {
                return rows;
            }
        }
        24
    }

    fn kitty_protocol_active(&self) -> bool {
        self.kitty_protocol_active.load(Ordering::SeqCst)
    }

    fn move_by(&mut self, lines: isize) {
        let mut stdout = std::io::stdout();
        if lines > 0 {
            let _ = write!(stdout, "\x1b[{lines}B");
        } else if lines < 0 {
            let _ = write!(stdout, "\x1b[{}A", -lines);
        }
        let _ = stdout.flush();
    }

    fn hide_cursor(&mut self) {
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\x1b[?25l");
        let _ = stdout.flush();
    }

    fn show_cursor(&mut self) {
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\x1b[?25h");
        let _ = stdout.flush();
    }

    fn clear_line(&mut self) {
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\x1b[K");
        let _ = stdout.flush();
    }

    fn clear_from_cursor(&mut self) {
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\x1b[J");
        let _ = stdout.flush();
    }

    fn clear_screen(&mut self) {
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\x1b[2J\x1b[H");
        let _ = stdout.flush();
    }

    fn set_title(&mut self, title: &str) {
        let mut stdout = std::io::stdout();
        let _ = write!(stdout, "\x1b]0;{title}\x07");
        let _ = stdout.flush();
    }

    fn set_progress(&mut self, active: bool) {
        let mut stdout = std::io::stdout();
        if active {
            let _ = stdout.write_all(TERMINAL_PROGRESS_ACTIVE_SEQUENCE.as_bytes());
            self.progress_active = true;
        } else {
            let _ = stdout.write_all(TERMINAL_PROGRESS_CLEAR_SEQUENCE.as_bytes());
            self.progress_active = false;
        }
        let _ = stdout.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_negotiation_sequences() {
        assert_eq!(
            parse_keyboard_protocol_negotiation_sequence("\x1b[?7u"),
            Some(KeyboardProtocolNegotiationSequence::KittyFlags { flags: 7 })
        );
        assert_eq!(
            parse_keyboard_protocol_negotiation_sequence("\x1b[?0u"),
            Some(KeyboardProtocolNegotiationSequence::KittyFlags { flags: 0 })
        );
        assert_eq!(
            parse_keyboard_protocol_negotiation_sequence("\x1b[?1;2c"),
            Some(KeyboardProtocolNegotiationSequence::DeviceAttributes)
        );
        assert_eq!(parse_keyboard_protocol_negotiation_sequence("plain"), None);
    }

    #[test]
    fn detects_prefixes() {
        assert!(is_keyboard_protocol_negotiation_sequence_prefix("\x1b["));
        assert!(is_keyboard_protocol_negotiation_sequence_prefix("\x1b[?7"));
        assert!(!is_keyboard_protocol_negotiation_sequence_prefix("abc"));
    }

    #[test]
    fn normalizes_shift_enter() {
        assert_eq!(
            normalize_native_shift_enter_input("\r", true, true),
            "\x1b[13;2u"
        );
        assert_eq!(normalize_native_shift_enter_input("\r", true, false), "\r");
        assert_eq!(normalize_native_shift_enter_input("a", true, true), "a");
    }

    #[test]
    fn apple_terminal_detection() {
        // Without TERM_PROGRAM=Apple_Terminal it is false.
        std::env::remove_var("TERM_PROGRAM");
        assert!(!is_apple_terminal_session());
    }
}
