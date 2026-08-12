//! Tree selector, port of `components/tree-selector.ts`.
//!
//! ponytail: the horizontal viewport panning (renderHorizontalViewport) is
//! simplified to fixed clipping; gutters/connectors are computed with the
//! same indentation rules as the JS flattenTree.

use std::sync::Arc;

use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::Component;
use pi_tui::utils::truncate_to_width;


use crate::core::messages::ContentOrText;
use crate::core::session_types::{SessionEntry, SessionMessage, SessionTreeNode};

use crate::modes::interactive::theme::theme::theme;

#[allow(dead_code)]
const TREE_GUTTER_WIDTH: usize = 2;

/// Flattened tree node for navigation.
#[derive(Clone)]
struct FlatNode {
    node: SessionTreeNode,
    /// Indentation level (each level = 3 chars).
    indent: usize,
    /// Whether to show connector (├─ or └─).
    show_connector: bool,
    /// If show_connector, true = last sibling (└─).
    is_last: bool,
    /// Gutter entries: (position level, show │).
    gutters: Vec<(usize, bool)>,
    /// True if this node is a root under a virtual branching root.
    is_virtual_root_child: bool,
}

/// Filter mode for tree display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FilterMode {
    Default,
    NoTools,
    UserOnly,
    LabeledOnly,
    All,
}

/// Tree list component with selection and ASCII art visualization.
pub struct TreeList {
    flat_nodes: Vec<FlatNode>,
    filtered_nodes: Vec<FlatNode>,
    selected_index: usize,
    current_leaf_id: Option<String>,
    max_visible_lines: usize,
    filter_mode: FilterMode,
    search_query: String,
    tool_call_map: std::collections::HashMap<String, (String, pi_protocol::Value)>,
    multiple_roots: bool,
    show_label_timestamps: bool,
    active_path_ids: std::collections::HashSet<String>,
    last_selected_id: Option<String>,
    folded_nodes: std::collections::HashSet<String>,
    pub on_select: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub on_cancel: Option<Arc<dyn Fn() + Send + Sync>>,
}

fn has_text_content(message: &SessionMessage) -> bool {
    match message {
        SessionMessage::Llm(pi_ai::types::Message::Assistant(assistant)) => {
            assistant.content.iter().any(|content| match content {
                pi_ai::types::Content::Text(text) => !text.text.trim().is_empty(),
                _ => false,
            })
        }
        SessionMessage::Llm(pi_ai::types::Message::User(user)) => match &user.content {
            pi_ai::types::UserMessageContent::Text(text) => !text.trim().is_empty(),
            pi_ai::types::UserMessageContent::Blocks(blocks) => blocks
                .iter()
                .any(|block| matches!(block, pi_ai::types::Content::Text(t) if !t.text.trim().is_empty())),
        },
        _ => true,
    }
}

fn extract_content(message: &SessionMessage) -> String {
    match message {
        SessionMessage::Llm(pi_ai::types::Message::Assistant(assistant)) => {
            let mut result = String::new();
            for content in &assistant.content {
                if let pi_ai::types::Content::Text(text) = content {
                    result.push_str(&text.text);
                }
            }
            result
        }
        SessionMessage::Llm(pi_ai::types::Message::User(user)) => match &user.content {
            pi_ai::types::UserMessageContent::Text(text) => text.clone(),
            pi_ai::types::UserMessageContent::Blocks(blocks) => {
                let mut result = String::new();
                for block in blocks {
                    if let pi_ai::types::Content::Text(text) = block {
                        result.push_str(&text.text);
                    }
                }
                result
            }
        },
        _ => String::new(),
    }
}

#[allow(dead_code)]
fn extract_full_content(message: &SessionMessage) -> String {
    extract_content(message)
}

fn message_role(message: &SessionMessage) -> &'static str {
    match message {
        SessionMessage::Llm(pi_ai::types::Message::Assistant(_)) => "assistant",
        SessionMessage::Llm(pi_ai::types::Message::User(_)) => "user",
        SessionMessage::Llm(pi_ai::types::Message::ToolResult(_)) => "toolResult",
        SessionMessage::Bash(_) => "bashExecution",
        _ => "custom",
    }
}

fn entry_is_settings_entry(entry: &SessionEntry) -> bool {
    matches!(
        entry,
        SessionEntry::Label { .. }
            | SessionEntry::Custom { .. }
            | SessionEntry::ModelChange { .. }
            | SessionEntry::ThinkingLevelChange { .. }
            | SessionEntry::SessionInfo { .. }
    )
}

