//! Harness event bus, prompt templates, and system-prompt formatting; ports
//! of `packages/agent/src/harness/{events,prompt-templates,system-prompt}.ts`.

use std::collections::HashMap;
use std::sync::Mutex;

use super::skills::Skill;
use super::env::types::{FileInfo, FileKind, FileSystem, Result};
use super::env::nodejs::StdExecutionEnv;

// ---------------------------------------------------------------------------
// events.ts
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct RunStartEvent {
    pub lane: String,
    pub run_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunEndEvent {
    pub lane: String,
    pub run_id: String,
    pub outcome: String,
    pub leaf_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HarnessEvent {
    RunStart(RunStartEvent),
    RunEnd(RunEndEvent),
}

impl HarnessEvent {
    pub fn event_type(&self) -> &'static str {
        match self {
            HarnessEvent::RunStart(_) => "run_start",
            HarnessEvent::RunEnd(_) => "run_end",
        }
    }
}

pub type HarnessEventListener = std::sync::Arc<dyn Fn(&HarnessEvent) + Send + Sync>;

static WATCH_LISTENERS: Mutex<Vec<HarnessEventListener>> = Mutex::new(Vec::new());

pub struct HarnessEventBus {
    listeners: std::sync::Arc<Mutex<HashMap<&'static str, Vec<HarnessEventListener>>>>,
}

impl HarnessEventBus {
    pub fn new() -> Self {
        Self {
            listeners: std::sync::Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a listener for future events of one type; returns its
    /// unsubscribe function.
    pub fn on(&self, event_type: &'static str, listener: HarnessEventListener) -> Box<dyn Fn() + Send + Sync> {
        self.listeners
            .lock()
            .unwrap()
            .entry(event_type)
            .or_default()
            .push(listener.clone());
        let listeners = self.listeners.clone();
        Box::new(move || {
            let mut listeners = listeners.lock().unwrap();
            if let Some(type_listeners) = listeners.get_mut(event_type) {
                type_listeners.retain(|existing| !std::sync::Arc::ptr_eq(existing, &listener));
                if type_listeners.is_empty() {
                    listeners.remove(event_type);
                }
            }
        })
    }

    /// Publish an event to current event subscriptions and watch
    /// subscriptions. Listeners are invoked synchronously.
    pub fn emit(&self, event: &HarnessEvent) {
        let event_type = event.event_type();
        let direct: Vec<HarnessEventListener> = self
            .listeners
            .lock()
            .unwrap()
            .get(event_type)
            .cloned()
            .unwrap_or_default();
        for listener in direct {
            listener(event);
        }
        // Deliver every event to each watcher; the receive closure buffers
        // until start() is called.
        let watch_listeners: Vec<HarnessEventListener> = WATCH_LISTENERS.lock().unwrap().clone();
        for listener in watch_listeners {
            listener(event);
        }
    }

    /// Create a watch handle that buffers events until start() is called.
    pub fn watch<TSnapshot>(&self, capture_snapshot: impl FnOnce() -> TSnapshot) -> WatchHandle<TSnapshot> {
        let buffered = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
        let state: std::sync::Arc<Mutex<Option<HarnessEventListener>>> = std::sync::Arc::new(Mutex::new(None));
        let receive: HarnessEventListener = {
            let buffered = buffered.clone();
            let state = state.clone();
            std::sync::Arc::new(move |event: &HarnessEvent| {
                // Clone out of the lock before invoking: reentrant emits must
                // not deadlock on the state mutex.
                let listener = state.lock().unwrap().clone();
                match listener {
                    Some(listener) => listener(event),
                    None => buffered.lock().unwrap().push(event.clone()),
                }
            })
        };
        WATCH_LISTENERS.lock().unwrap().push(receive.clone());
        let snapshot = capture_snapshot();
        WatchHandle {
            snapshot,
            receive,
            buffered,
            state,
        }
    }
}

/// A watch handle: snapshot at creation, buffered events until start().
pub struct WatchHandle<TSnapshot> {
    pub snapshot: TSnapshot,
    receive: HarnessEventListener,
    buffered: std::sync::Arc<Mutex<Vec<HarnessEvent>>>,
    state: std::sync::Arc<Mutex<Option<HarnessEventListener>>>,
}

impl<TSnapshot> WatchHandle<TSnapshot> {
    /// Flush buffered events (preserving order, reentrant emissions keep
    /// buffering) and deliver future events to the listener.
    pub fn start(&self, listener: HarnessEventListener) {
        *self.state.lock().unwrap() = Some(listener);
        loop {
            let pending = std::mem::take(&mut *self.buffered.lock().unwrap());
            if pending.is_empty() {
                break;
            }
            for event in pending {
                (self.receive)(&event);
            }
        }
    }

    pub fn unsubscribe(&self) {
        WATCH_LISTENERS.lock().unwrap().retain(|listener| !std::sync::Arc::ptr_eq(listener, &self.receive));
        self.buffered.lock().unwrap().clear();
    }
}

// ---------------------------------------------------------------------------
// system-prompt.ts
// ---------------------------------------------------------------------------

/// Format skills into the model-visible system prompt block.
pub fn format_skills_for_system_prompt(skills: &[Skill]) -> String {
    let visible_skills: Vec<&Skill> = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation.unwrap_or(false))
        .collect();
    if visible_skills.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = vec![
        "The following skills provide specialized instructions for specific tasks.".to_string(),
        "Read the full skill file when the task matches its description.".to_string(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];

    for skill in visible_skills {
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!("    <description>{}</description>", escape_xml(&skill.description)));
        lines.push(format!("    <location>{}</location>", escape_xml(&skill.file_path)));
        lines.push("  </skill>".to_string());
    }

    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

// ---------------------------------------------------------------------------
// prompt-templates.ts
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct PromptTemplate {
    pub name: String,
    pub description: Option<String>,
    pub content: String,
    /// Absolute path of the template file (JS filePath).
    pub file_path: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PromptTemplateDiagnostic {
    pub kind: String,
    pub code: String,
    pub message: String,
    pub path: String,
}

/// Load prompt templates from one or more paths. Directory inputs load
/// direct .md children non-recursively; missing paths and non-markdown files
/// are skipped.
pub fn load_prompt_templates(
    env: &StdExecutionEnv,
    paths: &[String],
) -> (Vec<PromptTemplate>, Vec<PromptTemplateDiagnostic>) {
    let mut prompt_templates: Vec<PromptTemplate> = Vec::new();
    let mut diagnostics: Vec<PromptTemplateDiagnostic> = Vec::new();
    for path in paths {
        let info = match env.file_info(path) {
            Result::Ok { value } => value,
            Result::Err { error } => {
                if error.code != super::env::types::FileErrorCode::NotFound {
                    diagnostics.push(diagnostic("file_info_failed", &error.message, path));
                }
                continue;
            }
        };
        match resolve_kind(env, &info, &mut diagnostics) {
            Some(FileKind::Directory) => {
                let result = load_templates_from_dir(env, &info.path);
                prompt_templates.extend(result.0);
                diagnostics.extend(result.1);
            }
            Some(FileKind::File) if info.name.ends_with(".md") => {
                let result = load_template_from_file(env, &info.path, &info.name);
                if let Some(template) = result.0 {
                    prompt_templates.push(template);
                }
                diagnostics.extend(result.1);
            }
            _ => {}
        }
    }
    (prompt_templates, diagnostics)
}

fn diagnostic(code: &str, message: &str, path: &str) -> PromptTemplateDiagnostic {
    PromptTemplateDiagnostic {
        kind: "warning".to_string(),
        code: code.to_string(),
        message: message.to_string(),
        path: path.to_string(),
    }
}

fn load_templates_from_dir(
    env: &StdExecutionEnv,
    dir: &str,
) -> (Vec<PromptTemplate>, Vec<PromptTemplateDiagnostic>) {
    let mut prompt_templates: Vec<PromptTemplate> = Vec::new();
    let mut diagnostics: Vec<PromptTemplateDiagnostic> = Vec::new();
    let mut entries = match env.list_dir(dir) {
        Result::Ok { value } => value,
        Result::Err { error } => {
            diagnostics.push(diagnostic("list_failed", &error.message, dir));
            return (prompt_templates, diagnostics);
        }
    };
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    for entry in entries {
        let Some(kind) = resolve_kind(env, &entry, &mut diagnostics) else {
            continue;
        };
        if kind != FileKind::File || !entry.name.ends_with(".md") {
            continue;
        }
        let result = load_template_from_file(env, &entry.path, &entry.name);
        if let Some(template) = result.0 {
            prompt_templates.push(template);
        }
        diagnostics.extend(result.1);
    }
    (prompt_templates, diagnostics)
}

fn load_template_from_file(
    env: &StdExecutionEnv,
    file_path: &str,
    file_name: &str,
) -> (Option<PromptTemplate>, Vec<PromptTemplateDiagnostic>) {
    let mut diagnostics: Vec<PromptTemplateDiagnostic> = Vec::new();
    let content = match env.read_text_file(file_path) {
        Result::Ok { value } => value,
        Result::Err { error } => {
            diagnostics.push(diagnostic("read_failed", &error.message, file_path));
            return (None, diagnostics);
        }
    };
    let parsed = super::skills::parse_frontmatter(&content);
    let (frontmatter, body) = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            diagnostics.push(diagnostic("parse_failed", &error, file_path));
            return (None, diagnostics);
        }
    };
    let description = frontmatter
        .get("description")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let name = file_name
        .strip_suffix(".md")
        .map(|name| name.to_string())
        .unwrap_or_else(|| file_name.to_string());
    (
        Some(PromptTemplate {
            name,
            description,
            content: body,
            file_path: file_path.to_string(),
        }),
        diagnostics,
    )
}

fn resolve_kind(
    env: &StdExecutionEnv,
    info: &FileInfo,
    diagnostics: &mut Vec<PromptTemplateDiagnostic>,
) -> Option<FileKind> {
    if info.kind == FileKind::File || info.kind == FileKind::Directory {
        return Some(info.kind.clone());
    }
    let canonical_path = match env.canonical_path(&info.path) {
        Result::Ok { value } => value,
        Result::Err { error } => {
            if error.code != super::env::types::FileErrorCode::NotFound {
                diagnostics.push(diagnostic("file_info_failed", &error.message, &info.path));
            }
            return None;
        }
    };
    match env.file_info(&canonical_path) {
        Result::Ok { value } => match value.kind {
            FileKind::File | FileKind::Directory => Some(value.kind),
            _ => None,
        },
        Result::Err { error } => {
            if error.code != super::env::types::FileErrorCode::NotFound {
                diagnostics.push(diagnostic("file_info_failed", &error.message, &info.path));
            }
            None
        }
    }
}

/// Parse command arguments honoring single/double quotes (mirrors
/// parseCommandArgs).
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for char in args_string.chars() {
        if let Some(quote) = in_quote {
            if char == quote {
                in_quote = None;
            } else {
                current.push(char);
            }
        } else if char == '"' || char == '\'' {
            in_quote = Some(char);
        } else if char == ' ' || char == '\t' {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(char);
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// Substitute prompt template placeholders ($1, $@, $ARGUMENTS, ${@:N},
/// ${@:N:L}) with command arguments.
pub fn substitute_args(content: &str, args: &[String]) -> String {
    let mut result = content.to_string();
    // $N (any digit run; out-of-range indices expand to the empty string).
    result = replace_positional_args(&result, args);
    // ${@:N} and ${@:N:L}
    result = replace_slice_args(&result, args);
    let all_args = args.join(" ");
    result = result.replace("$ARGUMENTS", &all_args);
    result = result.replace("$@", &all_args);
    result
}

fn replace_positional_args(content: &str, args: &[String]) -> String {
    let mut out = String::new();
    let mut rest = content;
    while let Some(position) = rest.find('$') {
        out.push_str(&rest[..position]);
        let after = &rest[position + 1..];
        let digits: String = after.chars().take_while(|char| char.is_ascii_digit()).collect();
        if digits.is_empty() {
            out.push('$');
            rest = after;
            continue;
        }
        let index = digits.parse::<usize>().unwrap_or(0);
        out.push_str(args.get(index - 1).map(|arg| arg.as_str()).unwrap_or(""));
        rest = &after[digits.len()..];
    }
    out.push_str(rest);
    out
}

fn replace_slice_args(content: &str, args: &[String]) -> String {
    let mut result = String::new();
    let mut rest = content;
    while let Some(start_index) = rest.find("${@:") {
        result.push_str(&rest[..start_index]);
        let after = &rest[start_index + 4..];
        let Some(end_index) = after.find('}') else {
            result.push_str(&rest[start_index..]);
            return result;
        };
        let spec = &after[..end_index];
        let (start_str, length_str) = match spec.split_once(':') {
            Some((start, length)) => (start, Some(length)),
            None => (spec, None),
        };
        let start = start_str.parse::<i64>().unwrap_or(1) - 1;
        let start = start.max(0) as usize;
        let replacement = match length_str {
            Some(length) => {
                let length = length.parse::<usize>().unwrap_or(0);
                args[start..(start + length).min(args.len())].join(" ")
            }
            None => args[start..].join(" "),
        };
        result.push_str(&replacement);
        rest = &after[end_index + 1..];
    }
    result.push_str(rest);
    result
}

/// Format a prompt template invocation with positional arguments.
pub fn format_prompt_template_invocation(template: &PromptTemplate, args: &[String]) -> String {
    substitute_args(&template.content, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_bus_delivers_and_unsubscribes() {
        let bus = HarnessEventBus::new();
        let received = std::sync::Arc::new(Mutex::new(Vec::new()));
        let received_clone = received.clone();
        let unsubscribe = bus.on("run_start", std::sync::Arc::new(move |event| {
            if let HarnessEvent::RunStart(run_start) = event {
                received_clone.lock().unwrap().push(run_start.run_id.clone());
            }
        }));
        bus.emit(&HarnessEvent::RunStart(RunStartEvent {
            lane: "main".to_string(),
            run_id: "r1".to_string(),
        }));
        assert_eq!(received.lock().unwrap().clone(), vec!["r1".to_string()]);
        unsubscribe();
        bus.emit(&HarnessEvent::RunStart(RunStartEvent {
            lane: "main".to_string(),
            run_id: "r2".to_string(),
        }));
        assert_eq!(received.lock().unwrap().clone(), vec!["r1".to_string()]);
    }

    #[test]
    fn watch_buffers_until_start() {
        let bus = HarnessEventBus::new();
        let handle = bus.watch(|| 42usize);
        assert_eq!(handle.snapshot, 42);
        bus.emit(&HarnessEvent::RunStart(RunStartEvent {
            lane: "main".to_string(),
            run_id: "r1".to_string(),
        }));
        let received = std::sync::Arc::new(Mutex::new(Vec::new()));
        let received_clone = received.clone();
        handle.start(std::sync::Arc::new(move |event| {
            received_clone.lock().unwrap().push(event.event_type().to_string());
        }));
        assert_eq!(received.lock().unwrap().clone(), vec!["run_start".to_string()]);
        bus.emit(&HarnessEvent::RunEnd(RunEndEvent {
            lane: "main".to_string(),
            run_id: "r1".to_string(),
            outcome: "completed".to_string(),
            leaf_id: "l1".to_string(),
        }));
        assert_eq!(
            received.lock().unwrap().clone(),
            vec!["run_start".to_string(), "run_end".to_string()]
        );
    }

    #[test]
    fn formats_skills_for_system_prompt() {
        let skills = vec![
            Skill {
                name: "alpha".to_string(),
                description: "Does <things> & more".to_string(),
                content: "body".to_string(),
                file_path: "/a/SKILL.md".to_string(),
                disable_model_invocation: None,
            },
            Skill {
                name: "hidden".to_string(),
                description: "h".to_string(),
                content: "body".to_string(),
                file_path: "/b/SKILL.md".to_string(),
                disable_model_invocation: Some(true),
            },
        ];
        let formatted = format_skills_for_system_prompt(&skills);
        assert!(formatted.contains("<available_skills>"));
        assert!(formatted.contains("<name>alpha</name>"));
        assert!(formatted.contains("Does &lt;things&gt; &amp; more"));
        assert!(!formatted.contains("hidden"));
        assert!(format_skills_for_system_prompt(&[]).is_empty());
    }

    #[test]
    fn parses_command_args_with_quotes() {
        assert_eq!(parse_command_args(""), Vec::<String>::new());
        assert_eq!(parse_command_args("a b\tc"), vec!["a", "b", "c"]);
        assert_eq!(parse_command_args("\"hello world\" it's"), vec!["hello world", "its"]);
        assert_eq!(parse_command_args("'a\"b'"), vec!["a\"b"]);
    }

    #[test]
    fn substitutes_args() {
        let args = vec!["first".to_string(), "second".to_string(), "third".to_string()];
        assert_eq!(substitute_args("$1 and $2", &args), "first and second");
        assert_eq!(substitute_args("$5", &args), ""); // out of range -> empty
        assert_eq!(substitute_args("$@", &args), "first second third");
        assert_eq!(substitute_args("$ARGUMENTS!", &args), "first second third!");
        assert_eq!(substitute_args("${@:2}", &args), "second third");
        assert_eq!(substitute_args("${@:1:2}", &args), "first second");
        assert_eq!(substitute_args("${@:0:2}", &args), "first second"); // 0 clamps to 1
        assert_eq!(substitute_args("no placeholders", &args), "no placeholders");
    }

    #[test]
    fn formats_template_invocation() {
        let template = PromptTemplate {
            name: "t".to_string(),
            description: Some("d".to_string()),
            content: "Do $1 with $ARGUMENTS".to_string(),
            file_path: "/tmp/t.md".to_string(),
        };
        assert_eq!(
            format_prompt_template_invocation(&template, &["X".to_string(), "Y".to_string()]),
            "Do X with X Y"
        );
    }
}
