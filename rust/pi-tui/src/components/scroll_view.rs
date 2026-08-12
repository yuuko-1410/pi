//! Scroll view container, port of `packages/tui/src/components/scroll-view.ts`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::tui::Component;

pub type ScrollViewScrollbar = &'static str; // "hidden" | "auto" | "always"

pub struct ScrollViewOptions {
    pub axis: Option<String>,
    pub follow: Option<String>,
    pub primary: Option<bool>,
    pub overscroll: Option<String>,
    pub scrollbar: Option<String>,
    pub scrollbar_hide_delay_ms: Option<f64>,
}

impl Default for ScrollViewOptions {
    fn default() -> Self {
        Self {
            axis: None,
            follow: None,
            primary: None,
            overscroll: None,
            scrollbar: None,
            scrollbar_hide_delay_ms: None,
        }
    }
}

/// Scrollable viewport around a single child component.
pub struct ScrollView {
    child: Arc<dyn Component>,
    follow_end: bool,
    pub primary: bool,
    pub overscroll: String,
    pub scrollbar_style: Arc<dyn Fn(&str) -> String + Send + Sync>,
    current_scrollbar: String,
    scrollbar_hide_delay_ms: f64,
    current_scroll_top: f64,
    content_height: f64,
    current_viewport_height: f64,
    following_end: bool,
    request_render: Arc<dyn Fn() + Send + Sync>,
    transient_scrollbar_visible: bool,
    scrollbar_active: bool,
}

impl ScrollView {
    pub fn new(
        child: Arc<dyn Component>,
        options: ScrollViewOptions,
        request_render: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        if let Some(axis) = &options.axis {
            if axis != "vertical" {
                return Err(format!("Unsupported ScrollView axis: {axis}"));
            }
        }
        let follow_end = options.follow.as_deref().unwrap_or("none") == "end";
        let scrollbar_hide_delay_ms = options.scrollbar_hide_delay_ms.unwrap_or(1000.0).max(0.0).floor();
        Ok(Self {
            child,
            follow_end,
            following_end: follow_end,
            primary: options.primary.unwrap_or(false),
            overscroll: options.overscroll.unwrap_or_else(|| "chain".to_string()),
            scrollbar_style: Arc::new(|text| format!("\x1b[100m{text}\x1b[49m")),
            current_scrollbar: options.scrollbar.unwrap_or_else(|| "hidden".to_string()),
            scrollbar_hide_delay_ms,
            current_scroll_top: 0.0,
            content_height: 0.0,
            current_viewport_height: 0.0,
            request_render,
            transient_scrollbar_visible: false,
            scrollbar_active: false,
        })
    }

    pub fn scroll_top(&self) -> f64 {
        self.current_scroll_top
    }

    pub fn is_following_end(&self) -> bool {
        self.following_end
    }

    pub fn viewport_height(&self) -> f64 {
        self.current_viewport_height
    }

    pub fn scrollbar(&self) -> &str {
        &self.current_scrollbar
    }

    pub fn is_scrollbar_visible(&self) -> bool {
        if self.current_scrollbar == "always" {
            return self.current_viewport_height > 0.0;
        }
        self.current_scrollbar == "auto"
            && self.content_height > self.current_viewport_height
            && self.transient_scrollbar_visible
    }

    pub fn set_scrollbar(&mut self, scrollbar: &str) {
        if scrollbar == self.current_scrollbar {
            return;
        }
        self.current_scrollbar = scrollbar.to_string();
        if scrollbar != "auto" {
            self.hide_transient_scrollbar();
        } else if self.scrollbar_active {
            self.mark_scrollbar_activity();
        }
        (self.request_render)();
    }

    pub fn get_content_width(&self, width: f64) -> f64 {
        if self.current_scrollbar == "always" && width > 1.0 {
            width - 1.0
        } else {
            width
        }
    }

