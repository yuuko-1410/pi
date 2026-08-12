//! Auth provider selector, port of `components/oauth-selector.ts`.

use std::sync::Arc;

use pi_tui::components::input::Input;
use pi_tui::fuzzy::fuzzy_filter;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::Component;
use pi_tui::utils::truncate_to_width;

use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::theme::theme::theme;

#[derive(Clone, Debug, PartialEq)]
pub enum AuthSelectorAuthType {
    OAuth,
    ApiKey,
}

#[derive(Clone, Debug)]
pub struct AuthStatus {
    pub auth_type: String,
    pub source: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AuthSelectorProvider {
    pub id: String,
    pub name: String,
    pub auth_type: AuthSelectorAuthType,
    pub status: Option<AuthStatus>,
}

pub fn format_auth_selector_provider_type(auth_type: &AuthSelectorAuthType) -> String {
    match auth_type {
        AuthSelectorAuthType::OAuth => "subscription".to_string(),
        AuthSelectorAuthType::ApiKey => "API key".to_string(),
    }
}

/// Component that renders an auth provider selector.
pub struct OAuthSelectorComponent {
    search_input: Arc<Input>,
    all_providers: Vec<AuthSelectorProvider>,
    filtered_providers: Vec<AuthSelectorProvider>,
    selected_index: usize,
    mode: &'static str, // "login" | "logout"
    on_select: Arc<dyn Fn(&str, AuthSelectorAuthType) + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
    show_auth_type_labels: bool,
    focused: bool,
}

impl OAuthSelectorComponent {
    pub fn new(
        mode: &'static str,
        providers: Vec<AuthSelectorProvider>,
        on_select: Arc<dyn Fn(&str, AuthSelectorAuthType) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        initial_search_input: Option<&str>,
    ) -> Self {
        let mut auth_types: Vec<AuthSelectorAuthType> = Vec::new();
        for provider in &providers {
            if !auth_types.contains(&provider.auth_type) {
                auth_types.push(provider.auth_type.clone());
            }
        }
        let show_auth_type_labels = auth_types.len() > 1;
        let mut component = Self {
            search_input: Arc::new(Input::new()),
            all_providers: providers,
            filtered_providers: Vec::new(),
            selected_index: 0,
            mode,
            on_select,
            on_cancel,
            show_auth_type_labels,
            focused: false,
        };
        component.filter_providers(initial_search_input.unwrap_or(""));
        component
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if let Some(input) = Arc::get_mut(&mut self.search_input) {
            input.focused = focused;
        }
    }

    fn filter_providers(&mut self, query: &str) {
        self.filtered_providers = if query.is_empty() {
            self.all_providers.clone()
        } else {
            fuzzy_filter(&self.all_providers, query, |provider| {
                format!(
                    "{} {} {}",
                    provider.name,
                    provider.id,
                    format_auth_selector_provider_type(&provider.auth_type)
                )
            })
        };
        self.selected_index = self
            .selected_index
            .min(self.filtered_providers.len().saturating_sub(1));
    }

    fn format_status_indicator(&self, provider: &AuthSelectorProvider) -> String {
        let t = theme();
        let t = t.as_ref();
        let muted = |text: &str| t.map(|t| t.fg("muted", text)).unwrap_or_else(|| text.to_string());
        let Some(status) = &provider.status else {
            return muted(" • unconfigured");
        };
        if status.auth_type != auth_type_str(&provider.auth_type) {
            let label = if status.auth_type == "oauth" {
                "subscription configured".to_string()
            } else {
                "API key configured".to_string()
            };
            return format!(
                "{}{}",
                muted(" • "),
                t.map(|t| t.fg("warning", &label)).unwrap_or(label)
            );
        }
        if status.source.is_none() || status.source.as_deref() == Some("OAuth") || status.source.as_deref() == Some("stored credential") {
            return t.map(|t| t.fg("success", " ✓ configured")).unwrap_or_else(|| " ✓ configured".to_string());
        }
        let source = status.source.clone().unwrap_or_default();
        let formatted_source = if source
            .split(',')
            .all(|part| {
                let part = part.trim();
                !part.is_empty()
                    && part.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                    && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            }) {
            format!("env: {source}")
        } else {
            source
        };
        t.map(|t| t.fg("success", &format!(" ✓ {formatted_source}")))
            .unwrap_or_else(|| format!(" ✓ {formatted_source}"))
    }

    fn update_list(&self) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        let mut lines: Vec<String> = Vec::new();

