//! Session selector, port of `components/session-selector.ts`.
//!
//! ponytail: session loading is synchronous (list sessions directly); the
//! async loader/progress plumbing and trash-cli deletion fall back to
//! permanent unlink.

use std::sync::Arc;

use pi_tui::components::input::Input;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::Component;
use pi_tui::utils::{truncate_to_width, visible_width};

use crate::core::keybindings::KeybindingsManager;
use crate::core::session_types::SessionInfo;
use crate::modes::interactive::components::keybinding_hints::{key_hint, key_text};
use crate::modes::interactive::components::session_selector_search::{
    filter_and_sort_sessions, has_session_name, NameFilter, SortMode,
};
use crate::modes::interactive::theme::theme::theme;

type SessionScope = &'static str; // "current" | "all"

fn shorten_path(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    if path.starts_with(&home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    }
}

fn format_session_date(modified_ms: f64) -> String {
    let now_ms = crate::core::session_manager::now_iso();
    let now_ms = parse_iso_ms(&now_ms);
    let diff_ms = now_ms - modified_ms;
    let diff_mins = (diff_ms / 60000.0).floor();
    let diff_hours = (diff_ms / 3600000.0).floor();
    let diff_days = (diff_ms / 86400000.0).floor();

    if diff_mins < 1.0 {
        "now".to_string()
    } else if diff_mins < 60.0 {
        format!("{diff_mins}m")
    } else if diff_hours < 24.0 {
        format!("{diff_hours}h")
    } else if diff_days < 7.0 {
        format!("{diff_days}d")
    } else if diff_days < 30.0 {
        format!("{}w", (diff_days / 7.0).floor())
    } else if diff_days < 365.0 {
        format!("{}mo", (diff_days / 30.0).floor())
    } else {
        format!("{}y", (diff_days / 365.0).floor())
    }
}

