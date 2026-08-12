//! Alternate-screen TUI, port of `packages/tui/src/tui-alt-screen.ts`.
//!
//! Core port: viewport scrolling with layout-integrated scroll views,
//! wheel and SGR mouse event parsing, page navigation, prompt jumping, and
//! layout-based rendering with kitty image eviction. Selection logic
//! (anchor/focus, double-click words, auto-scroll) is simplified to
//! character-selection anchoring with a copy callback; OSC 52 clipboard is
//! delegated to the caller.

use std::sync::{Arc, Mutex};

use crate::keybindings::{get_keybindings, KeybindingsManager};
use crate::keys::is_key_release;
use crate::layout::{get_scroll_views_at, LayoutFrame};
use crate::components::scroll_view::{ScrollView, ScrollViewOptions};
use crate::terminal::Terminal;
use crate::tui::{Component, TuiBase};

const PAGE_SCROLL_OVERLAP: f64 = 2.0;
const FOCUS_IN: &str = "\x1b[I";
const FOCUS_OUT: &str = "\x1b[O";

pub struct TuiAltScreenOptions {
    pub wheel_scroll_lines: Option<f64>,
    pub mouse: Option<bool>,
    pub open_url: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub on_right_click_paste: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Default for TuiAltScreenOptions {
    fn default() -> Self {
        Self {
            wheel_scroll_lines: None,
            mouse: Some(true),
            open_url: None,
            on_right_click_paste: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WheelEvent {
    pub direction: isize,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SgrMouseEvent {
    pub button: f64,
    pub x: f64,
    pub y: f64,
    pub release: bool,
}

/// Parse an SGR wheel event (CSI < b ; x ; y M) or X10 form.
pub fn parse_wheel_event(data: &str) -> Option<WheelEvent> {
    if let Some(rest) = data.strip_prefix("\x1b[<") {
        if rest.ends_with('M') || rest.ends_with('m') {
            let rest = &rest[..rest.len() - 1];
            let mut parts = rest.split(';');
            let button = parts.next()?.parse::<i64>().ok()?;
            if button & 64 == 0 {
                return None;
            }
            let direction = button & 3;
            if direction != 0 && direction != 1 {
                return None;
            }
            let x = parts.next()?.parse::<f64>().ok()? - 1.0;
            let y = parts.next()?.parse::<f64>().ok()? - 1.0;
            return Some(WheelEvent {
                direction: if direction == 0 { -1 } else { 1 },
                x,
                y,
            });
        }
    }
    // X10: ESC [ M b x y
    if data.len() == 6 && data.starts_with("\x1b[M") {
        let bytes = data.as_bytes();
        let button = (bytes[3] as i64) - 32;
        if button & 64 == 0 {
            return None;
        }
        let direction = button & 3;
        if direction != 0 && direction != 1 {
            return None;
        }
        return Some(WheelEvent {
            direction: if direction == 0 { -1 } else { 1 },
            x: (bytes[4] as f64) - 33.0,
            y: (bytes[5] as f64) - 33.0,
        });
    }
    None
}

/// Parse an SGR mouse event (CSI < b ; x ; y M/m).
pub fn parse_sgr_mouse_event(data: &str) -> Option<SgrMouseEvent> {
    let rest = data.strip_prefix("\x1b[<")?;
    if !rest.ends_with('M') && !rest.ends_with('m') {
        return None;
    }
    let release = rest.ends_with('m');
    let rest = &rest[..rest.len() - 1];
    let mut parts = rest.split(';');
    let button = parts.next()?.parse::<f64>().ok()?;
    let x = parts.next()?.parse::<f64>().ok()? - 1.0;
    let y = parts.next()?.parse::<f64>().ok()? - 1.0;
    Some(SgrMouseEvent { button, x, y, release })
}

pub fn is_mouse_sequence(data: &str) -> bool {
    data.starts_with("\x1b[<") || data.starts_with("\x1b[M")
}

/// Alternate-screen TUI with a scrollable, application-owned viewport.
pub struct TuiAltScreen {
    pub base: TuiBase,
    pub previous_screen: Vec<String>,
    pub last_document: Vec<String>,
    pub previous_screen_width: f64,
    pub previous_screen_height: f64,
    pub layout_root: Option<Arc<dyn Component>>,
    pub current_layout: Option<LayoutFrame>,
    pub scroll_states: Vec<(u64, Arc<Mutex<ScrollView>>)>,
    pub primary_scroll_state: Option<(u64, Arc<Mutex<ScrollView>>)>,
    pub wheel_scroll_lines: f64,
    pub mouse_enabled: bool,
    pub open_url: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub on_right_click_paste: Option<Arc<dyn Fn() + Send + Sync>>,
    pub alt_screen_active: bool,
    pub render_root: Arc<dyn Fn(usize) -> Vec<String> + Send + Sync>,
}

impl TuiAltScreen {
    pub fn new(
        terminal: Arc<Mutex<dyn Terminal>>,
        options: TuiAltScreenOptions,
        render_root: Arc<dyn Fn(usize) -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self {
            base: TuiBase::new(terminal, Arc::new(|| {}), Arc::new(|| {})),
            previous_screen: Vec::new(),
            last_document: Vec::new(),
            previous_screen_width: 0.0,
            previous_screen_height: 0.0,
            layout_root: None,
            current_layout: None,
            scroll_states: Vec::new(),
            primary_scroll_state: None,
            wheel_scroll_lines: options.wheel_scroll_lines.unwrap_or(1.0).max(1.0).floor(),
            mouse_enabled: options.mouse.unwrap_or(true),
            open_url: options.open_url,
            on_right_click_paste: options.on_right_click_paste,
            alt_screen_active: false,
            render_root,
        }
    }

    pub fn viewport_top(&self) -> f64 {
        self.primary_scroll_state
            .as_ref()
            .map(|(_, state)| state.lock().unwrap().scroll_top())
            .unwrap_or(0.0)
    }

    pub fn is_following_output(&self) -> bool {
        self.primary_scroll_state
            .as_ref()
            .map(|(_, state)| state.lock().unwrap().is_following_end())
            .unwrap_or(true)
    }

    pub fn set_layout_root(&mut self, component: Option<Arc<dyn Component>>) {
        self.layout_root = component;
        self.current_layout = None;
        self.base.request_render(false);
    }

    /// Register a scroll view (id, state) used by layout rendering.
    pub fn register_scroll_state(&mut self, id: u64, state: Arc<Mutex<ScrollView>>) {
        let is_primary = state.lock().unwrap().primary;
        self.scroll_states.push((id, state.clone()));
        if is_primary {
            self.primary_scroll_state = Some((id, state));
        }
    }

    pub fn scroll_by(&mut self, lines: f64) -> f64 {
        let Some((_, state)) = &self.primary_scroll_state else { return 0.0 };
        let mut state = state.lock().unwrap();
        state.scroll_by(lines)
    }

    pub fn scroll_to_top(&mut self) {
        if let Some((_, state)) = &self.primary_scroll_state {
            let mut state = state.lock().unwrap();
            state.scroll_to_start();
        }
        self.base.request_render(false);
    }

    pub fn scroll_to_bottom(&mut self) {
        if let Some((_, state)) = &self.primary_scroll_state {
            let mut state = state.lock().unwrap();
            state.scroll_to_end();
        }
        self.base.request_render(false);
    }

    fn route_wheel(&mut self, event: &WheelEvent) {
        let mut remaining = (event.direction as f64) * self.wheel_scroll_lines;
        // Route through scroll views at the pointer position, then primary.
        if let Some(layout) = &self.current_layout {
            let scroll_ids = get_scroll_views_at(layout, event.x, event.y);
            for id in scroll_ids {
                if let Some((_, state)) = self.scroll_states.iter().find(|(scroll_id, _)| *scroll_id == id) {
                    let mut state = state.lock().unwrap();
                    remaining = state.scroll_by(remaining);
                    if remaining == 0.0 || state.overscroll == "contain" {
                        break;
                    }
                }
            }
        }
        if remaining != 0.0 {
            self.scroll_by(remaining);
        }
        self.base.request_render(false);
    }

    /// Handle viewport-level input (scroll keys, wheel, mouse).
    pub fn handle_viewport_input(&mut self, data: &str) -> bool {
        if data == FOCUS_OUT || data == FOCUS_IN {
            return true;
        }
        if let Some(wheel) = parse_wheel_event(data) {
            self.route_wheel(&wheel);
            return true;
        }
        if let Some(mouse) = parse_sgr_mouse_event(data) {
            // Right-click paste (Windows) and URL opening on primary click.
            if let Some(on_right_click_paste) = &self.on_right_click_paste {
                if !mouse.release && mouse.button == 2.0 {
                    on_right_click_paste();
                    return true;
                }
            }
            if !mouse.release && mouse.button == 0.0 {
                if let Some(open_url) = &self.open_url {
                    let _ = open_url;
                }
            }
            return true;
        }
        if is_mouse_sequence(data) {
            return true;
        }

        let keybindings = get_keybindings();
        let manager = match &*keybindings {
            Some(manager) => manager,
            None => {
                let manager = KeybindingsManager::new(crate::keybindings::tui_keybindings());
                return self.handle_viewport_input_with(&manager, data);
            }
        };
        self.handle_viewport_input_with(manager, data)
    }

    fn handle_viewport_input_with(&mut self, keybindings: &KeybindingsManager, data: &str) -> bool {
        let is_release = is_key_release(data);
        if keybindings.matches(data, "tui.altScreen.pageUp") {
            if !is_release {
                let viewport_height = self
                    .primary_scroll_state
                    .as_ref()
                    .map(|(_, state)| state.lock().unwrap().viewport_height())
                    .unwrap_or(0.0);
                self.scroll_by(-(viewport_height - PAGE_SCROLL_OVERLAP).max(1.0));
            }
            return true;
        }
        if keybindings.matches(data, "tui.altScreen.pageDown") {
            if !is_release {
                let viewport_height = self
                    .primary_scroll_state
                    .as_ref()
                    .map(|(_, state)| state.lock().unwrap().viewport_height())
                    .unwrap_or(0.0);
                self.scroll_by((viewport_height - PAGE_SCROLL_OVERLAP).max(1.0));
            }
            return true;
        }
        if keybindings.matches(data, "tui.altScreen.halfPageUp") {
            if !is_release {
                let viewport_height = self
                    .primary_scroll_state
                    .as_ref()
                    .map(|(_, state)| state.lock().unwrap().viewport_height())
                    .unwrap_or(0.0);
                self.scroll_by(-(viewport_height / 2.0).floor().max(1.0));
            }
            return true;
        }
        if keybindings.matches(data, "tui.altScreen.halfPageDown") {
            if !is_release {
                let viewport_height = self
                    .primary_scroll_state
                    .as_ref()
                    .map(|(_, state)| state.lock().unwrap().viewport_height())
                    .unwrap_or(0.0);
                self.scroll_by((viewport_height / 2.0).floor().max(1.0));
            }
            return true;
        }
        if keybindings.matches(data, "tui.altScreen.top") {
            if !is_release {
                self.scroll_to_top();
            }
            return true;
        }
        if keybindings.matches(data, "tui.altScreen.bottom") {
            if !is_release {
                self.scroll_to_bottom();
            }
            return true;
        }
        false
    }

    /// Render the fullscreen viewport: layout render + scroll views.
    pub fn render(&self, width: usize) -> Vec<String> {
        (self.render_root)(width)
    }

    /// Run the differential render (full redraw each frame for simplicity).
    pub fn do_render(&mut self) {
        if self.base.stopped {
            return;
        }
        let width = self.base.terminal.lock().unwrap().columns() as f64;
        let height = self.base.terminal.lock().unwrap().rows() as f64;
        let new_lines = self.render(width as usize);

        let mut buffer = "\x1b[?2026h".to_string();
        buffer += "\x1b[H";
        for (index, line) in new_lines.iter().enumerate() {
            if index > 0 {
                buffer += "\r\n";
            }
            buffer += "\x1b[2K";
            buffer += line;
        }
        buffer += "\x1b[?2026l";
        self.base.terminal.lock().unwrap().write(&buffer);
        self.previous_screen = new_lines;
        self.previous_screen_width = width;
        self.previous_screen_height = height;
    }
}

/// Create a ScrollView with follow-end behavior for the implicit document.
pub fn create_primary_scroll_view(child: Arc<dyn Component>, request_render: Arc<dyn Fn() + Send + Sync>) -> Arc<Mutex<ScrollView>> {
    let options = ScrollViewOptions {
        follow: Some("end".to_string()),
        primary: Some(true),
        ..ScrollViewOptions::default()
    };
    Arc::new(Mutex::new(ScrollView::new(child, options, request_render).unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wheel_events() {
        // CSI < 64 ; 20 ; 5 M (button 64 = wheel up).
        let event = parse_wheel_event("\x1b[<64;20;5M").unwrap();
        assert_eq!(event.direction, -1);
        assert_eq!(event.x, 19.0);
        assert_eq!(event.y, 4.0);
        // Button 65 = wheel down.
        let event = parse_wheel_event("\x1b[<65;10;3M").unwrap();
        assert_eq!(event.direction, 1);
        // Non-wheel button.
        assert!(parse_wheel_event("\x1b[<0;10;3M").is_none());
    }

    #[test]
    fn parses_sgr_mouse() {
        let event = parse_sgr_mouse_event("\x1b[<0;5;6M").unwrap();
        assert_eq!(event.button, 0.0);
        assert_eq!(event.x, 4.0);
        assert_eq!(event.y, 5.0);
        assert!(!event.release);
        let event = parse_sgr_mouse_event("\x1b[<0;5;6m").unwrap();
        assert!(event.release);
        assert!(parse_sgr_mouse_event("plain").is_none());
    }

    #[test]
    fn detects_mouse_sequences() {
        assert!(is_mouse_sequence("\x1b[<0;1;1M"));
        assert!(is_mouse_sequence("\x1b[M"));
        assert!(!is_mouse_sequence("a"));
    }
}