    fn mark_scrollbar_activity(&mut self) {
        if self.current_scrollbar != "auto" || self.content_height <= self.current_viewport_height {
            return;
        }
        self.transient_scrollbar_visible = true;
        // ponytail: no timer-based auto-hide; transient visibility stays on
        // until setScrollbar changes it (JS uses a hide timer).
        if self.scrollbar_active {
            return;
        }
        let _ = self.scrollbar_hide_delay_ms;
    }

    fn hide_transient_scrollbar(&mut self) {
        self.transient_scrollbar_visible = false;
    }

    pub fn set_scrollbar_active(&mut self, active: bool) {
        if active == self.scrollbar_active {
            return;
        }
        self.scrollbar_active = active;
        self.mark_scrollbar_activity();
    }

    pub fn scroll_to(&mut self, scroll_top: f64) {
        let requested = if scroll_top.is_finite() {
            scroll_top.trunc()
        } else {
            self.current_scroll_top
        };
        let max_scroll_top = (self.content_height - self.current_viewport_height).max(0.0);
        let next = requested.max(0.0).min(max_scroll_top);
        if next == self.current_scroll_top {
            return;
        }
        self.current_scroll_top = next;
        self.following_end = self.follow_end && next == max_scroll_top;
        self.mark_scrollbar_activity();
        (self.request_render)();
    }

    /// Scroll by lines; returns the number of lines that could not be
    /// scrolled (JS `requested - moved`).
    pub fn scroll_by(&mut self, lines: f64) -> f64 {
        let requested = if lines.is_finite() { lines.trunc() } else { 0.0 };
        if requested == 0.0 {
            return 0.0;
        }
        let max_scroll_top = (self.content_height - self.current_viewport_height).max(0.0);
        let start = if self.following_end {
            max_scroll_top
        } else {
            self.current_scroll_top
        };
        let next = (start + requested).max(0.0).min(max_scroll_top);
        let moved = next - start;
        self.current_scroll_top = next;
        self.following_end = self.follow_end && next == max_scroll_top;
        if moved != 0.0 {
            self.mark_scrollbar_activity();
            (self.request_render)();
        }
        requested - moved
    }

    pub fn scroll_to_start(&mut self) {
        let new_following = self.follow_end && self.content_height <= self.current_viewport_height;
        let changed = self.current_scroll_top != 0.0 || self.following_end != new_following;
        self.current_scroll_top = 0.0;
        self.following_end = new_following;
        if changed {
            self.mark_scrollbar_activity();
            (self.request_render)();
        }
    }

    pub fn scroll_to_end(&mut self) {
        let next = (self.content_height - self.current_viewport_height).max(0.0);
        let changed = self.current_scroll_top != next;
        self.current_scroll_top = next;
        self.following_end = self.follow_end;
        if changed {
            self.mark_scrollbar_activity();
            (self.request_render)();
        }
    }

    pub fn update_layout(&mut self, content_height: f64, viewport_height: f64, request_render: Arc<dyn Fn() + Send + Sync>) {
        self.content_height = content_height.max(0.0).floor();
        self.current_viewport_height = viewport_height.max(0.0).floor();
        self.request_render = request_render;
        let max_scroll_top = (self.content_height - self.current_viewport_height).max(0.0);
        if self.following_end {
            self.current_scroll_top = max_scroll_top;
        } else {
            self.current_scroll_top = self.current_scroll_top.max(0.0).min(max_scroll_top);
        }
        if self.follow_end && self.current_scroll_top == max_scroll_top {
            self.following_end = true;
        }
        if self.content_height <= self.current_viewport_height {
            self.hide_transient_scrollbar();
        }
    }

    pub fn render_content(&self, width: f64) -> Vec<String> {
        let content_width = self.get_content_width(width);
        let lines = self.child.render(content_width as usize);
        if content_width == width {
            lines
        } else {
            lines.into_iter().map(|line| format!("{line} ")).collect()
        }
    }
}

static SCROLL_ID: AtomicU64 = AtomicU64::new(0);

