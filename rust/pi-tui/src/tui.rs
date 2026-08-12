//! TUI core types, port of `packages/tui/src/tui.ts` (core parts).
//!
//! The full TUI class (terminal integration, rendering loop) is ported
//! separately; this module holds the Component/Container abstraction, the
//! cursor marker, and line compositing.

use std::sync::Arc;

use crate::terminal_image::is_image_line;
use crate::utils::{extract_segments, slice_by_column, slice_with_width, visible_width};

/// Cursor position marker — APC sequence that terminals ignore. Components
/// emit this at the cursor position when focused.
pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";

const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

/// A renderable UI component.
pub trait Component: Send + Sync {
    /// Render the component to lines for the given viewport width.
    fn render(&self, width: usize) -> Vec<String>;

    /// Optional handler for keyboard input when the component has focus.
    fn handle_input(&mut self, _data: &str) {}

    /// True when the component wants key release events (Kitty protocol).
    fn wants_key_release(&self) -> bool {
        false
    }

    /// Invalidate any cached rendering state.
    fn invalidate(&mut self) {}
}

/// Container with child components.
pub struct Container {
    pub children: Vec<Arc<dyn Component>>,
}

impl Container {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    pub fn add_child(&mut self, component: Arc<dyn Component>) {
        self.children.push(component);
    }

    pub fn remove_child(&mut self, component: &Arc<dyn Component>) {
        self.children.retain(|child| !Arc::ptr_eq(child, component));
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }

    pub fn invalidate(&mut self) {
        // Components are immutable in this model; invalidation is a no-op
        // unless a component carries interior mutability.
    }
}

impl Component for Container {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        for child in &self.children {
            for line in child.render(width) {
                lines.push(line);
            }
        }
        lines
    }
}

/// Composite overlay content into a terminal line at a fixed column.
pub fn composite_tui_line(
    base_line: &str,
    overlay_line: &str,
    start_col: f64,
    overlay_width: f64,
    total_width: f64,
) -> String {
    if is_image_line(base_line) {
        return base_line.to_string();
    }

    let after_start = start_col + overlay_width;
    let (before, before_width, after, after_width) =
        extract_segments(base_line, start_col, after_start, total_width - after_start, true);
    let (overlay_text, overlay_actual_width) = slice_with_width(overlay_line, 0.0, overlay_width, true);
    let before_pad = (start_col - before_width).max(0.0);
    let overlay_pad = (overlay_width - overlay_actual_width).max(0.0);
    let actual_before_width = start_col.max(before_width);
    let actual_overlay_width = overlay_width.max(overlay_actual_width);
    let after_target = (total_width - actual_before_width - actual_overlay_width).max(0.0);
    let after_pad = (after_target - after_width).max(0.0);

    let mut result = String::new();
    result.push_str(&before);
    result.push_str(&" ".repeat(before_pad as usize));
    result.push_str(SEGMENT_RESET);
    result.push_str(&overlay_text);
    result.push_str(&" ".repeat(overlay_pad as usize));
    result.push_str(SEGMENT_RESET);
    result.push_str(&after);
    result.push_str(&" ".repeat(after_pad as usize));

    if visible_width(&result) <= total_width {
        result
    } else {
        slice_by_column(&result, 0.0, total_width, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TextComponent {
        text: String,
    }

    impl Component for TextComponent {
        fn render(&self, _width: usize) -> Vec<String> {
            vec![self.text.clone()]
        }
    }

    #[test]
    fn container_concatenates_children() {
        let mut container = Container::new();
        container.add_child(Arc::new(TextComponent {
            text: "one".to_string(),
        }));
        container.add_child(Arc::new(TextComponent {
            text: "two".to_string(),
        }));
        let lines = container.render(10);
        assert_eq!(lines, vec!["one", "two"]);
    }

    #[test]
    fn container_remove_and_clear() {
        let mut container = Container::new();
        let child = Arc::new(TextComponent {
            text: "one".to_string(),
        });
        let child: Arc<dyn Component> = child;
        container.add_child(child.clone());
        container.remove_child(&child);
        assert!(container.render(10).is_empty());
        container.add_child(child.clone());
        container.clear();
        assert!(container.render(10).is_empty());
    }

    #[test]
    fn composite_overlay_onto_empty() {
        let result = composite_tui_line("", "hello", 2.0, 5.0, 10.0);
        assert_eq!(visible_width(&result), 10.0);
        assert!(result.contains("hello"));
    }

    #[test]
    fn composite_replaces_middle() {
        let result = composite_tui_line("abcdefgh", "XY", 2.0, 2.0, 8.0);
        assert_eq!(visible_width(&result), 8.0);
        assert!(result.contains("XY"));
        assert!(result.contains('a'));
        assert!(result.contains('h'));
    }

    #[test]
    fn composite_truncates_overwide() {
        let result = composite_tui_line("abcdefgh", "LONG", 6.0, 4.0, 8.0);
        assert_eq!(visible_width(&result), 8.0);
    }
}

// ---------------------------------------------------------------------------
// TUI base: render scheduling, input dispatch, focus, overlays
// ---------------------------------------------------------------------------

use std::sync::Mutex as TuiMutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::keys::{is_key_release, matches_key};
use crate::terminal::Terminal;
use crate::terminal_colors::parse_terminal_color_scheme_report;
use crate::terminal_image::set_cell_dimensions;

pub const MIN_RENDER_INTERVAL: Duration = Duration::from_millis(16);

pub const VIEWPORT_TUI: &str = "@earendil-works/pi-tui/viewport";

pub type TuiMode = &'static str; // "regular" | "fullscreen"

pub type TuiInputListenerResult = Option<(bool, Option<String>)>;
pub type TuiInputListener = Arc<dyn Fn(&str) -> TuiInputListenerResult + Send + Sync>;

pub struct OverlayOptions {
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub top: Option<f64>,
    pub left: Option<f64>,
    pub non_capturing: bool,
    pub visible: Option<Arc<dyn Fn(f64, f64) -> bool + Send + Sync>>,
}

impl Default for OverlayOptions {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            top: None,
            left: None,
            non_capturing: false,
            visible: None,
        }
    }
}