        let max_visible = 8;
        let start_index = (self.selected_index as isize - (max_visible as isize / 2))
            .max(0)
            .min(self.filtered_providers.len() as isize - max_visible as isize)
            .max(0) as usize;
        let end_index = (start_index + max_visible).min(self.filtered_providers.len());

        for i in start_index..end_index {
            let provider = &self.filtered_providers[i];
            let is_selected = i == self.selected_index;
            let status_indicator = self.format_status_indicator(provider);
            let auth_type_label = if self.show_auth_type_labels {
                let label = format!(" [{}]", format_auth_selector_provider_type(&provider.auth_type));
                t.map(|t| t.fg("muted", &label)).unwrap_or(label)
            } else {
                String::new()
            };
            let line = if is_selected {
                let prefix = t.map(|t| t.fg("accent", "→ ")).unwrap_or_else(|| "→ ".to_string());
                let text = t.map(|t| t.fg("accent", &provider.name)).unwrap_or_else(|| provider.name.clone());
                format!("{prefix}{text}{auth_type_label}{status_indicator}")
            } else {
                let text = t.map(|t| t.fg("text", &provider.name)).unwrap_or_else(|| provider.name.clone());
                format!("  {text}{auth_type_label}{status_indicator}")
            };
            lines.push(truncate_to_width(&line, width_for(&lines, 60), "", false));
        }

        if start_index > 0 || end_index < self.filtered_providers.len() {
            let scroll_info = format!("  ({}/{})", self.selected_index + 1, self.filtered_providers.len());
            lines.push(t.map(|t| t.fg("muted", &scroll_info)).unwrap_or(scroll_info));
        }

        if self.filtered_providers.is_empty() {
            let message = if self.all_providers.is_empty() {
                if self.mode == "login" {
                    "No providers available"
                } else {
                    "No providers logged in. Use /login first."
                }
            } else {
                "No matching providers"
            };
            lines.push(format!("  {message}"));
        }

        lines
    }
}

fn width_for(_lines: &[String], default: usize) -> f64 {
    default as f64
}

fn auth_type_str(auth_type: &AuthSelectorAuthType) -> &'static str {
    match auth_type {
        AuthSelectorAuthType::OAuth => "oauth",
        AuthSelectorAuthType::ApiKey => "api_key",
    }
}

impl Component for OAuthSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let t = t.as_ref();
        let title = if self.mode == "login" {
            "Select provider to configure:"
        } else {
            "Select provider to logout:"
        };
        let title_styled = t
            .map(|t| t.bold(&t.fg("accent", title)))
            .unwrap_or_else(|| title.to_string());

        let mut lines: Vec<String> = Vec::new();
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines.push(format!(" {title_styled}"));
        lines.extend(self.search_input.render(width));
        for line in self.update_list() {
            lines.push(line);
        }
        lines.push(DynamicBorder::new(None).render(width).join("\n"));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = get_keybindings();
        let manager = match &*kb {
            Some(manager) => manager,
            None => return,
        };
        if manager.matches(data, "tui.select.up") {
            if self.filtered_providers.is_empty() {
                return;
            }
            self.selected_index = self.selected_index.saturating_sub(1);
        } else if manager.matches(data, "tui.select.down") {
            if self.filtered_providers.is_empty() {
                return;
            }
            self.selected_index = (self.selected_index + 1).min(self.filtered_providers.len() - 1);
        } else if manager.matches(data, "tui.select.confirm") {
            if let Some(selected) = self.filtered_providers.get(self.selected_index) {
                let auth_type = selected.auth_type.clone();
                let id = selected.id.clone();
                (self.on_select)(&id, auth_type);
            }
        } else if manager.matches(data, "tui.select.cancel") {
            (self.on_cancel)();
        } else if let Some(input) = Arc::get_mut(&mut self.search_input) {
            input.handle_input(data);
            let value = input.get_value().to_string();
            self.filter_providers(&value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_provider_type() {
        assert_eq!(format_auth_selector_provider_type(&AuthSelectorAuthType::OAuth), "subscription");
        assert_eq!(format_auth_selector_provider_type(&AuthSelectorAuthType::ApiKey), "API key");
    }

    #[test]
    fn renders_provider_list() {
        let providers = vec![AuthSelectorProvider {
            id: "acme".to_string(),
            name: "Acme".to_string(),
            auth_type: AuthSelectorAuthType::ApiKey,
            status: None,
        }];
        let component = OAuthSelectorComponent::new(
            "login",
            providers,
            Arc::new(|_, _| {}),
            Arc::new(|| {}),
            None,
        );
        let lines = component.render(60);
        assert!(lines.iter().any(|line| line.contains("Acme")));
    }
}
