//! Startup timing instrumentation, port of `core/timings.ts`.
//!
//! Enabled by the PI_TIMING=1 environment variable; otherwise all calls are
//! no-ops (same cost model as the JS early-return).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static TIMING_STATE: Mutex<Option<TimingState>> = Mutex::new(None);

struct TimingNamespace {
    timings: Vec<(String, f64)>,
    last_time: f64,
}

struct TimingState {
    namespaces: HashMap<String, TimingNamespace>,
}

fn enabled() -> bool {
    std::env::var("PI_TIMING").as_deref() == Ok("1")
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

pub fn reset_timings(namespace: &str) {
    if !enabled() {
        return;
    }
    let mut state = TIMING_STATE.lock().unwrap();
    if state.is_none() {
        *state = Some(TimingState {
            namespaces: HashMap::new(),
        });
    }
    state.as_mut().unwrap().namespaces.insert(
        namespace.to_string(),
        TimingNamespace {
            timings: Vec::new(),
            last_time: now_ms(),
        },
    );
}

pub fn time(label: &str, namespace: &str) {
    if !enabled() {
        return;
    }
    let mut state = TIMING_STATE.lock().unwrap();
    if state.is_none() {
        *state = Some(TimingState {
            namespaces: HashMap::new(),
        });
    }
    if !state.as_ref().unwrap().namespaces.contains_key(namespace) {
        drop(state);
        reset_timings(namespace);
        state = TIMING_STATE.lock().unwrap();
    }
    let now = now_ms();
    if let Some(ns) = state.as_mut().unwrap().namespaces.get_mut(namespace) {
        ns.timings.push((label.to_string(), now - ns.last_time));
        ns.last_time = now;
    }
}

fn print_group(title: &str, timings: &[(String, f64)]) {
    let printable: Vec<&(String, f64)> = timings.iter().filter(|(_, ms)| *ms >= 0.0).collect();
    if printable.is_empty() {
        return;
    }
    eprintln!("\n--- {title} ---");
    for (label, ms) in &printable {
        eprintln!("  {label}: {ms}ms");
    }
    let total: f64 = printable.iter().map(|(_, ms)| ms).sum();
    eprintln!("  TOTAL: {total}ms");
    eprintln!("{}", "-".repeat(title.len() + 8));
}

pub fn print_timings() {
    if !enabled() {
        return;
    }
    let state = TIMING_STATE.lock().unwrap();
    if let Some(state) = state.as_ref() {
        for (namespace, ns) in &state.namespaces {
            print_group(&format!("Startup Timings: {namespace}"), &ns.timings);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_default() {
        // PI_TIMING unset in test env: no-op calls must not panic.
        time("x", "main");
        print_timings();
    }
}
