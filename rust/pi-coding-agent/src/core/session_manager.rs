//! SessionManager, port of `core/session-manager.ts` (class + file I/O).
//!
//! Sessions are append-only trees stored in JSONL files. The manager keeps
//! the full entry list, an id index, resolved labels, and a leaf pointer;
//! appends create children of the leaf, branching moves the leaf.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use crate::core::messages::{parse_timestamp_ms, ContentOrText};
use pi_ai::types::Usage;
use pi_protocol::Value;
use crate::core::session_paths::{join, normalize_path, resolve_path};
use crate::core::session_types::*;

/// Max bytes scanned while discovering a session header (bounded sync scan).
const MAX_SESSION_HEADER_SCAN_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub struct SessionHeaderScanLimitError {
    pub file_path: String,
}

impl std::fmt::Display for SessionHeaderScanLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Session header exceeds {}-byte scan limit: {}",
            MAX_SESSION_HEADER_SCAN_BYTES, self.file_path
        )
    }
}

impl std::error::Error for SessionHeaderScanLimitError {}

pub struct NewSessionOptions {
    pub id: Option<String>,
    pub parent_session: Option<String>,
}

pub type SessionListProgress = Box<dyn Fn(i64, i64) + Send + Sync>;

/// Current wall-clock time as an ISO-8601 UTC string (`new Date().toISOString()`).
pub fn now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before unix epoch");
    let millis = now.as_millis() as u64;
    let seconds = millis / 1000;
    let ms = millis % 1000;
    let days = seconds / 86_400;
    let secs_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z"
    )
}

/// Inverse of days_from_civil (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Default session dir for a cwd: `<agentDir>/sessions/--<encoded-cwd>--`.
fn get_default_session_dir_path(cwd: &str) -> String {
    let resolved_cwd = resolve_path(cwd, None);
    let agent_dir = crate::config::get_agent_dir();
    let trimmed = resolved_cwd.trim_start_matches(['/', '\\']);
    let safe_path = format!(
        "--{}--",
        trimmed
            .replace(['/', '\\', ':'], "-")
    );
    join(&join(&agent_dir, "sessions"), &safe_path)
}

pub fn get_default_session_dir(cwd: &str) -> String {
    let session_dir = get_default_session_dir_path(cwd);
    if !Path::new(&session_dir).exists() {
        let _ = fs::create_dir_all(&session_dir);
    }
    session_dir
}

/// Parse a physical line into a FileEntry; blank/malformed lines yield None.
fn parse_session_entry_line(line: &str) -> Option<FileEntry> {
    if line.trim().is_empty() {
        return None;
    }
    parse_entry_json(line)
}

/// Read all entries from a session file; [] when missing or when the first
/// entry is not a valid session header.
pub fn load_entries_from_file(file_path: &str) -> Vec<FileEntry> {
    let resolved = normalize_path(file_path);
    let mut file = match File::open(&resolved) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let mut content = String::new();
    if file.read_to_string(&mut content).is_err() {
        return Vec::new();
    }
    let entries: Vec<FileEntry> = content
        .lines()
        .filter_map(parse_session_entry_line)
        .collect();

    if entries.is_empty() {
        return entries;
    }
    match &entries[0] {
        FileEntry::Header(header) if !header.id.is_empty() => entries,
        _ => Vec::new(),
    }
}

/// Best-effort header discovery: parse the first valid JSON line within the
/// scan limit. Returns None while scanning, Some(null) for a parsed
/// non-header, or the header. Mirrors parseSessionHeaderCandidate.
fn parse_session_header_candidate(line: &str) -> Option<Option<SessionHeader>> {
    if line.trim().is_empty() {
        return None;
    }
    match parse_entry_json(line) {
        None => None,
        Some(FileEntry::Header(header)) => Some(Some(header)),
        Some(_) => Some(None),
    }
}

/// Read just the session header. Throws SessionHeaderScanLimitError when the
/// first parseable line does not appear within the bounded scan.
pub fn read_session_header(file_path: &str) -> Result<Option<SessionHeader>, SessionHeaderScanLimitError> {
    let mut file = File::open(file_path).map_err(|_| SessionHeaderScanLimitError {
        file_path: file_path.to_string(),
    })?;
    // First 1MB: split into lines, parse each candidate, joining continuation
    // lines (a header could span buffers).
    let mut buf = [0u8; MAX_SESSION_HEADER_SCAN_BYTES + 64];
    let read = file
        .read(&mut buf)
        .map_err(|_| SessionHeaderScanLimitError {
            file_path: file_path.to_string(),
        })?;
    let text = String::from_utf8_lossy(&buf[..read]).to_string();
    let mut pending = String::new();
    for line in text.split('\n') {
        if pending.is_empty() {
            pending = line.to_string();
        } else {
            pending.push('\n');
            pending.push_str(line);
        }
        match parse_session_header_candidate(&pending) {
            Some(Some(header)) => return Ok(Some(header)),
            Some(None) => return Ok(None),
            None => {
                // Keep scanning: blank/malformed lines are skipped by the JS
                // reader, but a malformed first line means "not a session"
                // once a subsequent line parses. The JS implementation joins
                // physical lines only within one buffer chunk; approximate
                // by resetting pending for non-blank parses.
                if !line.trim().is_empty() && parse_entry_json(line).is_none() {
                    pending.clear();
                }
            }
        }
    }
    // Probe EOF: a header without a trailing newline within the scan limit.
    let mut probe = [0u8; 1];
    if file.read(&mut probe).map_err(|_| SessionHeaderScanLimitError {
        file_path: file_path.to_string(),
    })? == 0
    {
        return Ok(parse_session_header_candidate(&pending).flatten());
    }
    Err(SessionHeaderScanLimitError {
        file_path: file_path.to_string(),
    })
}