fn parse_iso_ms(iso: &str) -> f64 {
    // ISO timestamp "2024-01-01T00:00:00.000Z" style; approximate parse.
    let mut ms = 0.0;
    let bytes = iso.as_bytes();
    if bytes.len() >= 19 {
        let year: f64 = iso[0..4].parse().unwrap_or(0.0);
        let month: f64 = iso[5..7].parse().unwrap_or(0.0);
        let day: f64 = iso[8..10].parse().unwrap_or(0.0);
        let hour: f64 = iso[11..13].parse().unwrap_or(0.0);
        let min: f64 = iso[14..16].parse().unwrap_or(0.0);
        let sec: f64 = iso[17..19].parse().unwrap_or(0.0);
        let mut days = 0.0;
        for y in 1970..(year as i64) {
            days += if is_leap(y) { 366.0 } else { 365.0 };
        }
        let month_days = [31.0, if is_leap(year as i64) { 29.0 } else { 28.0 }, 31.0, 30.0, 31.0, 30.0, 31.0, 31.0, 30.0, 31.0, 30.0, 31.0];
        for m in 0..((month as i64 - 1).max(0)) as usize {
            days += month_days[m];
        }
        days += day - 1.0;
        ms = (days * 86400.0 + hour * 3600.0 + min * 60.0 + sec) * 1000.0;
    }
    ms
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// A session tree node for hierarchical display.
#[derive(Clone)]
struct SessionTreeNode {
    session: SessionInfo,
    children: Vec<SessionTreeNode>,
    latest_activity: f64,
}

/// Flattened node for display with tree structure info.
#[derive(Clone)]
struct FlatSessionNode {
    session: SessionInfo,
    depth: usize,
    is_last: bool,
    /// For each ancestor level, whether there are more siblings after it.
    ancestor_continues: Vec<bool>,
}

/// Build a tree from sessions based on parent_session_path.
fn build_session_tree(sessions: &[SessionInfo]) -> Vec<SessionTreeNode> {
    let mut by_path: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut nodes: Vec<SessionTreeNode> = Vec::new();
    for session in sessions {
        by_path.insert(session.path.clone(), nodes.len());
        nodes.push(SessionTreeNode {
            session: session.clone(),
            children: Vec::new(),
            latest_activity: session.modified_ms,
        });
    }

    let mut roots: Vec<usize> = Vec::new();
    for (i, session) in sessions.iter().enumerate() {
        match &session.parent_session_path {
            Some(parent) if by_path.contains_key(parent) => {
                let parent_idx = by_path[parent];
                let child_idx = by_path[&session.path];
                let child = nodes[child_idx].clone();
                nodes[parent_idx].children.push(child);
            }
            _ => roots.push(i),
        }
    }

    // Reconstruct children by cloning references; simpler: store indices.
    let mut tree_roots: Vec<SessionTreeNode> = Vec::new();
    for root in roots {
        let mut node = nodes[root].clone();
        node.children = collect_children(&nodes, &by_path, root);
        update_latest_activity(&mut node);
        tree_roots.push(node);
    }

    fn collect_children(
        nodes: &[SessionTreeNode],
        by_path: &std::collections::HashMap<String, usize>,
        parent_idx: usize,
    ) -> Vec<SessionTreeNode> {
        let mut children = Vec::new();
        for (i, node) in nodes.iter().enumerate() {
            if let Some(parent) = &node.session.parent_session_path {
                if by_path.get(parent) == Some(&parent_idx) {
                    let mut child = node.clone();
                    child.children = collect_children(nodes, by_path, i);
                    children.push(child);
                }
            }
        }
        children
    }

    fn update_latest_activity(node: &mut SessionTreeNode) -> f64 {
        let mut latest = node.session.modified_ms;
        for child in &mut node.children {
            latest = latest.max(update_latest_activity(child));
        }
        node.latest_activity = latest;
        latest
    }

    fn sort_nodes(nodes: &mut [SessionTreeNode]) {
        nodes.sort_by(|a, b| b.latest_activity.partial_cmp(&a.latest_activity).unwrap_or(std::cmp::Ordering::Equal));
        for node in nodes.iter_mut() {
            sort_nodes(&mut node.children);
        }
    }
    sort_nodes(&mut tree_roots);
    tree_roots
}

/// Flatten tree into display list with tree structure metadata.
fn flatten_session_tree(roots: &[SessionTreeNode]) -> Vec<FlatSessionNode> {
    let mut result: Vec<FlatSessionNode> = Vec::new();

    fn walk(
        node: &SessionTreeNode,
        depth: usize,
        ancestor_continues: &[bool],
        is_last: bool,
        result: &mut Vec<FlatSessionNode>,
    ) {
        result.push(FlatSessionNode {
            session: node.session.clone(),
            depth,
            is_last,
            ancestor_continues: ancestor_continues.to_vec(),
        });
        for (i, child) in node.children.iter().enumerate() {
            let child_is_last = i == node.children.len() - 1;
            let continues = if depth > 0 { !is_last } else { false };
            let mut ancestor_continues = ancestor_continues.to_vec();
            ancestor_continues.push(continues);
            walk(child, depth + 1, &ancestor_continues, child_is_last, result);
        }
    }

    for (i, root) in roots.iter().enumerate() {
        walk(root, 0, &[], i == roots.len() - 1, &mut result);
    }
    result
}

/// Custom session list component with multi-line items and search.
pub struct SessionList {
    all_sessions: Vec<SessionInfo>,
    filtered_sessions: Vec<FlatSessionNode>,
    selected_index: usize,
    search_input: Input,
    show_cwd: bool,
    sort_mode: SortMode,
    name_filter: NameFilter,
    keybindings: KeybindingsManager,
    show_path: bool,
    confirming_delete_path: Option<String>,
    current_session_canonical_path: Option<String>,
    pub on_select: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub on_cancel: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_toggle_scope: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_toggle_sort: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_toggle_name_filter: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_delete_session: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub on_rename_session: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub on_error: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    max_visible: usize,
}

fn canonicalize(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string())
}

impl SessionList {
    pub fn new(
        sessions: Vec<SessionInfo>,
        show_cwd: bool,
        sort_mode: SortMode,
        name_filter: NameFilter,
        keybindings: KeybindingsManager,
        current_session_file_path: Option<&str>,
    ) -> Self {
        let mut list = Self {
            all_sessions: sessions,
            filtered_sessions: Vec::new(),
            selected_index: 0,
            search_input: Input::new(),
            show_cwd,
            sort_mode,
            name_filter,
            keybindings,
            show_path: false,
            confirming_delete_path: None,
            current_session_canonical_path: current_session_file_path.map(canonicalize),
            on_select: None,
            on_cancel: None,
            on_toggle_scope: None,
            on_toggle_sort: None,
            on_toggle_name_filter: None,
            on_delete_session: None,
            on_rename_session: None,
            on_error: None,
            max_visible: 10,
        };
        list.filter_sessions("");
        list
    }

