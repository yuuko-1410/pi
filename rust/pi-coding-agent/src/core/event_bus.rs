//! Process-wide event bus, port of `core/event-bus.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct BusInner {
    handlers: HashMap<String, Vec<Arc<dyn Fn(&dyn std::any::Any) + Send + Sync>>>,
}

/// Event bus: emit pushes to all handlers of a channel; handler panics are
/// contained (logged) and never propagate, mirroring the JS wrapper.
#[derive(Clone, Default)]
pub struct EventBus {
    inner: Arc<Mutex<BusInner>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn emit(&self, channel: &str, data: &dyn std::any::Any) {
        let handlers = {
            let inner = self.inner.lock().unwrap();
            inner.handlers.get(channel).cloned().unwrap_or_default()
        };
        for handler in handlers {
            handler(data);
        }
    }

    pub fn on<F>(&self, channel: &str, handler: F) -> Arc<dyn Fn() + Send + Sync>
    where
        F: Fn(&dyn std::any::Any) + Send + Sync + 'static,
    {
        let channel_name = channel.to_string();
        let error_channel = channel_name.clone();
        let wrapped: Arc<dyn Fn(&dyn std::any::Any) + Send + Sync> = Arc::new(move |data| {
            if let Err(error) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(data))) {
                eprintln!("Event handler error ({}): {:?}", error_channel, error);
            }
        });
        self.inner
            .lock()
            .unwrap()
            .handlers
            .entry(channel.to_string())
            .or_default()
            .push(wrapped.clone());
        // Unsubscribe closure mirrors JS's returned off() function.
        let inner = self.inner.clone();
        Arc::new(move || {
            inner
                .lock()
                .unwrap()
                .handlers
                .entry(channel_name.clone())
                .or_default()
                .retain(|h| !Arc::ptr_eq(h, &wrapped));
        })
    }

    pub fn clear(&self) {
        self.inner.lock().unwrap().handlers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_reaches_handler_and_off_unsubscribes() {
        let bus = EventBus::new();
        let received = Arc::new(Mutex::new(0));
        let r = received.clone();
        let off = bus.on("chan", move |_| {
            *r.lock().unwrap() += 1;
        });
        bus.emit("chan", &1i32);
        assert_eq!(*received.lock().unwrap(), 1);
        off();
        bus.emit("chan", &2i32);
        assert_eq!(*received.lock().unwrap(), 1);
    }

    #[test]
    fn channels_are_isolated() {
        let bus = EventBus::new();
        let received = Arc::new(Mutex::new(0));
        let r = received.clone();
        bus.on("a", move |_| {
            *r.lock().unwrap() += 1;
        });
        bus.emit("b", &1i32);
        assert_eq!(*received.lock().unwrap(), 0);
        bus.clear();
    }

    #[test]
    fn panicking_handler_is_contained() {
        let bus = EventBus::new();
        bus.on("chan", |_| panic!("boom"));
        bus.emit("chan", &1i32); // must not propagate
    }
}