fn read_session_header_for_discovery(file_path: &str) -> Option<SessionHeader> {
    read_session_header(file_path).ok().flatten()
}

fn get_session_header_cwd(header: &SessionHeader) -> Option<String> {
    if header.cwd.is_empty() {
        None
    } else {
        Some(header.cwd.clone())
    }
}

fn session_cwd_matches(cwd: Option<&str>, resolved_cwd: &str) -> bool {
    match cwd {
        Some(cwd) if !cwd.is_empty() => resolve_path(cwd, None) == resolved_cwd,
        _ => false,
    }
}

/// Most recently modified session file in a dir, optionally filtered by cwd.
pub fn find_most_recent_session(session_dir: &str, cwd: Option<&str>) -> Option<String> {
    let resolved_session_dir = normalize_path(session_dir);
    let resolved_cwd = cwd.map(|cwd| resolve_path(cwd, None));
    let dir = match fs::read_dir(&resolved_session_dir) {
        Ok(dir) => dir,
        Err(_) => return None,
    };
    let mut candidates: Vec<(String, std::time::SystemTime)> = Vec::new();
    for entry in dir.flatten() {
        let path = entry.path();
        let name = path.to_string_lossy().to_string();
        if !name.ends_with(".jsonl") {
            continue;
        }
        let header = read_session_header_for_discovery(&name);
        let Some(header) = header else { continue };
        if let Some(resolved_cwd) = &resolved_cwd {
            if !session_cwd_matches(get_session_header_cwd(&header).as_deref(), resolved_cwd) {
                continue;
            }
        }
        let mtime = entry.metadata().ok().and_then(|meta| meta.modified().ok());
        candidates.push((name, mtime.unwrap_or(std::time::UNIX_EPOCH)));
    }
    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    candidates.first().map(|(path, _)| path.clone())
}

fn is_message_with_content(message: &SessionMessage) -> bool {
    matches!(
        message,
        SessionMessage::Llm(pi_ai::types::Message::User(_)) | SessionMessage::Llm(pi_ai::types::Message::Assistant(_))
    )
}

/// Role of an Llm message as a string ("" for custom/unknown).
fn message_role(message: &SessionMessage) -> &'static str {
    match message {
        SessionMessage::Llm(pi_ai::types::Message::User(_)) => "user",
        SessionMessage::Llm(pi_ai::types::Message::Assistant(_)) => "assistant",
        SessionMessage::Llm(pi_ai::types::Message::ToolResult(_)) => "toolResult",
        _ => "",
    }
}

fn extract_text_content(message: &SessionMessage) -> String {
    let content = match message {
        SessionMessage::Llm(pi_ai::types::Message::User(user)) => match &user.content {
            pi_ai::types::UserMessageContent::Text(text) => text.clone(),
            pi_ai::types::UserMessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    pi_ai::types::Content::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        },
        SessionMessage::Llm(pi_ai::types::Message::Assistant(assistant)) => assistant
            .content
            .iter()
            .filter_map(|block| match block {
                pi_ai::types::Content::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    };
    content
}

fn get_message_activity_time(entry: &SessionEntry) -> Option<f64> {
    let (message, entry_timestamp) = match entry {
        SessionEntry::Message { message, base, .. } => (message, &base.timestamp),
        _ => return None,
    };
    if !is_message_with_content(message) {
        return None;
    }
    let role = message_role(message);
    if role != "user" && role != "assistant" {
        return None;
    }
    let msg_timestamp = match message {
        SessionMessage::Llm(pi_ai::types::Message::User(user)) => user.timestamp,
        SessionMessage::Llm(pi_ai::types::Message::Assistant(assistant)) => assistant.timestamp,
        _ => f64::NAN,
    };
    if !msg_timestamp.is_nan() {
        return Some(msg_timestamp);
    }
    let t = parse_timestamp_ms(entry_timestamp);
    if t.is_nan() {
        None
    } else {
        Some(t)
    }
}

fn build_session_info(file_path: &str) -> Option<SessionInfo> {
    let stats = fs::metadata(file_path).ok()?;
    let content = fs::read_to_string(file_path).ok()?;
    let mut header: Option<SessionHeader> = None;
    let mut message_count = 0i64;
    let mut first_message = String::new();
    let mut all_messages: Vec<String> = Vec::new();
    let mut name: Option<String> = None;
    let mut last_activity_time: Option<f64> = None;

    for line in content.lines() {
        let Some(entry) = parse_session_entry_line(line) else {
            continue;
        };
        let entry = match entry {
            FileEntry::Header(parsed_header) => {
                if header.is_none() {
                    header = Some(parsed_header);
                }
                continue;
            }
            FileEntry::Entry(entry) => entry,
        };
        if header.is_none() {
            // First parsed entry was not a header: not a session file.
            return None;
        }
        if let SessionEntry::SessionInfo { name: entry_name, .. } = &entry {
            name = entry_name.as_deref().map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
        }
        if !matches!(entry, SessionEntry::Message { .. }) {
            continue;
        }
        message_count += 1;
        if let Some(activity_time) = get_message_activity_time(&entry) {
            last_activity_time = Some(last_activity_time.map_or(activity_time, |t| t.max(activity_time)));
        }
        let message = match &entry {
            SessionEntry::Message { message, .. } => message,
            _ => continue,
        };
        if !is_message_with_content(message) {
            continue;
        }
        let role = message_role(message);
        if role != "user" && role != "assistant" {
            continue;
        }
        let text_content = extract_text_content(message);
        if text_content.is_empty() {
            continue;
        }
        all_messages.push(text_content.clone());
        if first_message.is_empty() && role == "user" {
            first_message = text_content;
        }
    }

    let header = header?;
    let cwd = if header.cwd.is_empty() { String::new() } else { header.cwd.clone() };
    let parent_session_path = header.parent_session.clone();
    let header_time = parse_timestamp_ms(&header.timestamp);
    let modified_ms = match last_activity_time {
        Some(t) if t > 0.0 => t,
        _ if !header_time.is_nan() => header_time,
        _ => stats
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as f64)
            .unwrap_or(0.0),
    };

    Some(SessionInfo {
        path: file_path.to_string(),
        id: header.id.clone(),
        cwd,
        name,
        parent_session_path,
        created_ms: header_time,
        modified_ms,
        message_count,
        first_message: if first_message.is_empty() {
            "(no messages)".to_string()
        } else {
            first_message
        },
        all_messages_text: all_messages.join(" "),
    })
}

fn list_sessions_from_dir(dir: &str, on_progress: Option<&SessionListProgress>) -> Vec<SessionInfo> {
    let mut sessions = Vec::new();
    let dir_entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return sessions,
    };
    let files: Vec<String> = dir_entries
        .flatten()
        .map(|entry| entry.path().to_string_lossy().to_string())
        .filter(|path| path.ends_with(".jsonl"))
        .collect();
    let total = files.len() as i64;
    for (index, file) in files.iter().enumerate() {
        if let Some(info) = build_session_info(file) {
            sessions.push(info);
        }
        if let Some(progress) = on_progress {
            progress(index as i64 + 1, total);
        }
    }
    sessions
}