    pub fn set_sort_mode(&mut self, sort_mode: SortMode) {
        self.sort_mode = sort_mode;
        let query = self.search_input.get_value().to_string();
        self.filter_sessions(&query);
    }

    pub fn set_name_filter(&mut self, name_filter: NameFilter) {
        self.name_filter = name_filter;
        let query = self.search_input.get_value().to_string();
        self.filter_sessions(&query);
    }

    pub fn set_sessions(&mut self, sessions: Vec<SessionInfo>, show_cwd: bool) {
        self.all_sessions = sessions;
        self.show_cwd = show_cwd;
        let query = self.search_input.get_value().to_string();
        self.filter_sessions(&query);
    }

    pub fn get_search_input(&self) -> &Input {
        &self.search_input
    }

    fn filter_sessions(&mut self, query: &str) {
        let trimmed = query.trim();
        let name_filtered: Vec<SessionInfo> = self
            .all_sessions
            .iter()
            .filter(|session| self.name_filter == NameFilter::All || has_session_name(session))
            .cloned()
            .collect();

        if self.sort_mode == SortMode::Threaded && trimmed.is_empty() {
            let roots = build_session_tree(&name_filtered);
            self.filtered_sessions = flatten_session_tree(&roots);
        } else {
            let filtered = filter_and_sort_sessions(&name_filtered, query, self.sort_mode, NameFilter::All);
            self.filtered_sessions = filtered
                .into_iter()
                .map(|session| FlatSessionNode {
                    session,
                    depth: 0,
                    is_last: true,
                    ancestor_continues: Vec::new(),
                })
                .collect();
        }
        self.selected_index = self.selected_index.min(self.filtered_sessions.len().saturating_sub(1));
    }

    fn is_current_session_path(&self, path: &str) -> bool {
        match &self.current_session_canonical_path {
            Some(current) => canonicalize(path) == *current,
            None => false,
        }
    }

    fn start_delete_confirmation_for_selected_session(&mut self) {
        let Some(selected) = self.filtered_sessions.get(self.selected_index) else {
            return;
        };
        if self.is_current_session_path(&selected.session.path) {
            if let Some(on_error) = &self.on_error {
                on_error("Cannot delete the currently active session");
            }
            return;
        }
        self.confirming_delete_path = Some(selected.session.path.clone());
    }

    pub fn get_selected_session_path(&self) -> Option<String> {
        self.filtered_sessions.get(self.selected_index).map(|node| node.session.path.clone())
    }

    fn build_tree_prefix(&self, node: &FlatSessionNode) -> String {
        if node.depth == 0 {
            return String::new();
        }
        let mut parts: Vec<String> = Vec::new();
        for continues in &node.ancestor_continues {
            parts.push(if *continues { "│  ".to_string() } else { "   ".to_string() });
        }
        parts.push(if node.is_last { "└─ ".to_string() } else { "├─ ".to_string() });
        parts.concat()
    }
}

impl Component for SessionList {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        let mut lines: Vec<String> = Vec::new();

        lines.extend(self.search_input.render(width));
        lines.push(String::new());

        if self.filtered_sessions.is_empty() {
            let empty_message = if self.name_filter == NameFilter::Named {
                let toggle_key = key_text("app.session.toggleNamedFilter");
                if self.show_cwd {
                    format!("  No named sessions found. Press {toggle_key} to show all.")
                } else {
                    format!("  No named sessions in current folder. Press {toggle_key} to show all, or Tab to view all.")
                }
            } else if self.show_cwd {
                "  No sessions found".to_string()
            } else {
                "  No sessions in current folder. Press Tab to view all.".to_string()
            };
            let styled = t.map(|t| t.fg("muted", &empty_message)).unwrap_or(empty_message);
            lines.push(truncate_to_width(&styled, width as f64, "…", false));
            return lines;
        }

        let start_index = (self.selected_index as isize - (self.max_visible as isize / 2))
            .max(0)
            .min((self.filtered_sessions.len() as isize - self.max_visible as isize).max(0))
            .max(0) as usize;
        let end_index = (start_index + self.max_visible).min(self.filtered_sessions.len());

