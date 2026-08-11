//! Port of `packages/ai/src/utils/deferred-tools.ts`.

use crate::types::{Context, Message, Tool};

type ToolNameNormalizer = Box<dyn Fn(&str) -> String>;

pub fn identity_tool_name(name: &str) -> String {
    name.to_string()
}

#[derive(Clone, Debug, PartialEq)]
pub struct SplitDeferredTools {
    pub immediate: Vec<Tool>,
    pub deferred: Vec<(String, Tool)>,
}

/// Split current tools into prefix and transcript-loaded definitions.
/// Mirrors `splitDeferredTools`: the returned `deferred` map preserves
/// insertion order (Vec here).
pub fn split_deferred_tools(
    context: &Context,
    enabled: bool,
    normalize_name: Option<ToolNameNormalizer>,
) -> SplitDeferredTools {
    let normalize = normalize_name.unwrap_or_else(|| Box::new(identity_tool_name) as ToolNameNormalizer);

    // Unique tools by normalized name, first occurrence wins (JS Map.set).
    let mut unique_tools: Vec<(String, Tool)> = Vec::new();
    for tool in context.tools.as_deref().unwrap_or(&[]) {
        let name = normalize(&tool.name);
        if !unique_tools.iter().any(|(existing, _)| existing == &name) {
            unique_tools.push((name, tool.clone()));
        }
    }
    if !enabled {
        return SplitDeferredTools {
            immediate: unique_tools.into_iter().map(|(_, tool)| tool).collect(),
            deferred: Vec::new(),
        };
    }

    let mut deferred_names = std::collections::HashSet::new();
    let mut used_names = std::collections::HashSet::new();
    for message in &context.messages {
        match message {
            Message::Assistant(assistant) => {
                for block in &assistant.content {
                    if let crate::types::Content::ToolCall(tool_call) = block {
                        used_names.insert(normalize(&tool_call.name));
                    }
                }
            }
            Message::ToolResult(tool) => {
                for name in tool.added_tool_names.iter().flatten() {
                    let normalized = normalize(name);
                    if !used_names.contains(&normalized) {
                        deferred_names.insert(normalized);
                    }
                }
            }
            _ => {}
        }
    }

    let mut immediate = Vec::new();
    let mut deferred = Vec::new();
    for (name, tool) in unique_tools {
        if deferred_names.contains(&name) {
            deferred.push((name, tool));
        } else {
            immediate.push(tool);
        }
    }
    SplitDeferredTools { immediate, deferred }
}