// ---------------------------------------------------------------------------
// SessionManager
// ---------------------------------------------------------------------------

pub struct SessionManager {
    session_id: String,
    session_file: Option<String>,
    session_dir: String,
    cwd: String,
    persist: bool,
    flushed: bool,
    file_entries: Vec<FileEntry>,
    by_id: HashMap<String, SessionEntry>,
    labels_by_id: HashMap<String, String>,
    label_timestamps_by_id: HashMap<String, String>,
    leaf_id: Option<String>,
}

impl SessionManager {
    fn new(
        cwd: &str,
        session_dir: &str,
        session_file: Option<String>,
        persist: bool,
        options: Option<NewSessionOptions>,
        preloaded_file_entries: Option<Vec<FileEntry>>,
    ) -> Self {
        let mut manager = Self {
            session_id: String::new(),
            session_file: None,
            session_dir: normalize_path(session_dir),
            cwd: resolve_path(cwd, None),
            persist,
            flushed: false,
            file_entries: Vec::new(),
            by_id: HashMap::new(),
            labels_by_id: HashMap::new(),
            label_timestamps_by_id: HashMap::new(),
            leaf_id: None,
        };
        if persist && !manager.session_dir.is_empty() && !Path::new(&manager.session_dir).exists() {
            let _ = fs::create_dir_all(&manager.session_dir);
        }
        match session_file {
            Some(file) => manager.set_session_file_internal(&file, preloaded_file_entries),
            None => {
                manager.new_session(options);
            }
        }
        manager
    }

    pub fn get_cwd(&self) -> &str {
        &self.cwd
    }

    pub fn get_session_dir(&self) -> &str {
        &self.session_dir
    }

    pub fn uses_default_session_dir(&self) -> bool {
        self.session_dir == get_default_session_dir_path(&self.cwd)
    }

    pub fn get_session_id(&self) -> &str {
        &self.session_id
    }

    pub fn get_session_file(&self) -> Option<&str> {
        self.session_file.as_deref()
    }

    pub fn is_persisted(&self) -> bool {
        self.persist
    }

    /// Switch to a different session file (used for resume and branching).
    pub fn set_session_file(&mut self, session_file: &str) {
        self.set_session_file_internal(session_file, None);
    }

    fn set_session_file_internal(&mut self, session_file: &str, preloaded: Option<Vec<FileEntry>>) {
        self.session_file = Some(resolve_path(session_file, None));
        let file = self.session_file.clone().unwrap();
        if Path::new(&file).exists() {
            self.file_entries = preloaded.unwrap_or_else(|| load_entries_from_file(&file));

            if self.file_entries.is_empty() {
                let explicit_path = self.session_file.clone().unwrap();
                if fs::metadata(&explicit_path).map(|m| m.len() > 0).unwrap_or(false) {
                    panic!("Session file is not a valid pi session: {explicit_path}");
                }
                self.new_session(None);
                self.session_file = Some(explicit_path);
                self.rewrite_file();
                self.flushed = true;
                return;
            }

            let header = self
                .file_entries
                .iter()
                .find_map(|entry| match entry {
                    FileEntry::Header(header) => Some(header),
                    _ => None,
                });
            self.session_id = header.map(|h| h.id.clone()).unwrap_or_else(create_session_id);

            if migrate_session_entries(&mut self.file_entries) {
                self.rewrite_file();
            }

            self.build_index();
            self.flushed = true;
        } else {
            let explicit_path = self.session_file.clone().unwrap();
            self.new_session(None);
            self.session_file = Some(explicit_path); // preserve explicit path from --session flag
        }
    }

    /// Start a new session; returns the new session file path when persisting.
    pub fn new_session(&mut self, options: Option<NewSessionOptions>) -> Option<String> {
        if let Some(id) = options.as_ref().and_then(|o| o.id.as_deref()) {
            assert_valid_session_id(id).expect("invalid session id");
        }
        self.session_id = options
            .as_ref()
            .and_then(|o| o.id.clone())
            .unwrap_or_else(create_session_id);
        let timestamp = now_iso();
        let header = SessionHeader {
            version: Some(CURRENT_SESSION_VERSION),
            id: self.session_id.clone(),
            timestamp: timestamp.clone(),
            cwd: self.cwd.clone(),
            parent_session: options.as_ref().and_then(|o| o.parent_session.clone()),
        };
        self.file_entries = vec![FileEntry::Header(header)];
        self.by_id.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;
        self.flushed = false;

        if self.persist {
            let file_timestamp = timestamp.replace([':', '.'], "-");
            self.session_file = Some(join(
                self.get_session_dir(),
                &format!("{file_timestamp}_{}.jsonl", self.session_id),
            ));
        }
        self.session_file.clone()
    }

