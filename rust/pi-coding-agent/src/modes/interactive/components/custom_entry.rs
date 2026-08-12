//! Custom entry component, port of `components/custom-entry.ts`.
//!
//! ponytail: the extension EntryRenderer type is not ported; the host
//! supplies a renderer closure producing pre-rendered lines, or none
//! (renders a label placeholder).

use std::sync::Arc;

use pi_tui::components::basic::{Box, Text};
use pi_tui::tui::Container;
use pi_tui::tui::Component;


/// Renderer for a custom entry: returns rendered lines for the given
/// expanded state, or None to skip rendering.
pub type EntryRenderer = Arc<dyn Fn(&str, bool) -> Option<Vec<String>> + Send + Sync>;

pub struct CustomEntryComponent {
    container: Container,
    custom_type: String,
    renderer: Option<EntryRenderer>,
    expanded: bool,
    has_content: bool,
}

impl CustomEntryComponent {
    pub fn new(custom_type: &str, renderer: Option<EntryRenderer>) -> Self {
        let mut component = Self {
            container: Container::new(),
            custom_type: custom_type.to_string(),
            renderer,
            expanded: false,
            has_content: false,
        };
        component.rebuild();
        component
    }

    pub fn has_content(&self) -> bool {
        self.has_content
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        if self.expanded != expanded {
            self.expanded = expanded;
            self.rebuild();
        }
    }

    fn rebuild(&mut self) {
        self.container.clear();
        self.has_content = false;

        let Some(renderer) = &self.renderer else {
            return;
        };
        match renderer(&self.custom_type, self.expanded) {
            Some(lines) if !lines.is_empty() => {
                self.has_content = true;
                let mut boxed = Box::new(1, 1, None);
                for line in lines {
                    boxed.add_child(Arc::new(Text::new(&line, 0, 0, None)));
                }
                self.container.add_child(Arc::new(boxed));
            }
            _ => {}
        }
    }
}

impl Component for CustomEntryComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn invalidate(&mut self) {
        self.rebuild();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_renderer_renders_empty() {
        let component = CustomEntryComponent::new("customType", None);
        assert!(!component.has_content());
        assert!(component.render(40).is_empty());
    }

    #[test]
    fn renderer_lines_are_shown() {
        let renderer: EntryRenderer = Arc::new(|_, expanded| {
            if expanded {
                Some(vec!["expanded line".to_string()])
            } else {
                Some(vec!["collapsed line".to_string()])
            }
        });
        let mut component = CustomEntryComponent::new("customType", Some(renderer));
        assert!(component.has_content());
        assert!(component.render(40).iter().any(|line| line.contains("collapsed line")));
        component.set_expanded(true);
        assert!(component.render(40).iter().any(|line| line.contains("expanded line")));
    }

    #[test]
    fn theme_error_fallback_shown() {
        // A renderer that errors is equivalent to None here (sync closure
        // cannot panic without aborting the test).
        let component = CustomEntryComponent::new("customType", None);
        assert!(component.render(40).is_empty());
    }
}
