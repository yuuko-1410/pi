//! Loader and truncated text components, ports of
//! `packages/tui/src/components/{loader,truncated-text,cancellable-loader}.ts`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::keybindings::{get_keybindings, KeybindingsManager};
use crate::tui::Component;
use crate::utils::{truncate_to_width, visible_width};

use super::basic::Text;

pub struct LoaderIndicatorOptions {
    pub frames: Option<Vec<String>>,
    pub interval_ms: Option<f64>,
}

impl Default for LoaderIndicatorOptions {
    fn default() -> Self {
        Self {
            frames: None,
            interval_ms: None,
        }
    }
}

const DEFAULT_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const DEFAULT_INTERVAL_MS: f64 = 80.0;

/// Loader component with an optional spinning animation.
pub struct Loader {
    text: Text,
    frames: Vec<String>,
    interval_ms: f64,
    current_frame: usize,
    render_indicator_verbatim: bool,
    spinner_color_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
    message_color_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
    message: String,
    request_render: Arc<dyn Fn() + Send + Sync>,
}

impl Loader {
    pub fn new(
        spinner_color_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
        message_color_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
        message: &str,
        indicator: Option<&LoaderIndicatorOptions>,
        request_render: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let mut loader = Self {
            text: Text::new("", 1, 0, None),
            frames: DEFAULT_FRAMES.iter().map(|f| f.to_string()).collect(),
            interval_ms: DEFAULT_INTERVAL_MS,
            current_frame: 0,
            render_indicator_verbatim: false,
            spinner_color_fn,
            message_color_fn,
            message: message.to_string(),
            request_render,
        };
        loader.set_indicator(indicator);
        loader
    }

    pub fn set_message(&mut self, message: &str) {
        self.message = message.to_string();
        self.update_display();
    }

    pub fn set_indicator(&mut self, indicator: Option<&LoaderIndicatorOptions>) {
        self.render_indicator_verbatim = indicator.is_some();
        self.frames = match indicator.and_then(|options| options.frames.clone()) {
            Some(frames) => frames,
            None => DEFAULT_FRAMES.iter().map(|f| f.to_string()).collect(),
        };
        self.interval_ms = match indicator.and_then(|options| options.interval_ms) {
            Some(interval_ms) if interval_ms > 0.0 => interval_ms,
            _ => DEFAULT_INTERVAL_MS,
        };
        self.current_frame = 0;
        self.update_display();
    }

    /// Advance the animation frame (called by the interval; the caller
    /// schedules the timer).
    pub fn advance_frame(&mut self) {
        self.current_frame = (self.current_frame + 1) % self.frames.len();
        self.update_display();
    }

    fn update_display(&mut self) {
        let frame = self.frames.get(self.current_frame).cloned().unwrap_or_default();
        let rendered_frame = if self.render_indicator_verbatim {
            frame.clone()
        } else {
            (self.spinner_color_fn)(&frame)
        };
        let indicator = if frame.is_empty() {
            String::new()
        } else {
            format!("{rendered_frame} ")
        };
        self.text.set_text(&format!("{indicator}{}", (self.message_color_fn)(&self.message)));
        (self.request_render)();
    }
}

impl Component for Loader {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines = vec![String::new()];
        lines.extend(self.text.render(width));
        lines
    }

    fn handle_input(&mut self, _data: &str) {}
}

/// Text component that truncates to fit the viewport width.
pub struct TruncatedText {
    text: String,
    padding_x: usize,
    padding_y: usize,
}

impl TruncatedText {
    pub fn new(text: &str, padding_x: usize, padding_y: usize) -> Self {
        Self {
            text: text.to_string(),
            padding_x,
            padding_y,
        }
    }

    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
    }
}

