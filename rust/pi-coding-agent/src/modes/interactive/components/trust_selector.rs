//! Project trust selector, port of `components/trust-selector.ts`.

use std::sync::Arc;

use pi_tui::components::basic::Text;
use pi_tui::tui::Container;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::Component;

use crate::core::trust_manager::{get_project_trust_options, ProjectTrustOption, ProjectTrustStoreEntry};
use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::keybinding_hints::{key_hint, raw_key_hint};
use crate::modes::interactive::theme::theme::theme;

pub struct TrustSelection {
    pub trusted: bool,
    pub updates: Vec<crate::core::trust_manager::ProjectTrustUpdate>,
}

pub struct TrustSelectorOptions {
    pub cwd: String,
    pub saved_decision: Option<ProjectTrustStoreEntry>,
    pub project_trusted: bool,
    pub on_select: Arc<dyn Fn(TrustSelection) + Send + Sync>,
    pub on_cancel: Arc<dyn Fn() + Send + Sync>,
}

fn format_decision(trust_path: Option<&str>, decision: Option<&ProjectTrustStoreEntry>) -> String {
    let Some(decision) = decision else {
        return "none".to_string();
    };
    let label = if decision.decision { "trusted" } else { "untrusted" };
    if trust_path.is_some() && decision.path != trust_path.unwrap() {
        format!("{label} (inherited from {})", decision.path)
    } else {
        format!("{label} ({})", decision.path)
    }
}

/// Component that renders a project trust selector.
pub struct TrustSelectorComponent {
    list_container: Container,
    trust_options: Vec<ProjectTrustOption>,
    selected_index: usize,
    saved_decision: Option<ProjectTrustStoreEntry>,
    on_select: Arc<dyn Fn(TrustSelection) + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
    cwd: String,
    project_trusted: bool,
}

impl TrustSelectorComponent {
    pub fn new(options: TrustSelectorOptions) -> Self {
        let trust_options = get_project_trust_options(&options.cwd, true);
        let selected_index = trust_options
            .iter()
            .position(|option| Self::is_saved_option(option, options.saved_decision.as_ref()))
            .unwrap_or(0);
        let mut component = Self {
            list_container: Container::new(),
            trust_options,
            selected_index,
            saved_decision: options.saved_decision,
            on_select: options.on_select,
            on_cancel: options.on_cancel,
            cwd: options.cwd,
            project_trusted: options.project_trusted,
        };
        component.update_list();
        component
    }

    fn is_saved_option(option: &ProjectTrustOption, saved: Option<&ProjectTrustStoreEntry>) -> bool {
        let Some(saved) = saved else {
            return false;
        };
        option.saved_path.is_some()
            && saved.decision == option.trusted
            && saved.path == option.saved_path.as_deref().unwrap_or("")
    }

    fn update_list(&mut self) {
        self.list_container.clear();
        let t = theme();
        let t = t.as_ref();
        for (i, option) in self.trust_options.iter().enumerate() {
            let is_selected = i == self.selected_index;
            let is_current = Self::is_saved_option(option, self.saved_decision.as_ref());
            let checkmark = if is_current {
                t.map(|t| t.fg("success", " ✓")).unwrap_or_else(|| " ✓".to_string())
            } else {
                String::new()
            };
            let prefix = if is_selected {
                t.map(|t| t.fg("accent", "→ ")).unwrap_or_else(|| "→ ".to_string())
            } else {
                "  ".to_string()
            };
            let label = if is_selected {
                t.map(|t| t.fg("accent", &option.label)).unwrap_or_else(|| option.label.clone())
            } else {
                t.map(|t| t.fg("text", &option.label)).unwrap_or_else(|| option.label.clone())
            };
            let line = format!("{prefix}{label}{checkmark}");
            self.list_container.add_child(Arc::new(Text::new(&line, 1, 0, None)));
        }
    }
}

impl Component for TrustSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        let accent = |text: &str| t.map(|t| t.fg("accent", text)).unwrap_or_else(|| text.to_string());
        let muted = |text: &str| t.map(|t| t.fg("muted", text)).unwrap_or_else(|| text.to_string());

        let mut lines: Vec<String> = Vec::new();
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines.push(format!(" {}", accent("Project trust")));
        lines.push(format!(" {}", muted(&self.cwd)));
        let saved_path = self.trust_options.first().and_then(|option| option.saved_path.clone());
        lines.push(format!(
            " {}",
            muted(&format!(
                "Saved decision: {}",
                format_decision(saved_path.as_deref(), self.saved_decision.as_ref())
            ))
        ));
        lines.push(format!(
            " {}",
            muted(&format!(
                "Current session: {}",
                if self.project_trusted { "trusted" } else { "untrusted" }
            ))
        ));
        lines.extend(self.list_container.render(width));
        let hint = format!(
            "{}  {}  {}",
            raw_key_hint("↑↓", "navigate"),
            key_hint("tui.select.confirm", "save"),
            key_hint("tui.select.cancel", "cancel")
        );
        lines.push(format!(" {hint}"));
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = get_keybindings();
        let manager = match &*kb {
            Some(manager) => manager,
            None => return,
        };
        if manager.matches(data, "tui.select.up") || data == "k" {
            self.selected_index = self.selected_index.saturating_sub(1);
            self.update_list();
        } else if manager.matches(data, "tui.select.down") || data == "j" {
            self.selected_index = (self.selected_index + 1).min(self.trust_options.len().saturating_sub(1));
            self.update_list();
        } else if manager.matches(data, "tui.select.confirm") || data == "\n" {
            if let Some(selected) = self.trust_options.get(self.selected_index) {
                let selection = TrustSelection {
                    trusted: selected.trusted,
                    updates: selected.updates.clone(),
                };
                (self.on_select)(selection);
            }
        } else if manager.matches(data, "tui.select.cancel") {
            (self.on_cancel)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_options() {
        let component = TrustSelectorComponent::new(TrustSelectorOptions {
            cwd: "/tmp".to_string(),
            saved_decision: None,
            project_trusted: false,
            on_select: Arc::new(|_| {}),
            on_cancel: Arc::new(|| {}),
        });
        let lines = component.render(60);
        assert!(lines.iter().any(|line| line.contains("Project trust")));
        assert!(lines.iter().any(|line| line.contains("Trust")));
    }
}