impl TreeList {
    pub fn new(
        tree: Vec<SessionTreeNode>,
        current_leaf_id: Option<String>,
        max_visible_lines: usize,
        initial_selected_id: Option<String>,
        initial_filter_mode: Option<FilterMode>,
    ) -> Self {
        let multiple_roots = tree.len() > 1;
        let mut list = Self {
            flat_nodes: Vec::new(),
            filtered_nodes: Vec::new(),
            selected_index: 0,
            current_leaf_id: current_leaf_id.clone(),
            max_visible_lines,
            filter_mode: initial_filter_mode.unwrap_or(FilterMode::Default),
            search_query: String::new(),
            tool_call_map: std::collections::HashMap::new(),
            multiple_roots,
            show_label_timestamps: false,
            active_path_ids: std::collections::HashSet::new(),
            last_selected_id: None,
            folded_nodes: std::collections::HashSet::new(),
            on_select: None,
            on_cancel: None,
        };
        list.flat_nodes = list.flatten_tree(&tree);
        list.build_active_path();
        list.apply_filter();
        let target_id = initial_selected_id.or(current_leaf_id);
        list.selected_index = list.find_nearest_visible_index(target_id.as_deref());
        list.last_selected_id = list.filtered_nodes.get(list.selected_index).map(|n| n.node.entry.id().to_string());
        list
    }

    fn find_nearest_visible_index(&self, entry_id: Option<&str>) -> usize {
        if self.filtered_nodes.is_empty() {
            return 0;
        }
        let entry_map: std::collections::HashMap<String, &FlatNode> = self
            .flat_nodes
            .iter()
            .map(|node| (node.node.entry.id().to_string(), node))
            .collect();
        let visible_id_to_index: std::collections::HashMap<String, usize> = self
            .filtered_nodes
            .iter()
            .enumerate()
            .map(|(i, node)| (node.node.entry.id().to_string(), i))
            .collect();

        let mut current_id = entry_id;
        while let Some(id) = current_id {
            if let Some(index) = visible_id_to_index.get(id) {
                return *index;
            }
            let Some(node) = entry_map.get(id) else { break };
            current_id = node.node.entry.parent_id();
        }
        self.filtered_nodes.len() - 1
    }

    fn build_active_path(&mut self) {
        self.active_path_ids.clear();
        let Some(leaf_id) = &self.current_leaf_id else { return };
        let entry_map: std::collections::HashMap<String, &FlatNode> = self
            .flat_nodes
            .iter()
            .map(|node| (node.node.entry.id().to_string(), node))
            .collect();
        let mut current_id: Option<&str> = Some(leaf_id);
        while let Some(id) = current_id {
            self.active_path_ids.insert(id.to_string());
            let Some(node) = entry_map.get(id) else { break };
            current_id = node.node.entry.parent_id();
        }
    }

    /// Contains-active map computed post-order (iterative).
    fn compute_contains_active(&self, roots: &[SessionTreeNode]) -> std::collections::HashMap<String, bool> {
        let mut result = std::collections::HashMap::new();
        let mut all_nodes: Vec<&SessionTreeNode> = Vec::new();
        let mut stack: Vec<&SessionTreeNode> = roots.iter().collect();
        while let Some(node) = stack.pop() {
            all_nodes.push(node);
            for child in node.children.iter().rev() {
                stack.push(child);
            }
        }
        for node in all_nodes.iter().rev() {
            let mut has = self.current_leaf_id.as_ref().is_some_and(|leaf| node.entry.id() == leaf);
            for child in &node.children {
                if result.get(child.entry.id()).copied().unwrap_or(false) {
                    has = true;
                }
            }
            result.insert(node.entry.id().to_string(), has);
        }
        result
    }

