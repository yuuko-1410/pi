//! Earendil announcement component, port of
//! `components/earendil-announcement.ts`.
//!
//! ponytail: the bundled PNG image is not embedded; the announcement is
//! text-only (the image load is best-effort in JS anyway).

use std::sync::Arc;

use pi_tui::components::basic::Text;
use pi_tui::tui::Container;
use pi_tui::tui::Component;

use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::theme::theme::theme;

const BLOG_URL: &str = "https://mariozechner.at/posts/2026-04-08-ive-sold-out/";

pub struct EarendilAnnouncementComponent {
    container: Container,
}

impl EarendilAnnouncementComponent {
    pub fn new() -> Self {
        let mut container = Container::new();
        let accent_border = {
            let border = |text: &str| {
                theme()
                    .as_ref()
                    .map(|t| t.fg("accent", text))
                    .unwrap_or_else(|| text.to_string())
            };
            Box::new(move |text: &str| border(text)) as Box<dyn Fn(&str) -> String + Send + Sync>
        };
        container.add_child(Arc::new(DynamicBorder::new(Some(accent_border))));
        let accent_border2 = {
            let border = |text: &str| {
                theme()
                    .as_ref()
                    .map(|t| t.fg("accent", text))
                    .unwrap_or_else(|| text.to_string())
            };
            Box::new(move |text: &str| border(text)) as Box<dyn Fn(&str) -> String + Send + Sync>
        };
        container.add_child(Arc::new(DynamicBorder::new(Some(accent_border2))));
        let title = theme()
            .as_ref()
            .map(|t| t.bold(&t.fg("accent", "pi has joined Earendil")))
            .unwrap_or_else(|| "pi has joined Earendil".to_string());
        container.add_child(Arc::new(Text::new(&title, 1, 0, None)));
        let subtitle = theme()
            .as_ref()
            .map(|t| t.fg("muted", "Read the blog post:"))
            .unwrap_or_else(|| "Read the blog post:".to_string());
        container.add_child(Arc::new(Text::new(&subtitle, 1, 0, None)));
        let link = theme()
            .as_ref()
            .map(|t| t.fg("mdLink", BLOG_URL))
            .unwrap_or_else(|| BLOG_URL.to_string());
        container.add_child(Arc::new(Text::new(&link, 1, 0, None)));
        Self { container }
    }
}

impl Component for EarendilAnnouncementComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_announcement() {
        let component = EarendilAnnouncementComponent::new();
        let lines = component.render(60);
        assert!(lines.iter().any(|line| line.contains("Earendil")));
        assert!(lines.iter().any(|line| line.contains(BLOG_URL)));
    }
}