impl Component for TruncatedText {
    fn render(&self, width: usize) -> Vec<String> {
        let mut result: Vec<String> = Vec::new();
        let empty_line = " ".repeat(width);
        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }
        let available_width = ((width as isize) - (self.padding_x as isize) * 2).max(1) as f64;
        let single_line_text = match self.text.find('\n') {
            Some(index) => self.text[..index].to_string(),
            None => self.text.clone(),
        };
        let display_text = truncate_to_width(&single_line_text, available_width, "...", false);
        let left_padding = " ".repeat(self.padding_x);
        let right_padding = " ".repeat(self.padding_x);
        let line_with_padding = format!("{left_padding}{display_text}{right_padding}");
        let line_visible_width = visible_width(&line_with_padding);
        let padding_needed = ((width as f64) - line_visible_width).max(0.0) as usize;
        result.push(format!("{line_with_padding}{}", " ".repeat(padding_needed)));
        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }
        result
    }
}

/// Loader that can be cancelled with Escape.
pub struct CancellableLoader {
    loader: Loader,
    aborted: Arc<AtomicBool>,
    on_abort: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl CancellableLoader {
    pub fn new(
        spinner_color_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
        message_color_fn: Arc<dyn Fn(&str) -> String + Send + Sync>,
        message: &str,
        indicator: Option<&LoaderIndicatorOptions>,
        request_render: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            loader: Loader::new(spinner_color_fn, message_color_fn, message, indicator, request_render),
            aborted: Arc::new(AtomicBool::new(false)),
            on_abort: None,
        }
    }

    pub fn set_on_abort(&mut self, on_abort: Arc<dyn Fn() + Send + Sync>) {
        self.on_abort = Some(on_abort);
    }

    pub fn aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }

    pub fn stop(&mut self) {}
}

impl Component for CancellableLoader {
    fn render(&self, width: usize) -> Vec<String> {
        self.loader.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        let keybindings = get_keybindings();
        let matches = match &*keybindings {
            Some(manager) => manager.matches(data, "tui.select.cancel"),
            None => {
                let manager = KeybindingsManager::new(crate::keybindings::tui_keybindings());
                manager.matches(data, "tui.select.cancel")
            }
        };
        if matches {
            self.aborted.store(true, Ordering::SeqCst);
            if let Some(on_abort) = &self.on_abort {
                on_abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Arc<dyn Fn(&str) -> String + Send + Sync> {
        Arc::new(|text| text.to_string())
    }

    #[test]
    fn truncated_text_truncates() {
        let component = TruncatedText::new("hello world", 0, 0);
        let lines = component.render(5);
        assert_eq!(visible_width(&lines[0]), 5.0);
        assert!(lines[0].contains("..."));
    }

    #[test]
    fn truncated_text_takes_first_line() {
        let component = TruncatedText::new("first\nsecond", 0, 0);
        let lines = component.render(20);
        assert!(lines[0].starts_with("first"));
        assert!(!lines[0].contains("second"));
    }

    #[test]
    fn truncated_text_padding() {
        let component = TruncatedText::new("x", 1, 1);
        let lines = component.render(5);
        assert_eq!(lines.len(), 3);
        assert_eq!(visible_width(&lines[1]), 5.0);
    }

    #[test]
    fn loader_renders_indicator_and_message() {
        let mut loader = Loader::new(identity(), identity(), "Working", None, Arc::new(|| {}));
        let lines = loader.render(20);
        // Leading empty line plus content.
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("Working"));
        loader.advance_frame();
        let lines = loader.render(20);
        assert!(lines[1].contains("Working"));
    }

    #[test]
    fn loader_set_message_updates() {
        let mut loader = Loader::new(identity(), identity(), "Working", None, Arc::new(|| {}));
        loader.set_message("Done");
        let lines = loader.render(20);
        assert!(lines[1].contains("Done"));
    }

    #[test]
    fn cancellable_loader_aborts_on_escape() {
        let mut loader = CancellableLoader::new(identity(), identity(), "Working", None, Arc::new(|| {}));
        let aborted = Arc::new(AtomicBool::new(false));
        let on_abort = {
            let aborted = aborted.clone();
            Arc::new(move || {
                aborted.store(true, Ordering::SeqCst);
            })
        };
        loader.set_on_abort(on_abort);
        assert!(!loader.aborted());
        loader.handle_input("\x1b");
        assert!(loader.aborted());
        assert!(aborted.load(Ordering::SeqCst));
    }
}