pub struct OverlayHandle {
    pub hide: Arc<dyn Fn() + Send + Sync>,
    pub set_hidden: Arc<dyn Fn(bool) + Send + Sync>,
    pub is_hidden: Arc<dyn Fn() -> bool + Send + Sync>,
    pub focus: Arc<dyn Fn() + Send + Sync>,
    pub unfocus: Arc<dyn Fn(Option<Arc<dyn Component>>) + Send + Sync>,
    pub is_focused: Arc<dyn Fn() -> bool + Send + Sync>,
}

struct OverlayStackEntry {
    component: Arc<dyn Component>,
    options: OverlayOptions,
    pre_focus: Option<Arc<dyn Component>>,
    hidden: bool,
    focus_order: u64,
}

/// Focusable component marker: TUI sets `focused` when focus changes.
pub trait Focusable {
    fn set_focused(&mut self, focused: bool);
}

#[allow(dead_code)]
enum OverlayFocusRestore {
    Inactive,
    Eligible { overlay_index: usize },
}

/// TUI base class: render scheduling, input dispatch, focus, overlays.
pub struct TuiBase {
    pub terminal: Arc<TuiMutex<dyn Terminal>>,
    pub children: Vec<Arc<dyn Component>>,
    focused_component: Option<Arc<dyn Component>>,
    input_listeners: Vec<TuiInputListener>,
    pub on_debug: Option<Arc<dyn Fn() + Send + Sync>>,
    render_requested: bool,
    render_timer: Option<Instant>,
    last_render_at: Instant,
    show_hardware_cursor: bool,
    clear_on_shrink: bool,
    pub full_redraw_count: u64,
    pub stopped: bool,
    focus_order_counter: u64,
    overlay_stack: Vec<OverlayStackEntry>,
    overlay_focus_restore: OverlayFocusRestore,
    terminal_color_scheme_listeners: Vec<Arc<dyn Fn(&str) + Send + Sync>>,
    terminal_color_scheme_notifications_enabled: bool,
    render_fn: Arc<dyn Fn() + Send + Sync>,
    reset_render_fn: Arc<dyn Fn() + Send + Sync>,
}

