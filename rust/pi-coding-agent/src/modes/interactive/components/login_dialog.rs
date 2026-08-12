//! Login dialog component, port of `components/login-dialog.ts`.
//!
//! ponytail: openBrowser is a no-op (no browser spawning); promise-based
//! input resolution is replaced with an explicit pending-input flag the
//! host polls (sync analog).

use std::sync::Arc;

use pi_tui::components::basic::Text;
use pi_tui::tui::Container;
use pi_tui::components::input::Input;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::Component;

use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::keybinding_hints::key_hint;
use crate::modes::interactive::theme::theme::theme;

pub struct OAuthDeviceCodeInfo {
    pub verification_uri: String,
    pub user_code: String,
}

pub struct AuthInfoLink {
    pub label: Option<String>,
    pub url: String,
}

/// Login dialog component - replaces editor during OAuth login flow.
pub struct LoginDialogComponent {
    content: Container,
    input: Input,
    on_complete: Arc<dyn Fn(bool, Option<String>) + Send + Sync>,
    pub pending_input: Option<String>, // host resolves this after handle_input
    cancelled: bool,
}

impl LoginDialogComponent {
    pub fn new(
        provider_id: &str,
        on_complete: Arc<dyn Fn(bool, Option<String>) + Send + Sync>,
        provider_name_override: Option<&str>,
        title_override: Option<&str>,
    ) -> Self {
        let provider_name = provider_name_override.unwrap_or(provider_id);
        let title = title_override.map(|s| s.to_string()).unwrap_or_else(|| format!("Login to {provider_name}"));

        let mut content = Container::new();
        let title_styled = theme()
            .as_ref()
            .map(|t| t.bold(&t.fg("accent", &title)))
            .unwrap_or(title.clone());
        content.add_child(Arc::new(Text::new(&title_styled, 1, 0, None)));

        let input = Input::new();

        Self {
            content,
            input,
            on_complete,
            pending_input: None,
            cancelled: false,
        }
    }

    fn cancel(&mut self) {
        if self.cancelled {
            return;
        }
        self.cancelled = true;
        (self.on_complete)(false, Some("Login cancelled".to_string()));
    }

    /// Show URL and optional instructions (onAuth callback).
    pub fn show_auth(&mut self, url: &str, instructions: Option<&str>) {
        self.content.clear();
        let linked_url = format!("\x1b]8;;{url}\x07{url}\x1b]8;;\x07");
        self.content.add_child(Arc::new(Text::new(&linked_url, 1, 0, None)));
        let click_hint = if cfg!(target_os = "macos") { "Cmd+click to open" } else { "Ctrl+click to open" };
        let hyperlink = format!("\x1b]8;;{url}\x07{click_hint}\x1b]8;;\x07");
        self.content.add_child(Arc::new(Text::new(&hyperlink, 1, 0, None)));
        if let Some(instructions) = instructions {
            let styled = theme()
                .as_ref()
                .map(|t| t.fg("warning", instructions))
                .unwrap_or_else(|| instructions.to_string());
            self.content.add_child(Arc::new(Text::new(&styled, 1, 0, None)));
        }
    }

    /// Show URL and user code (onDeviceCode callback).
    pub fn show_device_code(&mut self, info: &OAuthDeviceCodeInfo) {
        self.content.clear();
        let linked_url = format!("\x1b]8;;{}\x07{}\x1b]8;;\x07", info.verification_uri, info.verification_uri);
        self.content.add_child(Arc::new(Text::new(&linked_url, 1, 0, None)));
        let click_hint = if cfg!(target_os = "macos") { "Cmd+click to open" } else { "Ctrl+click to open" };
        let hyperlink = format!("\x1b]8;;{}\x07{click_hint}\x1b]8;;\x07", info.verification_uri);
        self.content.add_child(Arc::new(Text::new(&hyperlink, 1, 0, None)));
        let code_text = format!("Enter code: {}", info.user_code);
        let styled = theme()
            .as_ref()
            .map(|t| t.fg("warning", &code_text))
            .unwrap_or(code_text);
        self.content.add_child(Arc::new(Text::new(&styled, 1, 0, None)));
    }