    fn flatten_tree(&mut self, roots: &[SessionTreeNode]) -> Vec<FlatNode> {
        let mut result: Vec<FlatNode> = Vec::new();
        let contains_active = self.compute_contains_active(roots);
        let multiple_roots = roots.len() > 1;

        // Order roots: active-containing branch first.
        let mut ordered_roots: Vec<&SessionTreeNode> = roots.iter().collect();
        ordered_roots.sort_by(|a, b| {
            let a_active = contains_active.get(a.entry.id()).copied().unwrap_or(false);
            let b_active = contains_active.get(b.entry.id()).copied().unwrap_or(false);
            b_active.cmp(&a_active)
        });

        // Stack items: (node, indent, just_branched, show_connector, is_last, gutters, is_virtual_root_child)
        type StackItem = (&'static str, usize, bool, bool, bool, Vec<(usize, bool)>, bool);
        // We need owned nodes; use indices instead.
        struct StackEntry {
            node: SessionTreeNode,
            indent: usize,
            just_branched: bool,
            show_connector: bool,
            is_last: bool,
            gutters: Vec<(usize, bool)>,
            is_virtual_root_child: bool,
        }
        let mut stack: Vec<StackEntry> = Vec::new();
        for i in (0..ordered_roots.len()).rev() {
            let is_last = i == ordered_roots.len() - 1;
            stack.push(StackEntry {
                node: ordered_roots[i].clone(),
                indent: if multiple_roots { 1 } else { 0 },
                just_branched: multiple_roots,
                show_connector: multiple_roots,
                is_last,
                gutters: Vec::new(),
                is_virtual_root_child: multiple_roots,
            });
        }

        while let Some(item) = stack.pop() {
            // Collect tool calls from assistant messages.
            if let SessionEntry::Message { message, .. } = &item.node.entry {
                if message_role(message) == "assistant" {
                    if let SessionMessage::Llm(pi_ai::types::Message::Assistant(assistant)) = message {
                        for content in &assistant.content {
                            if let pi_ai::types::Content::ToolCall(tool_call) = content {
                                self.tool_call_map
                                    .insert(tool_call.id.clone(), (tool_call.name.clone(), tool_call.arguments.clone()));
                            }
                        }
                    }
                }
            }

            result.push(FlatNode {
                node: item.node.clone(),
                indent: item.indent,
                show_connector: item.show_connector,
                is_last: item.is_last,
                gutters: item.gutters.clone(),
                is_virtual_root_child: item.is_virtual_root_child,
            });

            let children = &item.node.children;
            let multiple_children = children.len() > 1;

            // Order children: active-containing first.
            let mut ordered_children: Vec<&SessionTreeNode> = children.iter().collect();
            ordered_children.sort_by(|a, b| {
                let a_active = contains_active.get(a.entry.id()).copied().unwrap_or(false);
                let b_active = contains_active.get(b.entry.id()).copied().unwrap_or(false);
                b_active.cmp(&a_active)
            });

            let child_indent = if multiple_children {
                item.indent + 1
            } else if item.just_branched && item.indent > 0 {
                item.indent + 1
            } else {
                item.indent
            };

            let connector_displayed = item.show_connector && !item.is_virtual_root_child;
            let current_display_indent = if self.multiple_roots { item.indent.saturating_sub(1) } else { item.indent };
            let connector_position = current_display_indent.saturating_sub(1);
            let mut child_gutters: Vec<(usize, bool)> = item.gutters.clone();
            if connector_displayed {
                child_gutters.push((connector_position, !item.is_last));
            }

            for i in (0..ordered_children.len()).rev() {
                let child_is_last = i == ordered_children.len() - 1;
                stack.push(StackEntry {
                    node: ordered_children[i].clone(),
                    indent: child_indent,
                    just_branched: multiple_children,
                    show_connector: multiple_children,
                    is_last: child_is_last,
                    gutters: child_gutters.clone(),
                    is_virtual_root_child: false,
                });
            }
        }
        let _ = stack;
        let _: Option<StackItem> = None;
        result
    }

    fn get_searchable_text(&self, node: &SessionTreeNode) -> String {
        let entry = &node.entry;
        let mut parts: Vec<String> = Vec::new();
        if let Some(label) = &node.label {
            parts.push(label.clone());
        }
        match entry {
            SessionEntry::Message { message, .. } => {
                parts.push(message_role(message).to_string());
                let content = extract_content(message);
                if !content.is_empty() {
                    parts.push(content);
                }
            }
            SessionEntry::CustomMessage { custom_type, content, .. } => {
                parts.push(custom_type.clone());
                let text = match content {
                    ContentOrText::Text(text) => text.clone(),
                    ContentOrText::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|block| match block {
                            pi_ai::types::Content::Text(text) => Some(text.text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                };
                parts.push(text);
            }
            SessionEntry::Compaction { .. } => parts.push("compaction".to_string()),
            SessionEntry::BranchSummary { summary, .. } => {
                parts.push("branch summary".to_string());
                parts.push(summary.clone());
            }
            SessionEntry::SessionInfo { name, .. } => {
                parts.push("title".to_string());
                if let Some(name) = name {
                    parts.push(name.clone());
                }
            }
            SessionEntry::ModelChange { model_id, .. } => {
                parts.push("model".to_string());
                parts.push(model_id.clone());
            }
            SessionEntry::ThinkingLevelChange { thinking_level, .. } => {
                parts.push("thinking".to_string());
                parts.push(thinking_level.clone());
            }
            SessionEntry::Custom { custom_type, .. } => {
                parts.push("custom".to_string());
                parts.push(custom_type.clone());
            }
            SessionEntry::Label { label, .. } => {
                parts.push("label".to_string());
                parts.push(label.clone().unwrap_or_default());
            }
        }
        parts.join(" ")
    }

    fn apply_filter(&mut self) {
        if !self.filtered_nodes.is_empty() {
            self.last_selected_id = self
                .filtered_nodes
                .get(self.selected_index)
                .map(|n| n.node.entry.id().to_string())
                .or_else(|| self.last_selected_id.clone());
        }

        let search_tokens: Vec<String> = self
            .search_query
            .to_lowercase()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();

        self.filtered_nodes = self
            .flat_nodes
            .iter()
            .filter(|flat_node| {
                let entry = &flat_node.node.entry;
                let is_current_leaf = Some(entry.id()) == self.current_leaf_id.as_deref();

                if let SessionEntry::Message { message, .. } = entry {
                    if message_role(message) == "assistant" && !is_current_leaf {
                        let has_text = has_text_content(message);
                        let is_error_or_aborted = match message {
                            SessionMessage::Llm(pi_ai::types::Message::Assistant(assistant)) => {
                                assistant.stop_reason != pi_ai::types::StopReason::Stop
                                    && assistant.stop_reason != pi_ai::types::StopReason::ToolUse
                            }
                            _ => false,
                        };
                        if !has_text && !is_error_or_aborted {
                            return false;
                        }
                    }
                }

                let is_settings_entry = entry_is_settings_entry(entry);
                let passes_filter = match self.filter_mode {
                    FilterMode::UserOnly => {
                        matches!(entry, SessionEntry::Message { message, .. } if message_role(message) == "user")
                    }
                    FilterMode::NoTools => {
                        !is_settings_entry
                            && !matches!(entry, SessionEntry::Message { message, .. } if message_role(message) == "toolResult")
                    }
                    FilterMode::LabeledOnly => flat_node.node.label.is_some(),
                    FilterMode::All => true,
                    FilterMode::Default => !is_settings_entry,
                };
                if !passes_filter {
                    return false;
                }

                if !search_tokens.is_empty() {
                    let node_text = self.get_searchable_text(&flat_node.node).to_lowercase();
                    return search_tokens.iter().all(|token| node_text.contains(token));
                }
                true
            })
            .cloned()
            .collect();

        // Filter out descendants of folded nodes.
        if !self.folded_nodes.is_empty() {
            let mut skip_set = std::collections::HashSet::new();
            for flat_node in &self.flat_nodes {
                let id = flat_node.node.entry.id().to_string();
                let parent_id = flat_node.node.entry.parent_id().map(|s| s.to_string());
                if let Some(parent_id) = parent_id {
                    if self.folded_nodes.contains(&parent_id) || skip_set.contains(&parent_id) {
                        skip_set.insert(id);
                    }
                }
            }
            self.filtered_nodes.retain(|node| !skip_set.contains(node.node.entry.id()));
        }

        if let Some(last_selected_id) = &self.last_selected_id {
            self.selected_index = self.find_nearest_visible_index(Some(last_selected_id));
        } else if self.selected_index >= self.filtered_nodes.len() {
            self.selected_index = self.filtered_nodes.len().saturating_sub(1);
        }

        if !self.filtered_nodes.is_empty() {
            self.last_selected_id = self
                .filtered_nodes
                .get(self.selected_index)
                .map(|n| n.node.entry.id().to_string())
                .or_else(|| self.last_selected_id.clone());
        }
    }

    fn get_status_labels(&self) -> String {
        let mut labels = String::new();
        match self.filter_mode {
            FilterMode::NoTools => labels += " [no-tools]",
            FilterMode::UserOnly => labels += " [user]",
            FilterMode::LabeledOnly => labels += " [labeled]",
            FilterMode::All => labels += " [all]",
            _ => {}
        }
        if self.show_label_timestamps {
            labels += " [+label time]";
        }
        labels
    }

    pub fn set_filter_mode(&mut self, mode: FilterMode) {
        self.filter_mode = mode;
        self.apply_filter();
    }

    pub fn get_filter_mode(&self) -> FilterMode {
        self.filter_mode
    }

    pub fn get_search_query(&self) -> &str {
        &self.search_query
    }

    pub fn set_search_query(&mut self, query: &str) {
        self.search_query = query.to_string();
        self.apply_filter();
    }

    pub fn toggle_label_timestamps(&mut self) {
        self.show_label_timestamps = !self.show_label_timestamps;
    }

    pub fn get_selected_id(&self) -> Option<String> {
        self.filtered_nodes.get(self.selected_index).map(|n| n.node.entry.id().to_string())
    }

    pub fn fold_selected(&mut self) {
        let Some(id) = self.get_selected_id() else { return };
        if self.folded_nodes.contains(&id) {
            self.folded_nodes.remove(&id);
        } else {
            self.folded_nodes.insert(id);
        }
        self.apply_filter();
    }

    fn format_tool_call(&self, name: &str, args: &pi_protocol::Value) -> String {
        let get_str = |key: &str| -> String {
            match args.as_map() {
                Some(entries) => entries
                    .iter()
                    .find(|(k, _)| k == key)
                    .and_then(|(_, v)| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                None => String::new(),
            }
        };
        let home = std::env::var("HOME").unwrap_or_default();
        let shorten_path = |p: &str| -> String {
            if !home.is_empty() && p.starts_with(&home) {
                format!("~{}", &p[home.len()..])
            } else {
                p.to_string()
            }
        };
        match name {
            "read" => {
                let path = shorten_path(&get_str("path"));
                let offset = args.as_map().and_then(|e| e.iter().find(|(k, _)| k == "offset").and_then(|(_, v)| v.as_number()));
                let limit = args.as_map().and_then(|e| e.iter().find(|(k, _)| k == "limit").and_then(|(_, v)| v.as_number()));
                let display = if offset.is_some() || limit.is_some() {
                    let start = offset.unwrap_or(1.0) as i64;
                    let end = match limit {
                        Some(limit) => format!("{}-{}", start, start + limit as i64 - 1),
                        None => String::new(),
                    };
                    format!("{path}:{start}{end}")
                } else {
                    path
                };
                format!("[read: {display}]")
            }
            "write" => format!("[write: {}]", shorten_path(&get_str("path"))),
            "edit" => format!("[edit: {}]", shorten_path(&get_str("path"))),
            "bash" => {
                let raw_cmd = get_str("command");
                let cmd: String = raw_cmd
                    .replace(['\n', '\t'], " ")
                    .trim()
                    .chars()
                    .take(50)
                    .collect();
                let ellipsis = if raw_cmd.len() > 50 { "..." } else { "" };
                format!("[bash: {cmd}{ellipsis}]")
            }
            "grep" => format!("[grep: /{}/ in {}]", get_str("pattern"), shorten_path(&get_str("path"))),
            "find" => format!("[find: {} in {}]", get_str("pattern"), shorten_path(&get_str("path"))),
            "ls" => format!("[ls: {}]", shorten_path(&get_str("path"))),
            _ => {
                let args_str = pi_ai::utils::json::json_stringify(args);
                let truncated: String = args_str.chars().take(40).collect();
                let ellipsis = if args_str.len() > 40 { "..." } else { "" };
                format!("[{name}: {truncated}{ellipsis}]")
            }
        }
    }

    fn get_entry_display_text(&self, node: &SessionTreeNode, is_selected: bool) -> String {
        let t = theme();
        let t = t.as_ref();
        let entry = &node.entry;
        let normalize = |s: &str| -> String { s.replace(['\n', '\t'], " ").trim().to_string() };

        let result = match entry {
            SessionEntry::Message { message, .. } => {
                let role = message_role(message);
                match role {
                    "user" => {
                        let content = normalize(&extract_content(message));
                        let label = t.map(|t| t.fg("accent", "user: ")).unwrap_or_else(|| "user: ".to_string());
                        format!("{label}{content}")
                    }
                    "assistant" => {
                        let label = t.map(|t| t.fg("success", "assistant: ")).unwrap_or_else(|| "assistant: ".to_string());
                        let text_content = normalize(&extract_content(message));
                        if !text_content.is_empty() {
                            format!("{label}{text_content}")
                        } else if let SessionMessage::Llm(pi_ai::types::Message::Assistant(assistant)) = message {
                            if assistant.stop_reason == pi_ai::types::StopReason::Aborted {
                                format!("{label}{}", t.map(|t| t.fg("muted", "(aborted)")).unwrap_or_else(|| "(aborted)".to_string()))
                            } else if let Some(error) = &assistant.error_message {
                                let err: String = normalize(error).chars().take(80).collect();
                                format!("{label}{}", t.map(|t| t.fg("error", &err)).unwrap_or(err))
                            } else {
                                format!("{label}{}", t.map(|t| t.fg("muted", "(no content)")).unwrap_or_else(|| "(no content)".to_string()))
                            }
                        } else {
                            format!("{label}{}", t.map(|t| t.fg("muted", "(no content)")).unwrap_or_else(|| "(no content)".to_string()))
                        }
                    }
                    "toolResult" => {
                        if let SessionMessage::Llm(pi_ai::types::Message::ToolResult(tool_result)) = message {
                            if let Some((name, args)) = self.tool_call_map.get(&tool_result.tool_call_id) {
                                t.map(|t| t.fg("muted", &self.format_tool_call(name, args)))
                                    .unwrap_or_else(|| self.format_tool_call(name, args))
                            } else {
                                let name = tool_result.tool_name.clone();
                                t.map(|t| t.fg("muted", &format!("[{name}]"))).unwrap_or_else(|| format!("[{name}]"))
                            }
                        } else {
                            t.map(|t| t.fg("muted", "[tool]")).unwrap_or_else(|| "[tool]".to_string())
                        }
                    }
                    "bashExecution" => {
                        let command = match message {
                            SessionMessage::Bash(bash) => bash.command.clone(),
                            _ => String::new(),
                        };
                        t.map(|t| t.fg("dim", &format!("[bash]: {}", normalize(&command))))
                            .unwrap_or_else(|| format!("[bash]: {}", normalize(&command)))
                    }
                    _ => t.map(|t| t.fg("dim", &format!("[{role}]"))).unwrap_or_else(|| format!("[{role}]")),
                }
            }
            SessionEntry::CustomMessage { custom_type, content, .. } => {
                let text = match content {
                    ContentOrText::Text(text) => text.clone(),
                    ContentOrText::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|block| match block {
                            pi_ai::types::Content::Text(text) => Some(text.text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(""),
                };
                let label = t.map(|t| t.fg("customMessageLabel", &format!("[{custom_type}]: ")))
                    .unwrap_or_else(|| format!("[{custom_type}]: "));
                format!("{label}{}", normalize(&text))
            }
            SessionEntry::Compaction { tokens_before, .. } => {
                let tokens = (tokens_before / 1000.0).round() as i64;
                t.map(|t| t.fg("borderAccent", &format!("[compaction: {tokens}k tokens]")))
                    .unwrap_or_else(|| format!("[compaction: {tokens}k tokens]"))
            }
            SessionEntry::BranchSummary { summary, .. } => {
                let label = t.map(|t| t.fg("warning", "[branch summary]: ")).unwrap_or_else(|| "[branch summary]: ".to_string());
                format!("{label}{}", normalize(summary))
            }
            SessionEntry::ModelChange { model_id, .. } => {
                t.map(|t| t.fg("dim", &format!("[model: {model_id}]"))).unwrap_or_else(|| format!("[model: {model_id}]"))
            }
            SessionEntry::ThinkingLevelChange { thinking_level, .. } => {
                t.map(|t| t.fg("dim", &format!("[thinking: {thinking_level}]")))
                    .unwrap_or_else(|| format!("[thinking: {thinking_level}]"))
            }
            SessionEntry::Custom { custom_type, .. } => {
                t.map(|t| t.fg("dim", &format!("[custom: {custom_type}]"))).unwrap_or_else(|| format!("[custom: {custom_type}]"))
            }
            SessionEntry::Label { label, .. } => {
                t.map(|t| t.fg("dim", &format!("[label: {}]", label.clone().unwrap_or_else(|| "(cleared)".to_string()))))
                    .unwrap_or_else(|| format!("[label: {}]", label.clone().unwrap_or_else(|| "(cleared)".to_string())))
            }
            SessionEntry::SessionInfo { name, .. } => match name {
                Some(name) => format!("[title: {name}]"),
                None => format!("[title: {}]", t.map(|t| t.italic("empty")).unwrap_or_else(|| "empty".to_string())),
            },
        };

        if is_selected {
            t.map(|t| t.bold(&result)).unwrap_or(result)
        } else {
            result
        }
    }

    fn render_rows(&self, width: usize) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        if self.filtered_nodes.is_empty() {
            let empty = t.map(|t| t.fg("muted", "  No entries found")).unwrap_or_else(|| "  No entries found".to_string());
            let status = format!("  (0/0){}", self.get_status_labels());
            let status = t.map(|t| t.fg("muted", &status)).unwrap_or(status);
            return vec![truncate_to_width(&empty, width as f64, "", false), truncate_to_width(&status, width as f64, "", false)];
        }

        let start_index = (self.selected_index as isize - (self.max_visible_lines as isize / 2))
            .max(0)
            .min((self.filtered_nodes.len() as isize - self.max_visible_lines as isize).max(0))
            .max(0) as usize;
        let end_index = (start_index + self.max_visible_lines).min(self.filtered_nodes.len());

        let mut lines: Vec<String> = Vec::new();
        for i in start_index..end_index {
            let flat_node = &self.filtered_nodes[i];
            let entry = &flat_node.node.entry;
            let is_selected = i == self.selected_index;

            let cursor = if is_selected {
                t.map(|t| t.fg("accent", "› ")).unwrap_or_else(|| "› ".to_string())
            } else {
                "  ".to_string()
            };

            let display_indent = if self.multiple_roots { flat_node.indent.saturating_sub(1) } else { flat_node.indent };
            let connector = if flat_node.show_connector && !flat_node.is_virtual_root_child {
                if flat_node.is_last { "└─ " } else { "├─ " }
            } else {
                ""
            };
            let connector_position = if connector.is_empty() {
                usize::MAX
            } else {
                display_indent.saturating_sub(1)
            };

            let total_chars = display_indent * 3;
            let mut prefix_chars: Vec<char> = Vec::new();
            let is_folded = self.folded_nodes.contains(entry.id());
            for index in 0..total_chars {
                let level = index / 3;
                let pos_in_level = index % 3;
                let gutter = flat_node.gutters.iter().find(|(position, _)| *position == level);
                if let Some((_, show)) = gutter {
                    if pos_in_level == 0 {
                        prefix_chars.push(if *show { '│' } else { ' ' });
                    } else {
                        prefix_chars.push(' ');
                    }
                } else if !connector.is_empty() && level == connector_position {
                    if pos_in_level == 0 {
                        prefix_chars.push(if flat_node.is_last { '└' } else { '├' });
                    } else if pos_in_level == 1 {
                        prefix_chars.push(if is_folded { '⊞' } else { '⊟' });
                    } else {
                        prefix_chars.push(' ');
                    }
                } else {
                    prefix_chars.push(' ');
                }
            }
            let prefix: String = prefix_chars.iter().collect();

            let shows_fold_in_connector = flat_node.show_connector && !flat_node.is_virtual_root_child;
            let fold_marker = if is_folded && !shows_fold_in_connector {
                t.map(|t| t.fg("accent", "⊞ ")).unwrap_or_else(|| "⊞ ".to_string())
            } else {
                String::new()
            };

            let is_on_active_path = self.active_path_ids.contains(entry.id());
            let path_marker = if is_on_active_path {
                t.map(|t| t.fg("accent", "• ")).unwrap_or_else(|| "• ".to_string())
            } else {
                String::new()
            };

            let label = if let Some(label) = &flat_node.node.label {
                t.map(|t| t.fg("warning", &format!("[{label}] "))).unwrap_or_else(|| format!("[{label}] "))
            } else {
                String::new()
            };

            let content = self.get_entry_display_text(&flat_node.node, is_selected);
            let prefix_part = t.map(|t| t.fg("dim", &prefix)).unwrap_or(prefix);
            let mut gutter = cursor;
            let mut body = format!("{prefix_part}{fold_marker}{path_marker}{label}{content}");
            if is_selected {
                let bg = t.map(|t| t.get_bg_ansi("selectedBg")).unwrap_or_default();
                if !bg.is_empty() {
                    gutter = format!("{bg}{gutter}\x1b[49m");
                    body = format!("{bg}{body}\x1b[49m");
                }
            }
            let line = format!("{gutter}{body}");
            lines.push(truncate_to_width(&line, width as f64, "", false));
        }

        let status = format!("  ({}/{}){}", self.selected_index + 1, self.filtered_nodes.len(), self.get_status_labels());
        let status = t.map(|t| t.fg("muted", &status)).unwrap_or(status);
        lines.push(truncate_to_width(&status, width as f64, "", false));
        lines
    }
}

impl Component for TreeList {
    fn render(&self, width: usize) -> Vec<String> {
        self.render_rows(width)
    }

    fn handle_input(&mut self, key_data: &str) {
        let kb = get_keybindings();
        let manager = match &*kb {
            Some(manager) => manager,
            None => return,
        };
        if manager.matches(key_data, "tui.select.up") || manager.matches(key_data, "app.tree.foldOrUp") {
            if manager.matches(key_data, "app.tree.foldOrUp") && !manager.matches(key_data, "tui.select.up") {
                // fold vs up is disambiguated elsewhere; treat as fold when folded
                let id = self.get_selected_id();
                if let Some(id) = id {
                    if self.folded_nodes.contains(&id) {
                        self.folded_nodes.remove(&id);
                        self.apply_filter();
                        return;
                    }
                }
            }
            if self.filtered_nodes.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index == 0 {
                self.filtered_nodes.len() - 1
            } else {
                self.selected_index - 1
            };
        } else if manager.matches(key_data, "tui.select.down") || manager.matches(key_data, "app.tree.unfoldOrDown") {
            if manager.matches(key_data, "app.tree.unfoldOrDown") && !manager.matches(key_data, "tui.select.down") {
                let id = self.get_selected_id();
                if let Some(id) = id {
                    if !self.folded_nodes.contains(&id) {
                        // Fold if it has children (simplified: mark folded).
                        let has_children = self.flat_nodes.iter().any(|n| n.node.entry.parent_id() == Some(id.as_str()));
                        if has_children {
                            self.folded_nodes.insert(id);
                            self.apply_filter();
                            return;
                        }
                    }
                }
            }
            if self.filtered_nodes.is_empty() {
                return;
            }
            self.selected_index = if self.selected_index + 1 >= self.filtered_nodes.len() {
                0
            } else {
                self.selected_index + 1
            };
        } else if manager.matches(key_data, "tui.select.confirm") {
            if let Some(id) = self.get_selected_id() {
                if let Some(on_select) = &self.on_select {
                    on_select(&id);
                }
            }
        } else if manager.matches(key_data, "tui.select.cancel") {
            if let Some(on_cancel) = &self.on_cancel {
                on_cancel();
            }
        } else if manager.matches(key_data, "app.session.toggleLabelTimestamp") {
            self.toggle_label_timestamps();
        }
    }
}

/// Wrapper component with borders (matches TreeSelectorComponent usage).
pub struct TreeSelectorComponent {
    inner: TreeList,
}

impl TreeSelectorComponent {
    pub fn new(
        tree: Vec<SessionTreeNode>,
        current_leaf_id: Option<String>,
        max_visible_lines: usize,
        initial_selected_id: Option<String>,
        initial_filter_mode: Option<FilterMode>,
    ) -> Self {
        Self {
            inner: TreeList::new(tree, current_leaf_id, max_visible_lines, initial_selected_id, initial_filter_mode),
        }
    }

    pub fn get_tree_list(&self) -> &TreeList {
        &self.inner
    }

    pub fn get_tree_list_mut(&mut self) -> &mut TreeList {
        &mut self.inner
    }

    /// Wire selection/cancel callbacks into the underlying list.
    pub fn set_callbacks(
        &mut self,
        on_select: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
    ) {
        self.inner.on_select = Some(on_select);
        self.inner.on_cancel = Some(on_cancel);
    }
}

impl Component for TreeSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        lines.push(crate::modes::interactive::components::dynamic_border::DynamicBorder::new(None).render(width).join("\n"));
        lines.extend(self.inner.render(width));
        lines.push(crate::modes::interactive::components::dynamic_border::DynamicBorder::new(None).render(width).join("\n"));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        self.inner.handle_input(data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(id: &str, parent: Option<&str>) -> SessionEntry {
        SessionEntry::Message {
            base: crate::core::session_types::SessionEntryBase {
                id: id.to_string(),
                parent_id: parent.map(|s| s.to_string()),
                timestamp: String::new(),
            },
            message: SessionMessage::Llm(pi_ai::types::Message::User(pi_ai::types::UserMessage {
                content: pi_ai::types::UserMessageContent::Text("hello".to_string()),
                timestamp: 0.0,
            })),
        }
    }

    #[test]
    fn flattens_tree_with_indent() {
        let root = SessionTreeNode {
            entry: make_entry("root", None),
            children: vec![SessionTreeNode {
                entry: make_entry("child", Some("root")),
                children: vec![],
                label: None,
                label_timestamp: None,
            }],
            label: None,
            label_timestamp: None,
        };
        let list = TreeList::new(vec![root], None, 10, None, None);
        assert_eq!(list.flat_nodes.len(), 2);
        assert_eq!(list.filtered_nodes.len(), 2);
    }

    #[test]
    fn filter_modes() {
        let root = SessionTreeNode {
            entry: make_entry("root", None),
            children: vec![],
            label: None,
            label_timestamp: None,
        };
        let mut list = TreeList::new(vec![root], None, 10, None, None);
        list.set_filter_mode(FilterMode::UserOnly);
        assert_eq!(list.filtered_nodes.len(), 1);
        list.set_filter_mode(FilterMode::All);
        assert_eq!(list.filtered_nodes.len(), 1);
    }
}