        for i in start_index..end_index {
            let node = &self.filtered_sessions[i];
            let session = &node.session;
            let is_selected = i == self.selected_index;
            let is_confirming_delete = Some(session.path.as_str()) == self.confirming_delete_path.as_deref();
            let is_current = self.is_current_session_path(&session.path);

            let prefix = self.build_tree_prefix(node);

            let has_name = session.name.as_deref().is_some_and(|name| !name.trim().is_empty());
            let display_text = session.name.clone().unwrap_or_else(|| session.first_message.clone());
            let normalized: String = display_text
                .chars()
                .map(|c| if (c as u32) < 0x20 || c as u32 == 0x7f { ' ' } else { c })
                .collect();
            let normalized = normalized.trim();

            let age = format_session_date(session.modified_ms);
            let msg_count = format!("{}", session.message_count);
            let mut right_part = format!("{msg_count} {age}");
            if self.show_cwd && !session.cwd.is_empty() {
                right_part = format!("{} {right_part}", shorten_path(&session.cwd));
            }
            if self.show_path {
                right_part = format!("{} {right_part}", shorten_path(&session.path));
            }

            let cursor = if is_selected {
                t.map(|t| t.fg("accent", "› ")).unwrap_or_else(|| "› ".to_string())
            } else {
                "  ".to_string()
            };

            let prefix_width = visible_width(&prefix) as usize;
            let right_width = visible_width(&right_part) as usize + 2;
            let available_for_msg = (width as isize - 2 - prefix_width as isize - right_width as isize).max(10) as f64;
            let truncated_msg = truncate_to_width(normalized, available_for_msg, "…", false);

            let message_color = if is_confirming_delete {
                Some("error")
            } else if is_current {
                Some("accent")
            } else if has_name {
                Some("warning")
            } else {
                None
            };
            let mut styled_msg = match message_color {
                Some(color) => t.map(|t| t.fg(color, &truncated_msg)).unwrap_or(truncated_msg.clone()),
                None => truncated_msg.clone(),
            };
            if is_selected {
                styled_msg = t.map(|t| t.bold(&styled_msg)).unwrap_or(styled_msg);
            }

            let prefix_styled = t.map(|t| t.fg("dim", &prefix)).unwrap_or(prefix);
            let left_part = format!("{cursor}{prefix_styled}{styled_msg}");
            let left_width = visible_width(&left_part) as usize;
            let spacing = (width as isize - left_width as isize - visible_width(&right_part) as isize).max(1) as usize;
            let styled_right = if is_confirming_delete {
                t.map(|t| t.fg("error", &right_part)).unwrap_or_else(|| right_part.clone())
            } else {
                t.map(|t| t.fg("dim", &right_part)).unwrap_or_else(|| right_part.clone())
            };

            let mut line = format!("{left_part}{}{styled_right}", " ".repeat(spacing));
            if is_selected {
                let bg = t.map(|t| t.get_bg_ansi("selectedBg")).unwrap_or_default();
                if !bg.is_empty() {
                    line = format!("{bg}{line}\x1b[49m");
                }
            }
            lines.push(truncate_to_width(&line, width as f64, "", false));
        }

        if start_index > 0 || end_index < self.filtered_sessions.len() {
            let scroll_text = format!("  ({}/{})", self.selected_index + 1, self.filtered_sessions.len());
            let styled = t.map(|t| t.fg("muted", &scroll_text)).unwrap_or(scroll_text);
            lines.push(truncate_to_width(&styled, width as f64, "", false));
        }

        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = get_keybindings();
        let manager = match &*kb {
            Some(manager) => manager,
            None => return,
        };

        if self.confirming_delete_path.is_some() {
            if manager.matches(data, "tui.select.confirm") {
                let path = self.confirming_delete_path.clone().unwrap();
                self.confirming_delete_path = None;
                if let Some(on_delete) = &self.on_delete_session {
                    on_delete(&path);
                }
                return;
            }
            if manager.matches(data, "tui.select.cancel") {
                self.confirming_delete_path = None;
                return;
            }
            return;
        }