    fn build_index(&mut self) {
        self.by_id.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;
        for entry in &self.file_entries {
            let FileEntry::Entry(entry) = entry else { continue };
            self.by_id.insert(entry.id().to_string(), entry.clone());
            self.leaf_id = Some(entry.id().to_string());
            if let SessionEntry::Label { target_id, label, base, .. } = entry {
                match label {
                    Some(label) => {
                        self.labels_by_id.insert(target_id.clone(), label.clone());
                        self.label_timestamps_by_id.insert(target_id.clone(), base.timestamp.clone());
                    }
                    None => {
                        self.labels_by_id.remove(target_id);
                        self.label_timestamps_by_id.remove(target_id);
                    }
                }
            }
        }
    }

    fn rewrite_file(&self) {
        if !self.persist || self.session_file.is_none() {
            return;
        }
        let file = self.session_file.as_deref().unwrap();
        let mut fd = match File::create(file) {
            Ok(fd) => fd,
            Err(error) => panic!("failed to open session file for writing: {error}"),
        };
        for entry in &self.file_entries {
            let _ = writeln!(fd, "{}", crate::core::session_types::file_entry_to_json_string(entry));
        }
    }

    fn persist_entry(&mut self, entry: &SessionEntry) {
        if !self.persist || self.session_file.is_none() {
            return;
        }
        let file = self.session_file.clone().unwrap();
        let has_assistant = self.file_entries.iter().any(|entry| {
            matches!(
                entry,
                FileEntry::Entry(SessionEntry::Message { message: SessionMessage::Llm(pi_ai::types::Message::Assistant(_)), .. })
            )
        });
        if !has_assistant {
            if self.flushed {
                let mut fd = OpenOptions::new().append(true).open(&file).expect("append session");
                let _ = writeln!(
                    fd,
                    "{}",
                    crate::core::session_types::file_entry_to_json_string(&FileEntry::Entry(entry.clone()))
                );
            } else {
                // Mark as not flushed so when assistant arrives, all entries get written.
                self.flushed = false;
            }
            return;
        }

        if !self.flushed {
            let mut fd = OpenOptions::new().write(true).create_new(true).open(&file).expect("create session");
            for entry in &self.file_entries {
                let _ = writeln!(fd, "{}", crate::core::session_types::file_entry_to_json_string(entry));
            }
            self.flushed = true;
        } else {
            let mut fd = OpenOptions::new().append(true).open(&file).expect("append session");
            let _ = writeln!(fd, "{}", crate::core::session_types::file_entry_to_json_string(&FileEntry::Entry(entry.clone())));
        }
    }

    fn append_entry(&mut self, entry: SessionEntry) -> String {
        let id = entry.id().to_string();
        self.file_entries.push(FileEntry::Entry(entry.clone()));
        self.by_id.insert(id.clone(), entry.clone());
        self.leaf_id = Some(id.clone());
        self.persist_entry(&entry);
        id
    }

    // -----------------------------------------------------------------------
    // Appenders
    // -----------------------------------------------------------------------

    pub fn append_message(&mut self, message: SessionMessage) -> String {
        let entry = SessionEntry::Message {
            base: SessionEntryBase {
                id: generate_id(&|candidate| self.by_id.contains_key(candidate)),
                parent_id: self.leaf_id.clone(),
                timestamp: now_iso(),
            },
            message,
        };
        self.append_entry(entry)
    }

    pub fn append_thinking_level_change(&mut self, thinking_level: String) -> String {
        let entry = SessionEntry::ThinkingLevelChange {
            base: SessionEntryBase {
                id: generate_id(&|candidate| self.by_id.contains_key(candidate)),
                parent_id: self.leaf_id.clone(),
                timestamp: now_iso(),
            },
            thinking_level,
        };
        self.append_entry(entry)
    }

    pub fn append_model_change(&mut self, provider: String, model_id: String) -> String {
        let entry = SessionEntry::ModelChange {
            base: SessionEntryBase {
                id: generate_id(&|candidate| self.by_id.contains_key(candidate)),
                parent_id: self.leaf_id.clone(),
                timestamp: now_iso(),
            },
            provider,
            model_id,
        };
        self.append_entry(entry)
    }

    pub fn append_compaction(
        &mut self,
        summary: String,
        first_kept_entry_id: String,
        tokens_before: f64,
        details: Option<Value>,
        from_hook: Option<bool>,
        usage: Option<Usage>,
    ) -> String {
        let entry = SessionEntry::Compaction {
            base: SessionEntryBase {
                id: generate_id(&|candidate| self.by_id.contains_key(candidate)),
                parent_id: self.leaf_id.clone(),
                timestamp: now_iso(),
            },
            summary,
            first_kept_entry_id,
            tokens_before,
            details,
            usage,
            from_hook,
            first_kept_entry_index: None,
        };
        self.append_entry(entry)
    }

    pub fn append_custom_entry(&mut self, custom_type: String, data: Option<Value>) -> String {
        let entry = SessionEntry::Custom {
            base: SessionEntryBase {
                id: generate_id(&|candidate| self.by_id.contains_key(candidate)),
                parent_id: self.leaf_id.clone(),
                timestamp: now_iso(),
            },
            custom_type,
            data,
        };
        self.append_entry(entry)
    }