    /// Show input for manual code/URL entry; returns Some(value) when the
    /// host has submitted (pending_input set by handle_input).
    pub fn show_manual_input(&mut self, prompt: &str) {
        self.input.set_value("");
        self.pending_input = None;
        self.content.add_child(Arc::new(Text::new(prompt, 1, 0, None)));
        self.content.add_child(Arc::new(Input::new()));
        let hint = format!("({} to cancel)", key_hint("tui.select.cancel", ""));
        self.content.add_child(Arc::new(Text::new(&hint, 1, 0, None)));
    }

    /// Show prompt and wait for input (does not clear content).
    pub fn show_prompt(&mut self, message: &str, placeholder: Option<&str>) {
        self.pending_input = None;
        let styled = theme()
            .as_ref()
            .map(|t| t.fg("text", message))
            .unwrap_or_else(|| message.to_string());
        self.content.add_child(Arc::new(Text::new(&styled, 1, 0, None)));
        if let Some(placeholder) = placeholder {
            let styled = format!("e.g., {placeholder}");
            let dim = theme()
                .as_ref()
                .map(|t| t.fg("dim", &styled))
                .unwrap_or(styled);
            self.content.add_child(Arc::new(Text::new(&dim, 1, 0, None)));
        }
        self.input.set_value("");
        self.content.add_child(Arc::new(Input::new()));
        let hint = format!(
            "({} to cancel, {} to submit)",
            key_hint("tui.select.cancel", ""),
            key_hint("tui.select.confirm", "")
        );
        self.content.add_child(Arc::new(Text::new(&hint, 1, 0, None)));
    }

    /// Show informational text before another login step.
    pub fn show_details(&mut self, lines: &[String]) {
        self.content.clear();
        for line in lines {
            self.content.add_child(Arc::new(Text::new(line, 1, 0, None)));
        }
    }

    /// Show provider-owned info and links.
    pub fn show_info(&mut self, message: &str, links: &[AuthInfoLink], show_close_hint: bool) {
        let styled = theme()
            .as_ref()
            .map(|t| t.fg("text", message))
            .unwrap_or_else(|| message.to_string());
        self.content.add_child(Arc::new(Text::new(&styled, 1, 0, None)));
        for link in links {
            let text = match &link.label {
                Some(label) => format!("{label}: {}", link.url),
                None => link.url.clone(),
            };
            let hyperlink = format!("\x1b]8;;{}\x07{text}\x1b]8;;\x07", link.url);
            let accent = theme()
                .as_ref()
                .map(|t| t.fg("accent", &hyperlink))
                .unwrap_or(hyperlink);
            self.content.add_child(Arc::new(Text::new(&accent, 1, 0, None)));
        }
        if show_close_hint {
            let hint = format!("({} to close)", key_hint("tui.select.cancel", ""));
            self.content.add_child(Arc::new(Text::new(&hint, 1, 0, None)));
        }
    }

    /// Show waiting message (polling flows).
    pub fn show_waiting(&mut self, message: &str) {
        let styled = theme()
            .as_ref()
            .map(|t| t.fg("dim", message))
            .unwrap_or_else(|| message.to_string());
        self.content.add_child(Arc::new(Text::new(&styled, 1, 0, None)));
        let hint = format!("({} to cancel)", key_hint("tui.select.cancel", ""));
        self.content.add_child(Arc::new(Text::new(&hint, 1, 0, None)));
    }

    /// Show progress message.
    pub fn show_progress(&mut self, message: &str) {
        let styled = theme()
            .as_ref()
            .map(|t| t.fg("dim", message))
            .unwrap_or_else(|| message.to_string());
        self.content.add_child(Arc::new(Text::new(&styled, 1, 0, None)));
    }
}

impl Component for LoginDialogComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines.extend(self.content.render(width));
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = get_keybindings();
        let manager = match &*kb {
            Some(manager) => manager,
            None => return,
        };
        if manager.matches(data, "tui.select.cancel") {
            self.cancel();
            return;
        }
        if manager.matches(data, "tui.select.confirm") || data == "\n" {
            self.pending_input = Some(self.input.get_value().to_string());
            return;
        }
        self.input.handle_input(data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_dialog() {
        let component = LoginDialogComponent::new(
            "acme",
            Arc::new(|_, _| {}),
            None,
            None,
        );
        let lines = component.render(60);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|line| line.contains("Login to acme")));
    }
}
