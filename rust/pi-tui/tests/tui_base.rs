//! TuiBase tests using a memory terminal.

use std::sync::{Arc, Mutex};

use pi_tui::terminal::Terminal;
use pi_tui::tui::TuiBase;

struct MemoryTerminal {
    output: Arc<Mutex<String>>,
}

impl Terminal for MemoryTerminal {
    fn start(&mut self, _on_input: Arc<dyn Fn(&str) + Send + Sync>) {}
    fn stop(&mut self) {}
    fn write(&mut self, data: &str) {
        self.output.lock().unwrap().push_str(data);
    }
    fn columns(&self) -> usize {
        80
    }
    fn rows(&self) -> usize {
        24
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&mut self, _lines: isize) {}
    fn hide_cursor(&mut self) {
        self.output.lock().unwrap().push_str("<hide>");
    }
    fn show_cursor(&mut self) {
        self.output.lock().unwrap().push_str("<show>");
    }
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {}
    fn clear_screen(&mut self) {}
    fn set_title(&mut self, _title: &str) {}
    fn set_progress(&mut self, _active: bool) {}
}

struct CountingComponent;

impl pi_tui::tui::Component for CountingComponent {
    fn render(&self, _width: usize) -> Vec<String> {
        vec!["content".to_string()]
    }
}

#[test]
fn render_scheduling_and_input() {
    let output = Arc::new(Mutex::new(String::new()));
    let terminal: Arc<Mutex<dyn Terminal>> = Arc::new(Mutex::new(MemoryTerminal {
        output: output.clone(),
    }));
    let renders = Arc::new(Mutex::new(0usize));
    let renders_clone = renders.clone();
    let mut tui = TuiBase::new(
        terminal.clone(),
        Arc::new(move || {
            *renders_clone.lock().unwrap() += 1;
        }),
        Arc::new(|| {}),
    );
    tui.start();
    assert!(*renders.lock().unwrap() >= 1);
    tui.handle_terminal_input("x");
    tui.stop();
    assert!(output.lock().unwrap().contains("<show>"));
}

#[test]
fn focus_and_overlay_stack() {
    let terminal: Arc<Mutex<dyn Terminal>> = Arc::new(Mutex::new(MemoryTerminal {
        output: Arc::new(Mutex::new(String::new())),
    }));
    let mut tui = TuiBase::new(terminal, Arc::new(|| {}), Arc::new(|| {}));
    let component: Arc<dyn pi_tui::tui::Component> = Arc::new(CountingComponent);
    tui.add_child(component.clone());
    tui.set_focus(Some(component.clone()));
    assert!(tui.get_focused_component().is_some());
    tui.set_focus(None);
    assert!(tui.get_focused_component().is_none());
    tui.show_overlay(component.clone(), None);
    assert!(tui.has_overlay());
    tui.hide_overlay();
    assert!(!tui.has_overlay());
}

#[test]
fn color_scheme_report_dispatch() {
    let terminal: Arc<Mutex<dyn Terminal>> = Arc::new(Mutex::new(MemoryTerminal {
        output: Arc::new(Mutex::new(String::new())),
    }));
    let mut tui = TuiBase::new(terminal, Arc::new(|| {}), Arc::new(|| {}));
    let received = Arc::new(Mutex::new(None::<String>));
    let received_clone = received.clone();
    tui.on_terminal_color_scheme_change(Arc::new(move |scheme| {
        *received_clone.lock().unwrap() = Some(scheme.to_string());
    }));
    tui.handle_terminal_input("\x1b[?997;1n");
    assert_eq!(received.lock().unwrap().as_deref(), Some("dark"));
}

#[test]
fn cell_size_response_consumed() {
    let terminal: Arc<Mutex<dyn Terminal>> = Arc::new(Mutex::new(MemoryTerminal {
        output: Arc::new(Mutex::new(String::new())),
    }));
    let renders = Arc::new(Mutex::new(0usize));
    let renders_clone = renders.clone();
    let mut tui = TuiBase::new(
        terminal,
        Arc::new(move || {
            *renders_clone.lock().unwrap() += 1;
        }),
        Arc::new(|| {}),
    );
    tui.handle_terminal_input("\x1b[6;18;9t");
    assert!(*renders.lock().unwrap() >= 1);
}