    pub fn append_session_info(&mut self, name: String) -> String {
        let sanitized = name
            .split(['\r', '\n'])
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();
        let entry = SessionEntry::SessionInfo {
            base: SessionEntryBase {
                id: generate_id(&|candidate| self.by_id.contains_key(candidate)),
                parent_id: self.leaf_id.clone(),
                timestamp: now_iso(),
            },
            name: Some(sanitized),
        };
        self.append_entry(entry)
    }

    pub fn get_session_name(&self) -> Option<String> {
        let entries = self.get_entries();
        for entry in entries.iter().rev() {
            if let SessionEntry::SessionInfo { name, .. } = entry {
                return name.as_deref().map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
            }
        }
        None
    }

    pub fn append_custom_message_entry(
        &mut self,
        custom_type: String,
        content: ContentOrText,
        display: bool,
        details: Option<Value>,
    ) -> String {
        let entry = SessionEntry::CustomMessage {
            base: SessionEntryBase {
                id: generate_id(&|candidate| self.by_id.contains_key(candidate)),
                parent_id: self.leaf_id.clone(),
                timestamp: now_iso(),
            },
            custom_type,
            content,
            details,
            display,
        };
        self.append_entry(entry)
    }

    // -----------------------------------------------------------------------
    // Traversal
    // -----------------------------------------------------------------------

    pub fn get_leaf_id(&self) -> Option<String> {
        self.leaf_id.clone()
    }

    pub fn get_leaf_entry(&self) -> Option<SessionEntry> {
        self.leaf_id.as_ref().and_then(|id| self.by_id.get(id).cloned())
    }

    pub fn get_entry(&self, id: &str) -> Option<SessionEntry> {
        self.by_id.get(id).cloned()
    }

    pub fn get_children(&self, parent_id: &str) -> Vec<SessionEntry> {
        self.by_id
            .values()
            .filter(|entry| entry.parent_id() == Some(parent_id))
            .cloned()
            .collect()
    }

    pub fn get_label(&self, id: &str) -> Option<String> {
        self.labels_by_id.get(id).cloned()
    }

    pub fn append_label_change(&mut self, target_id: String, label: Option<String>) -> String {
        if !self.by_id.contains_key(&target_id) {
            panic!("Entry {target_id} not found");
        }
        let timestamp = now_iso();
        let entry = SessionEntry::Label {
            base: SessionEntryBase {
                id: generate_id(&|candidate| self.by_id.contains_key(candidate)),
                parent_id: self.leaf_id.clone(),
                timestamp: timestamp.clone(),
            },
            target_id: target_id.clone(),
            label: label.clone(),
        };
        let entry_id = entry.id().to_string();
        self.append_entry(entry);
        match label {
            Some(label) => {
                self.labels_by_id.insert(target_id.clone(), label);
                self.label_timestamps_by_id.insert(target_id, timestamp);
            }
            None => {
                self.labels_by_id.remove(&target_id);
                self.label_timestamps_by_id.remove(&target_id);
            }
        }
        entry_id
    }

    /// Walk from an entry to root, returning all entries in path order.
    pub fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry> {
        let mut path: Vec<SessionEntry> = Vec::new();
        let start_id = from_id.or(self.leaf_id.as_deref());
        let mut current = start_id.and_then(|id| self.by_id.get(id));
        while let Some(entry) = current {
            path.push(entry.clone());
            current = entry.parent_id().and_then(|parent| self.by_id.get(parent));
        }
        path.reverse();
        path
    }

    pub fn build_context_entries(&self) -> Vec<SessionEntry> {
        let entries = self.get_entries();
        build_context_entries(&entries, self.leaf_id.as_deref(), &build_entry_index(&entries))
            .into_iter()
            .cloned()
            .collect()
    }

    pub fn build_session_context(&self) -> SessionContext {
        let entries = self.get_entries();
        build_session_context(&entries, self.leaf_id.as_deref(), &build_entry_index(&entries))
    }

    pub fn get_header(&self) -> Option<SessionHeader> {
        self.file_entries
            .iter()
            .find_map(|entry| match entry {
                FileEntry::Header(header) => Some(header.clone()),
                _ => None,
            })
    }