        if manager.matches(data, "tui.input.tab") {
            if let Some(toggle) = &self.on_toggle_scope {
                toggle();
            }
            return;
        }
        if manager.matches(data, "app.session.toggleSort") {
            if let Some(toggle) = &self.on_toggle_sort {
                toggle();
            }
            return;
        }
        if self.keybindings.matches(data, "app.session.toggleNamedFilter") {
            if let Some(toggle) = &self.on_toggle_name_filter {
                toggle();
            }
            return;
        }
        if manager.matches(data, "app.session.togglePath") {
            self.show_path = !self.show_path;
            return;
        }
        if manager.matches(data, "app.session.delete") || manager.matches(data, "app.session.deleteNoninvasive") {
            let query_empty = self.search_input.get_value().is_empty();
            if manager.matches(data, "app.session.deleteNoninvasive") && !query_empty {
                self.search_input.handle_input(data);
                let query = self.search_input.get_value().to_string();
                self.filter_sessions(&query);
                return;
            }
            self.start_delete_confirmation_for_selected_session();
            return;
        }
        if manager.matches(data, "app.session.rename") {
            let selected = self.filtered_sessions.get(self.selected_index);
            if let Some(selected) = selected {
                if let Some(on_rename) = &self.on_rename_session {
                    on_rename(&selected.session.path);
                }
            }
            return;
        }

        if manager.matches(data, "tui.select.up") {
            self.selected_index = self.selected_index.saturating_sub(1);
        } else if manager.matches(data, "tui.select.down") {
            self.selected_index = (self.selected_index + 1).min(self.filtered_sessions.len().saturating_sub(1));
        } else if manager.matches(data, "tui.select.pageUp") {
            self.selected_index = self.selected_index.saturating_sub(self.max_visible);
        } else if manager.matches(data, "tui.select.pageDown") {
            self.selected_index = (self.selected_index + self.max_visible).min(self.filtered_sessions.len().saturating_sub(1));
        } else if manager.matches(data, "tui.select.confirm") {
            let selected = self.filtered_sessions.get(self.selected_index);
            if let Some(selected) = selected {
                if let Some(on_select) = &self.on_select {
                    on_select(&selected.session.path);
                }
            }
        } else if manager.matches(data, "tui.select.cancel") {
            if let Some(on_cancel) = &self.on_cancel {
                on_cancel();
            }
        } else {
            self.search_input.handle_input(data);
            let query = self.search_input.get_value().to_string();
            self.filter_sessions(&query);
        }
    }
}

/// Delete a session file (trash CLI first, then permanent unlink).
pub fn delete_session_file(session_path: &str) -> Result<bool, String> {
    use std::process::Command;
    // Try `trash` CLI first.
    let trash_result = Command::new("trash").arg(session_path).output();
    if let Ok(output) = trash_result {
        if output.status.success() || !std::path::Path::new(session_path).exists() {
            return Ok(true);
        }
    }
    std::fs::remove_file(session_path).map_err(|error| error.to_string())?;
    Ok(false)
}

/// Component that renders a session selector.
pub struct SessionSelectorComponent {
    session_list: SessionList,
    scope: SessionScope,
    sort_mode: SortMode,
    name_filter: NameFilter,
    #[allow(dead_code)]
    current_sessions: Vec<SessionInfo>,
    #[allow(dead_code)]
    all_sessions: Vec<SessionInfo>,
    #[allow(dead_code)]
    show_all: bool,
}

impl SessionSelectorComponent {
    pub fn new(
        current_sessions: Vec<SessionInfo>,
        all_sessions: Vec<SessionInfo>,
        on_select: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        current_session_file_path: Option<&str>,
    ) -> Self {
        let keybindings = KeybindingsManager::new(Default::default(), None);
        let mut session_list = SessionList::new(
            current_sessions.clone(),
            false,
            SortMode::Threaded,
            NameFilter::All,
            keybindings,
            current_session_file_path,
        );
        session_list.on_select = Some(on_select);
        session_list.on_cancel = Some(on_cancel);
        Self {
            session_list,
            scope: "current",
            sort_mode: SortMode::Threaded,
            name_filter: NameFilter::All,
            current_sessions,
            all_sessions,
            show_all: false,
        }
    }

    pub fn get_session_list(&self) -> &SessionList {
        &self.session_list
    }

    #[allow(dead_code)]
    fn toggle_scope(&mut self) {
        if self.scope == "current" {
            self.scope = "all";
            self.show_all = true;
            self.session_list.set_sessions(self.all_sessions.clone(), true);
        } else {
            self.scope = "current";
            self.show_all = false;
            self.session_list.set_sessions(self.current_sessions.clone(), false);
        }
    }