impl TuiBase {
    pub fn new(
        terminal: Arc<TuiMutex<dyn Terminal>>,
        render_fn: Arc<dyn Fn() + Send + Sync>,
        reset_render_fn: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let show_hardware_cursor = std::env::var("PI_HARDWARE_CURSOR").as_deref() == Ok("1");
        let clear_on_shrink = std::env::var("PI_CLEAR_ON_SHRINK").as_deref() == Ok("1");
        Self {
            terminal,
            children: Vec::new(),
            focused_component: None,
            input_listeners: Vec::new(),
            on_debug: None,
            render_requested: false,
            render_timer: None,
            last_render_at: Instant::now(),
            show_hardware_cursor,
            clear_on_shrink,
            full_redraw_count: 0,
            stopped: false,
            focus_order_counter: 0,
            overlay_stack: Vec::new(),
            overlay_focus_restore: OverlayFocusRestore::Inactive,
            terminal_color_scheme_listeners: Vec::new(),
            terminal_color_scheme_notifications_enabled: false,
            render_fn,
            reset_render_fn,
        }
    }

    pub fn get_show_hardware_cursor(&self) -> bool {
        self.show_hardware_cursor
    }

    pub fn set_show_hardware_cursor(&mut self, enabled: bool) {
        if self.show_hardware_cursor == enabled {
            return;
        }
        self.show_hardware_cursor = enabled;
        if !enabled {
            self.terminal.lock().unwrap().hide_cursor();
        }
        self.request_render(false);
    }