    /// All session entries (excludes header); shallow copy.
    pub fn get_entries(&self) -> Vec<SessionEntry> {
        self.file_entries
            .iter()
            .filter_map(|entry| match entry {
                FileEntry::Entry(entry) => Some(entry.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn get_tree(&self) -> Vec<SessionTreeNode> {
        let entries = self.get_entries();
        // Defensive copy with resolved labels; children stored by id first to
        // avoid shared-mutation aliasing.
        let mut children_by_id: HashMap<String, Vec<String>> = HashMap::new();
        let mut node_map: HashMap<String, SessionTreeNode> = HashMap::new();
        let mut roots: Vec<String> = Vec::new();

        for entry in &entries {
            node_map.insert(
                entry.id().to_string(),
                SessionTreeNode {
                    entry: entry.clone(),
                    children: Vec::new(),
                    label: self.labels_by_id.get(entry.id()).cloned(),
                    label_timestamp: self.label_timestamps_by_id.get(entry.id()).cloned(),
                },
            );
        }
        for entry in &entries {
            let parent_id = entry.parent_id();
            if parent_id.is_none() || parent_id == Some(entry.id()) {
                roots.push(entry.id().to_string());
            } else if node_map.contains_key(parent_id.unwrap()) {
                children_by_id
                    .entry(parent_id.unwrap().to_string())
                    .or_default()
                    .push(entry.id().to_string());
            } else {
                // Orphan - treat as root.
                roots.push(entry.id().to_string());
            }
        }

        // Sort children by timestamp (oldest first, newest at bottom).
        // ponytail: iterative via explicit stack like JS; recursion depth
        // mirrors tree depth and could overflow on pathological files.
        fn attach(
            id: &str,
            node_map: &HashMap<String, SessionTreeNode>,
            children_by_id: &HashMap<String, Vec<String>>,
        ) -> SessionTreeNode {
            let mut node = node_map.get(id).unwrap().clone();
            let mut children = children_by_id.get(id).cloned().unwrap_or_default();
            children.sort_by(|a, b| {
                let ta = node_map.get(a).map(|n| parse_timestamp_ms(n.entry.timestamp())).unwrap_or(f64::NAN);
                let tb = node_map.get(b).map(|n| parse_timestamp_ms(n.entry.timestamp())).unwrap_or(f64::NAN);
                ta.partial_cmp(&tb).unwrap_or(std::cmp::Ordering::Equal)
            });
            node.children = children.iter().map(|child| attach(child, node_map, children_by_id)).collect();
            node
        }

        roots
            .iter()
            .filter(|root| node_map.contains_key(*root))
            .map(|root| attach(root, &node_map, &children_by_id))
            .collect()
    }

    // -----------------------------------------------------------------------
    // Branching
    // -----------------------------------------------------------------------

    pub fn branch(&mut self, branch_from_id: &str) {
        if !self.by_id.contains_key(branch_from_id) {
            panic!("Entry {branch_from_id} not found");
        }
        self.leaf_id = Some(branch_from_id.to_string());
    }

    pub fn reset_leaf(&mut self) {
        self.leaf_id = None;
    }

    pub fn branch_with_summary(
        &mut self,
        branch_from_id: Option<String>,
        summary: String,
        details: Option<Value>,
        from_hook: Option<bool>,
        usage: Option<Usage>,
    ) -> String {
        if let Some(branch_from_id) = &branch_from_id {
            if !self.by_id.contains_key(branch_from_id) {
                panic!("Entry {branch_from_id} not found");
            }
        }
        self.leaf_id = branch_from_id.clone();
        let entry = SessionEntry::BranchSummary {
            base: SessionEntryBase {
                id: generate_id(&|candidate| self.by_id.contains_key(candidate)),
                parent_id: branch_from_id.clone(),
                timestamp: now_iso(),
            },
            from_id: branch_from_id.unwrap_or_else(|| "root".to_string()),
            summary,
            details,
            usage,
            from_hook,
        };
        self.append_entry(entry)
    }

    pub fn create_branched_session(&mut self, leaf_id: &str) -> Option<String> {
        let previous_session_file = self.session_file.clone();
        let path = self.get_branch(Some(leaf_id));
        if path.is_empty() {
            panic!("Entry {leaf_id} not found");
        }

        // Filter out LabelEntry from the path; re-chain retained entries.
        let mut path_without_labels: Vec<SessionEntry> = Vec::new();
        let mut path_parent_id: Option<String> = None;
        for entry in &path {
            if matches!(entry, SessionEntry::Label { .. }) {
                continue;
            }
            path_without_labels.push(entry.with_parent(path_parent_id.clone()));
            path_parent_id = Some(entry.id().to_string());
        }

        let new_session_id = create_session_id();
        let timestamp = now_iso();
        let file_timestamp = timestamp.replace([':', '.'], "-");
        let new_session_file = join(
            self.get_session_dir(),
            &format!("{file_timestamp}_{new_session_id}.jsonl"),
        );

        let header = SessionHeader {
            version: Some(CURRENT_SESSION_VERSION),
            id: new_session_id.clone(),
            timestamp: timestamp.clone(),
            cwd: self.cwd.clone(),
            parent_session: if self.persist {
                previous_session_file
            } else {
                None
            },
        };

        // Collect labels for entries in the path.
        let path_entry_ids: HashSet<String> = path_without_labels.iter().map(|e| e.id().to_string()).collect();
        let mut labels_to_write: Vec<(String, String, String)> = Vec::new();
        for (target_id, label) in &self.labels_by_id {
            if path_entry_ids.contains(target_id) {
                if let Some(label_timestamp) = self.label_timestamps_by_id.get(target_id) {
                    labels_to_write.push((target_id.clone(), label.clone(), label_timestamp.clone()));
                }
            }
        }

        if self.persist {
            let last_entry_id = path_without_labels.last().map(|e| e.id().to_string()).unwrap_or_default();
            let mut label_parent_id: Option<String> = if last_entry_id.is_empty() {
                None
            } else {
                Some(last_entry_id)
            };
            let mut label_entries: Vec<SessionEntry> = Vec::new();
            let mut used_ids: HashSet<String> = path_entry_ids.clone();
            for (target_id, label, label_timestamp) in &labels_to_write {
                let label_entry = SessionEntry::Label {
                    base: SessionEntryBase {
                        id: generate_id(&|candidate| used_ids.contains(candidate)),
                        parent_id: label_parent_id.clone(),
                        timestamp: label_timestamp.clone(),
                    },
                    target_id: target_id.clone(),
                    label: Some(label.clone()),
                };
                used_ids.insert(label_entry.id().to_string());
                label_parent_id = Some(label_entry.id().to_string());
                label_entries.push(label_entry);
            }

            self.file_entries = std::iter::once(FileEntry::Header(header))
                .chain(path_without_labels.into_iter().map(FileEntry::Entry))
                .chain(label_entries.into_iter().map(FileEntry::Entry))
                .collect();
            self.session_id = new_session_id;
            self.session_file = Some(new_session_file.clone());
            self.build_index();

            let has_assistant = self.file_entries.iter().any(|entry| {
                matches!(
                    entry,
                    FileEntry::Entry(SessionEntry::Message { message: SessionMessage::Llm(pi_ai::types::Message::Assistant(_)), .. })
                )
            });
            if has_assistant {
                self.rewrite_file();
                self.flushed = true;
            } else {
                self.flushed = false;
            }

            return Some(new_session_file);
        }

        // In-memory mode.
        let mut label_entries: Vec<SessionEntry> = Vec::new();
        let mut label_parent_id = path_without_labels.last().map(|e| e.id().to_string());
        let mut used_ids: HashSet<String> = path_entry_ids.clone();
        for (target_id, label, label_timestamp) in &labels_to_write {
            let label_entry = SessionEntry::Label {
                base: SessionEntryBase {
                    id: generate_id(&|candidate| used_ids.contains(candidate)),
                    parent_id: label_parent_id.clone(),
                    timestamp: label_timestamp.clone(),
                },
                target_id: target_id.clone(),
                label: Some(label.clone()),
            };
            used_ids.insert(label_entry.id().to_string());
            label_parent_id = Some(label_entry.id().to_string());
            label_entries.push(label_entry);
        }
        self.file_entries = std::iter::once(FileEntry::Header(header))
            .chain(path_without_labels.into_iter().map(FileEntry::Entry))
            .chain(label_entries.into_iter().map(FileEntry::Entry))
            .collect();
        self.session_id = new_session_id;
        self.build_index();
        None
    }

    // -----------------------------------------------------------------------
    // Static constructors
    // -----------------------------------------------------------------------

    pub fn create(cwd: &str, session_dir: Option<&str>, options: Option<NewSessionOptions>) -> SessionManager {
        let dir = match session_dir {
            Some(dir) => normalize_path(dir),
            None => get_default_session_dir(cwd),
        };
        Self::new(cwd, &dir, None, true, options, None)
    }

    pub fn open(path: &str, session_dir: Option<&str>, cwd_override: Option<&str>) -> SessionManager {
        let resolved_path = resolve_path(path, None);
        let mut header: Option<SessionHeader> = None;
        let mut preloaded: Option<Vec<FileEntry>> = None;
        if cwd_override.is_none() && Path::new(&resolved_path).exists() {
            match read_session_header(&resolved_path) {
                Ok(header_value) => header = header_value,
                Err(_) => {
                    // Bounded scan failed; a full load remains authoritative.
                    preloaded = Some(load_entries_from_file(&resolved_path));
                    let first = preloaded.as_ref().and_then(|entries| entries.first());
                    header = match first {
                        Some(FileEntry::Header(header)) => Some(header.clone()),
                        _ => None,
                    };
                }
            }
        }
        let cwd = cwd_override
            .map(|cwd| cwd.to_string())
            .or_else(|| header.as_ref().and_then(get_session_header_cwd))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default().to_string_lossy().to_string());
        let dir = match session_dir {
            Some(dir) => normalize_path(dir),
            None => {
                // Derive from the file's parent directory.
                let parent = Path::new(&resolved_path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                resolve_path(&parent, None)
            }
        };
        Self::new(&cwd, &dir, Some(resolved_path), true, None, preloaded)
    }

    pub fn continue_recent(cwd: &str, session_dir: Option<&str>) -> SessionManager {
        let dir = match session_dir {
            Some(dir) => normalize_path(dir),
            None => get_default_session_dir(cwd),
        };
        let filter_cwd = session_dir.is_some() && dir != get_default_session_dir_path(cwd);
        let most_recent = find_most_recent_session(&dir, if filter_cwd { Some(cwd) } else { None });
        match most_recent {
            Some(most_recent) => Self::new(cwd, &dir, Some(most_recent), true, None, None),
            None => Self::new(cwd, &dir, None, true, None, None),
        }
    }

    pub fn in_memory(cwd: Option<String>, options: Option<NewSessionOptions>) -> SessionManager {
        let cwd = cwd.unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });
        Self::new(&cwd, "", None, false, options, None)
    }

    pub fn fork_from(
        source_path: &str,
        target_cwd: &str,
        session_dir: Option<&str>,
        options: Option<NewSessionOptions>,
    ) -> SessionManager {
        let resolved_source_path = resolve_path(source_path, None);
        let resolved_target_cwd = resolve_path(target_cwd, None);
        let source_entries = load_entries_from_file(&resolved_source_path);
        if source_entries.is_empty() {
            panic!("Cannot fork: source session file is empty or invalid: {resolved_source_path}");
        }
        let source_header = source_entries.iter().find_map(|entry| match entry {
            FileEntry::Header(header) => Some(header.clone()),
            _ => None,
        });
        let _source_header = source_header.as_ref().unwrap_or_else(|| {
            panic!("Cannot fork: source session has no header: {resolved_source_path}")
        });

        let dir = match session_dir {
            Some(dir) => normalize_path(dir),
            None => get_default_session_dir(&resolved_target_cwd),
        };
        if !Path::new(&dir).exists() {
            let _ = fs::create_dir_all(&dir);
        }

        if let Some(id) = options.as_ref().and_then(|o| o.id.as_deref()) {
            assert_valid_session_id(id).expect("invalid session id");
        }
        let new_session_id = options
            .as_ref()
            .and_then(|o| o.id.clone())
            .unwrap_or_else(create_session_id);
        let timestamp = now_iso();
        let file_timestamp = timestamp.replace([':', '.'], "-");
        let new_session_file = join(&dir, &format!("{file_timestamp}_{new_session_id}.jsonl"));

        let new_header = SessionHeader {
            version: Some(CURRENT_SESSION_VERSION),
            id: new_session_id,
            timestamp,
            cwd: resolved_target_cwd.clone(),
            parent_session: Some(resolved_source_path.clone()),
        };
        let mut fd = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&new_session_file)
            .expect("create forked session");
        let _ = writeln!(fd, "{}", crate::core::session_types::file_entry_to_json_string(&FileEntry::Header(new_header)));
        for entry in &source_entries {
            if let FileEntry::Entry(entry) = entry {
                let _ = writeln!(fd, "{}", crate::core::session_types::file_entry_to_json_string(&FileEntry::Entry(entry.clone())));
            }
        }

        Self::new(&resolved_target_cwd, &dir, Some(new_session_file), true, None, None)
    }

    /// List all sessions for a directory.
    pub fn list(cwd: &str, session_dir: Option<&str>, on_progress: Option<SessionListProgress>) -> Vec<SessionInfo> {
        let dir = match session_dir {
            Some(dir) => normalize_path(dir),
            None => get_default_session_dir(cwd),
        };
        let filter_cwd = session_dir.is_some() && dir != get_default_session_dir_path(cwd);
        let resolved_cwd = resolve_path(cwd, None);
        let mut sessions: Vec<SessionInfo> = list_sessions_from_dir(&dir, on_progress.as_ref())
            .into_iter()
            .filter(|session| !filter_cwd || session_cwd_matches(Some(&session.cwd), &resolved_cwd))
            .collect();
        sessions.sort_by(|a, b| b.modified_ms.partial_cmp(&a.modified_ms).unwrap_or(std::cmp::Ordering::Equal));
        sessions
    }

    /// List all sessions across all project directories.
    pub fn list_all(session_dir: Option<&str>, on_progress: Option<SessionListProgress>) -> Vec<SessionInfo> {
        match session_dir {
            Some(dir) => {
                let dir = normalize_path(dir);
                let mut sessions = list_sessions_from_dir(&dir, on_progress.as_ref());
                sessions.sort_by(|a, b| b.modified_ms.partial_cmp(&a.modified_ms).unwrap_or(std::cmp::Ordering::Equal));
                sessions
            }
            None => {
                let sessions_dir = crate::config::get_sessions_dir();
                let mut dirs: Vec<String> = Vec::new();
                match fs::read_dir(&sessions_dir) {
                    Ok(entries) => {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            let file_type = entry.file_type().ok();
                            if file_type.map(|t| t.is_dir() || t.is_symlink()).unwrap_or(false) {
                                dirs.push(path.to_string_lossy().to_string());
                            }
                        }
                    }
                    Err(_) => return Vec::new(),
                }

                let mut all_files: Vec<String> = Vec::new();
                for dir in &dirs {
                    if let Ok(entries) = fs::read_dir(dir) {
                        for entry in entries.flatten() {
                            let path = entry.path().to_string_lossy().to_string();
                            if path.ends_with(".jsonl") {
                                all_files.push(path);
                            }
                        }
                    }
                }
                let total = all_files.len() as i64;
                let mut sessions = Vec::new();
                for (index, file) in all_files.iter().enumerate() {
                    if let Some(info) = build_session_info(file) {
                        sessions.push(info);
                    }
                    if let Some(progress) = on_progress.as_ref() {
                        progress(index as i64 + 1, total);
                    }
                }
                sessions.sort_by(|a, b| b.modified_ms.partial_cmp(&a.modified_ms).unwrap_or(std::cmp::Ordering::Equal));
                sessions
            }
        }
    }
}