/// Internal id for layout matching (JS uses object identity).
pub fn next_scroll_id() -> u64 {
    SCROLL_ID.fetch_add(1, Ordering::Relaxed) + 1
}

/// Shared mutable scroll state holder for layout integration.
pub struct ScrollState {
    pub inner: Mutex<ScrollView>,
    pub id: u64,
}

impl ScrollState {
    pub fn new(view: ScrollView) -> Self {
        Self {
            inner: Mutex::new(view),
            id: next_scroll_id(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedChild {
        lines: Vec<String>,
    }

    impl Component for FixedChild {
        fn render(&self, _width: usize) -> Vec<String> {
            self.lines.clone()
        }
    }

    fn view(content_height: f64) -> ScrollView {
        let child = Arc::new(FixedChild {
            lines: vec![String::new(); content_height as usize],
        });
        ScrollView::new(
            child,
            ScrollViewOptions::default(),
            Arc::new(|| {}),
        )
        .unwrap()
    }

    #[test]
    fn scroll_to_clamps() {
        let mut view = view(20.0);
        view.update_layout(20.0, 5.0, Arc::new(|| {}));
        assert_eq!(view.scroll_top(), 0.0);
        view.scroll_to(10.0);
        assert_eq!(view.scroll_top(), 10.0);
        view.scroll_to(100.0);
        assert_eq!(view.scroll_top(), 15.0);
        view.scroll_to(-5.0);
        assert_eq!(view.scroll_top(), 0.0);
    }

    #[test]
    fn scroll_by_returns_remaining() {
        let mut view = view(20.0);
        view.update_layout(20.0, 5.0, Arc::new(|| {}));
        assert_eq!(view.scroll_by(3.0), 0.0);
        assert_eq!(view.scroll_top(), 3.0);
        // Cannot scroll beyond max.
        let remaining = view.scroll_by(100.0);
        assert_eq!(remaining, 88.0);
        assert_eq!(view.scroll_top(), 15.0);
    }

    #[test]
    fn follow_end_tracks_bottom() {
        let mut view = view(20.0);
        let options = ScrollViewOptions {
            follow: Some("end".to_string()),
            ..ScrollViewOptions::default()
        };
        view = ScrollView::new(
            Arc::new(FixedChild {
                lines: vec![String::new(); 20],
            }),
            options,
            Arc::new(|| {}),
        )
        .unwrap();
        view.update_layout(20.0, 5.0, Arc::new(|| {}));
        assert!(view.is_following_end());
        assert_eq!(view.scroll_top(), 15.0);
    }

    #[test]
    fn scrollbar_visibility_rules() {
        let mut view = view(20.0);
        view.update_layout(20.0, 5.0, Arc::new(|| {}));
        assert!(!view.is_scrollbar_visible()); // hidden default

        view.set_scrollbar("always");
        assert!(view.is_scrollbar_visible());

        view.set_scrollbar("auto");
        assert!(!view.is_scrollbar_visible()); // transient not active

        view.set_scrollbar_active(true);
        assert!(view.is_scrollbar_visible());
    }

    #[test]
    fn get_content_width_reserves_scrollbar_column() {
        let mut view = view(1.0);
        view.set_scrollbar("always");
        assert_eq!(view.get_content_width(20.0), 19.0);
        assert_eq!(view.get_content_width(1.0), 1.0);
        view.set_scrollbar("hidden");
        assert_eq!(view.get_content_width(20.0), 20.0);
    }

    #[test]
    fn update_layout_clamps_scroll_top() {
        let mut view = view(10.0);
        view.update_layout(10.0, 5.0, Arc::new(|| {}));
        view.scroll_to(5.0);
        view.update_layout(10.0, 2.0, Arc::new(|| {}));
        assert_eq!(view.scroll_top(), 5.0);
        view.scroll_to(5.0);
        view.update_layout(4.0, 2.0, Arc::new(|| {}));
        assert_eq!(view.scroll_top(), 2.0);
    }
}