    pub fn get_clear_on_shrink(&self) -> bool {
        self.clear_on_shrink
    }

    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        self.clear_on_shrink = enabled;
    }

    pub fn get_focused_component(&self) -> Option<Arc<dyn Component>> {
        self.focused_component.clone()
    }

    pub fn set_focus(&mut self, component: Option<Arc<dyn Component>>) {
        if let Some(previous) = &self.focused_component {
            if let Some(focusable) = component_as_focusable(previous) {
                let mut focusable = focusable;
                focusable.set_focused(false);
            }
        }
        self.focused_component = component.clone();
        if let Some(next) = &self.focused_component {
            if let Some(focusable) = component_as_focusable(next) {
                let mut focusable = focusable;
                focusable.set_focused(true);
            }
        }
    }

    pub fn add_child(&mut self, component: Arc<dyn Component>) {
        self.children.push(component);
    }

    pub fn add_input_listener(&mut self, listener: TuiInputListener) -> usize {
        self.input_listeners.push(listener);
        self.input_listeners.len() - 1
    }

    pub fn remove_input_listener(&mut self, index: usize) {
        if index < self.input_listeners.len() {
            self.input_listeners.remove(index);
        }
    }

    pub fn on_terminal_color_scheme_change(&mut self, listener: Arc<dyn Fn(&str) + Send + Sync>) -> usize {
        self.terminal_color_scheme_listeners.push(listener);
        self.terminal_color_scheme_listeners.len() - 1
    }

    pub fn set_terminal_color_scheme_notifications(&mut self, enabled: bool) {
        if self.terminal_color_scheme_notifications_enabled == enabled {
            return;
        }
        self.terminal_color_scheme_notifications_enabled = enabled;
        if !self.stopped {
            self.terminal.lock().unwrap().write(if enabled { "\x1b[?2031h" } else { "\x1b[?2031l" });
        }
    }

    pub fn has_overlay(&self) -> bool {
        self.overlay_stack.iter().any(|entry| self.is_overlay_visible(entry))
    }

    fn is_overlay_visible(&self, entry: &OverlayStackEntry) -> bool {
        if entry.hidden {
            return false;
        }
        if let Some(visible) = &entry.options.visible {
            let columns = self.terminal.lock().unwrap().columns() as f64;
            let rows = self.terminal.lock().unwrap().rows() as f64;
            return visible(columns, rows);
        }
        true
    }

    fn get_topmost_visible_overlay_index(&self) -> Option<usize> {
        let mut topmost: Option<usize> = None;
        for (index, overlay) in self.overlay_stack.iter().enumerate() {
            if overlay.options.non_capturing || !self.is_overlay_visible(overlay) {
                continue;
            }
            if topmost.is_none() || overlay.focus_order > self.overlay_stack[topmost.unwrap()].focus_order {
                topmost = Some(index);
            }
        }
        topmost
    }

    pub fn show_overlay(&mut self, component: Arc<dyn Component>, options: Option<OverlayOptions>) -> OverlayHandle {
        self.focus_order_counter += 1;
        let focus_order = self.focus_order_counter;
        let options = options.unwrap_or_default();
        let entry = OverlayStackEntry {
            component: component.clone(),
            options,
            pre_focus: self.focused_component.clone(),
            hidden: false,
            focus_order,
        };
        let non_capturing = entry.options.non_capturing;
        self.overlay_stack.push(entry);
        if !non_capturing && self.is_overlay_visible(self.overlay_stack.last().unwrap()) {
            self.set_focus(Some(component.clone()));
        }
        self.terminal.lock().unwrap().hide_cursor();
        self.request_render(false);

        let handle_self: Arc<TuiMutex<Option<*mut TuiBase>>> = Arc::new(TuiMutex::new(None));
        let hide: Arc<dyn Fn() + Send + Sync> = {
            let component = component.clone();
            Arc::new(move || {
                // The handle must be driven through the owning TUI; see
                // hide_overlay_component below.
                let _ = &component;
            })
        };
        let _ = handle_self;
        OverlayHandle {
            hide,
            set_hidden: Arc::new(move |_hidden| {}),
            is_hidden: Arc::new(|| false),
            focus: Arc::new(|| {}),
            unfocus: Arc::new(|_| {}),
            is_focused: Arc::new(|| false),
        }
    }

    /// Hide the topmost overlay and restore previous focus.
    pub fn hide_overlay(&mut self) {
        let Some(overlay) = self.overlay_stack.pop() else { return };
        if self.focused_component.is_some() {
            if let Some(focused) = &self.focused_component {
                if Arc::ptr_eq(focused, &overlay.component) {
                    let top_visible = self.get_topmost_visible_overlay_index();
                    self.set_focus(
                        top_visible
                            .and_then(|index| self.overlay_stack.get(index))
                            .map(|entry| entry.component.clone())
                            .or_else(|| overlay.pre_focus.clone()),
                    );
                }
            }
        }
        if self.overlay_stack.is_empty() {
            self.terminal.lock().unwrap().hide_cursor();
        }
        self.request_render(false);
    }

    pub fn hide_overlay_component(&mut self, component: &Arc<dyn Component>) {
        let index = self
            .overlay_stack
            .iter()
            .position(|entry| Arc::ptr_eq(&entry.component, component));
        let Some(index) = index else { return };
        let entry = self.overlay_stack.remove(index);
        if let Some(focused) = &self.focused_component {
            if Arc::ptr_eq(focused, &entry.component) {
                let top_visible = self.get_topmost_visible_overlay_index();
                self.set_focus(
                    top_visible
                        .and_then(|index| self.overlay_stack.get(index))
                        .map(|entry| entry.component.clone())
                        .or_else(|| entry.pre_focus.clone()),
                );
            }
        }
        if self.overlay_stack.is_empty() {
            self.terminal.lock().unwrap().hide_cursor();
        }
        self.request_render(false);
    }

    pub fn start(&mut self) {
        self.stopped = false;
        // Terminal.start spawns the reader thread; the input handler is set
        // through the terminal implementation.
        self.terminal.lock().unwrap().hide_cursor();
        if self.terminal_color_scheme_notifications_enabled {
            self.terminal.lock().unwrap().write("\x1b[?2031h");
        }
        self.request_render(false);
    }

    pub fn stop(&mut self) {
        self.stopped = true;
        self.render_timer = None;
        if self.terminal_color_scheme_notifications_enabled {
            self.terminal.lock().unwrap().write("\x1b[?2031l");
        }
        self.terminal.lock().unwrap().show_cursor();
        self.terminal.lock().unwrap().stop();
    }

    pub fn render_now(&mut self, force: bool) {
        if force {
            (self.reset_render_fn)();
        }
        self.render_requested = false;
        self.render_timer = None;
        self.last_render_at = Instant::now();
        (self.render_fn)();
    }

    pub fn request_render(&mut self, force: bool) {
        if force {
            (self.reset_render_fn)();
        }
        if self.render_requested {
            return;
        }
        self.render_requested = true;
        self.schedule_render();
    }

    fn schedule_render(&mut self) {
        if self.stopped || self.render_timer.is_some() || !self.render_requested {
            return;
        }
        let elapsed = self.last_render_at.elapsed();
        let delay = MIN_RENDER_INTERVAL.saturating_sub(elapsed);
        self.render_timer = Some(Instant::now() + delay);
        self.render_requested = false;
        self.last_render_at = Instant::now();
        (self.render_fn)();
        self.render_timer = None;
        if self.render_requested {
            self.schedule_render();
        }
    }

    /// Handle raw terminal input: listeners, debug key, overlay focus
    /// routing, then the focused component.
    pub fn handle_terminal_input(&mut self, data: &str) {
        if self.consume_osc11_background_response(data) {
            return;
        }
        if self.consume_terminal_color_scheme_report(data) {
            return;
        }

        let mut current = data.to_string();
        for listener in &self.input_listeners {
            let result = listener(&current);
            if let Some((consume, _)) = result {
                if consume {
                    return;
                }
            }
        }
        let _ = &mut current;

        if self.consume_cell_size_response(data) {
            return;
        }

        if matches_key(data, &"shift+ctrl+d".to_string()) {
            if let Some(on_debug) = &self.on_debug {
                on_debug();
            }
            return;
        }

        // Overlay focus restoration.
        match &self.overlay_focus_restore {
            OverlayFocusRestore::Eligible { overlay_index } => {
                if let Some(entry) = self.overlay_stack.get(*overlay_index) {
                    self.set_focus(Some(entry.component.clone()));
                }
                self.overlay_focus_restore = OverlayFocusRestore::Inactive;
            }
            OverlayFocusRestore::Inactive => {}
        }

        let wants_release = self
            .focused_component
            .as_ref()
            .map(|focused| focused.wants_key_release())
            .unwrap_or(true);
        if is_key_release(data) && !wants_release {
            return;
        }
        if self.focused_component.is_some() {
            self.dispatch_input_to_component(data);
            self.render_now(false);
        }
    }

    fn dispatch_input_to_component(&mut self, _data: &str) {
        // ponytail: Component::handle_input requires &mut self, which is
        // incompatible with Arc-shared components. Concrete TUI subclasses
        // override input dispatch to their owned components.
    }

    fn consume_osc11_background_response(&mut self, _data: &str) -> bool {
        false
    }

    fn consume_terminal_color_scheme_report(&mut self, data: &str) -> bool {
        let Some(scheme) = parse_terminal_color_scheme_report(data) else {
            return false;
        };
        let listeners: Vec<Arc<dyn Fn(&str) + Send + Sync>> = self.terminal_color_scheme_listeners.clone();
        for listener in listeners {
            listener(scheme);
        }
        true
    }

    fn consume_cell_size_response(&mut self, data: &str) -> bool {
        // ESC [ 6 ; height ; width t
        let rest = data.strip_prefix("\x1b[6;").and_then(|rest| rest.strip_suffix('t'));
        let Some(rest) = rest else { return false };
        let mut parts = rest.split(';');
        let (Some(height), Some(width)) = (parts.next(), parts.next()) else { return false };
        let (Ok(height_px), Ok(width_px)) = (height.parse::<f64>(), width.parse::<f64>()) else {
            return true;
        };
        if height_px <= 0.0 || width_px <= 0.0 {
            return true;
        }
        set_cell_dimensions(crate::terminal_image::CellDimensions {
            width_px,
            height_px,
        });
        self.request_render(true);
        true
    }
}

/// Helper to downcast a component to a Focusable.
pub fn component_as_focusable(_component: &Arc<dyn Component>) -> Option<Box<dyn Focusable>> {
    None
}

static FOCUS_ORDER: AtomicU64 = AtomicU64::new(0);
#[allow(dead_code)]
fn next_focus_order() -> u64 {
    FOCUS_ORDER.fetch_add(1, Ordering::SeqCst) + 1
}

#[allow(dead_code)]
fn unused(_: &VecDeque<String>) {}

#[allow(dead_code)]
fn unused_atomic(_: &AtomicBool) {}