impl std::fmt::Debug for SessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionManager")
            .field("session_id", &self.session_id)
            .field("session_file", &self.session_file)
            .field("leaf_id", &self.leaf_id)
            .finish()
    }
}

/// Read-only view trait mirroring the JS ReadonlySessionManager pick.
pub trait ReadonlySessionManager {
    fn get_cwd(&self) -> String;
    fn get_session_dir(&self) -> String;
    fn get_session_id(&self) -> String;
    fn get_session_file(&self) -> Option<String>;
    fn get_leaf_id(&self) -> Option<String>;
    fn get_leaf_entry(&self) -> Option<SessionEntry>;
    fn get_entry(&self, id: &str) -> Option<SessionEntry>;
    fn get_label(&self, id: &str) -> Option<String>;
    fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry>;
    fn build_context_entries(&self) -> Vec<SessionEntry>;
    fn get_header(&self) -> Option<SessionHeader>;
    fn get_entries(&self) -> Vec<SessionEntry>;
    fn get_tree(&self) -> Vec<SessionTreeNode>;
    fn get_session_name(&self) -> Option<String>;
}

impl ReadonlySessionManager for SessionManager {
    fn get_cwd(&self) -> String {
        self.cwd.clone()
    }
    fn get_session_dir(&self) -> String {
        self.session_dir.clone()
    }
    fn get_session_id(&self) -> String {
        self.session_id.clone()
    }
    fn get_session_file(&self) -> Option<String> {
        self.session_file.clone()
    }
    fn get_leaf_id(&self) -> Option<String> {
        self.leaf_id.clone()
    }
    fn get_leaf_entry(&self) -> Option<SessionEntry> {
        SessionManager::get_leaf_entry(self)
    }
    fn get_entry(&self, id: &str) -> Option<SessionEntry> {
        SessionManager::get_entry(self, id)
    }
    fn get_label(&self, id: &str) -> Option<String> {
        SessionManager::get_label(self, id)
    }
    fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry> {
        SessionManager::get_branch(self, from_id)
    }
    fn build_context_entries(&self) -> Vec<SessionEntry> {
        SessionManager::build_context_entries(self)
    }
    fn get_header(&self) -> Option<SessionHeader> {
        SessionManager::get_header(self)
    }
    fn get_entries(&self) -> Vec<SessionEntry> {
        SessionManager::get_entries(self)
    }
    fn get_tree(&self) -> Vec<SessionTreeNode> {
        SessionManager::get_tree(self)
    }
    fn get_session_name(&self) -> Option<String> {
        SessionManager::get_session_name(self)
    }
}