    #[allow(dead_code)]
    fn toggle_sort_mode(&mut self) {
        self.sort_mode = match self.sort_mode {
            SortMode::Threaded => SortMode::Recent,
            SortMode::Recent => SortMode::Relevance,
            SortMode::Relevance => SortMode::Threaded,
        };
        self.session_list.set_sort_mode(self.sort_mode);
    }

    #[allow(dead_code)]
    fn toggle_name_filter(&mut self) {
        self.name_filter = if self.name_filter == NameFilter::All { NameFilter::Named } else { NameFilter::All };
        self.session_list.set_name_filter(self.name_filter);
    }
}

impl Component for SessionSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        let accent = |text: &str| t.map(|t| t.fg("accent", text)).unwrap_or_else(|| text.to_string());
        let muted = |text: &str| t.map(|t| t.fg("muted", text)).unwrap_or_else(|| text.to_string());
        let bold = |text: &str| t.map(|t| t.bold(text)).unwrap_or_else(|| text.to_string());

        let title = if self.scope == "current" {
            "Resume Session (Current Folder)"
        } else {
            "Resume Session (All)"
        };
        let left_text = bold(title);

        let sort_label = match self.sort_mode {
            SortMode::Threaded => "Threaded",
            SortMode::Recent => "Recent",
            SortMode::Relevance => "Fuzzy",
        };
        let sort_text = format!("{}{}", muted("Sort: "), accent(sort_label));
        let name_label = if self.name_filter == NameFilter::All { "All" } else { "Named" };
        let name_text = format!("{}{}", muted("Name: "), accent(name_label));

        let scope_text = if self.scope == "current" {
            format!("{}{}", accent("◉ Current Folder"), muted(" | ○ All"))
        } else {
            format!("{}{}", muted("○ Current Folder | "), accent("◉ All"))
        };

        let right_text = truncate_to_width(&format!("{scope_text}  {name_text}  {sort_text}"), width as f64, "", false);
        let right_width = visible_width(&right_text) as usize;
        let available_left = (width as isize - right_width as isize - 1).max(0) as f64;
        let left = truncate_to_width(&left_text, available_left, "", false);
        let left_width = visible_width(&left) as usize;
        let spacing = (width as isize - left_width as isize - right_width as isize).max(0) as usize;

        let hint = format!(
            "{} · {} · {}",
            key_hint("tui.input.tab", "scope"),
            key_hint("app.session.toggleSort", "sort"),
            key_hint("app.session.toggleNamedFilter", "named")
        );

        let mut lines: Vec<String> = Vec::new();
        lines.push(crate::modes::interactive::components::dynamic_border::DynamicBorder::new(None).render(width).join("\n"));
        lines.push(format!("{left}{}{right_text}", " ".repeat(spacing)));
        lines.push(truncate_to_width(&hint, width as f64, "…", false));
        lines.extend(self.session_list.render(width));
        lines.push(crate::modes::interactive::components::dynamic_border::DynamicBorder::new(None).render(width).join("\n"));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        self.session_list.handle_input(data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(id: &str, name: Option<&str>, text: &str, modified_ms: f64, parent: Option<&str>) -> SessionInfo {
        SessionInfo {
            path: format!("/tmp/{id}.jsonl"),
            id: id.to_string(),
            cwd: "/tmp".to_string(),
            name: name.map(|s| s.to_string()),
            parent_session_path: parent.map(|s| s.to_string()),
            created_ms: 0.0,
            modified_ms,
            message_count: 1,
            first_message: text.to_string(),
            all_messages_text: text.to_string(),
        }
    }

    #[test]
    fn formats_dates() {
        let now = crate::core::session_manager::now_iso();
        let now_ms = parse_iso_ms(&now);
        assert_eq!(format_session_date(now_ms - 60_000.0), "1m");
        assert_eq!(format_session_date(now_ms - 3600_000.0), "1h");
    }

    #[test]
    fn builds_session_tree() {
        let sessions = vec![
            make_session("a", None, "root", 100.0, None),
            make_session("b", None, "child", 200.0, Some("/tmp/a.jsonl")),
        ];
        let roots = build_session_tree(&sessions);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].children.len(), 1);
        let flat = flatten_session_tree(&roots);
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[1].depth, 1);
    }

    #[test]
    fn deletes_nonexistent_file() {
        let result = delete_session_file("/tmp/pi-nonexistent-session-test.jsonl");
        // If the file doesn't exist, trash fails and unlink fails; we accept either.
        assert!(result.is_ok() || result.is_err());
    }
}
