//! Default stream fn registry, port of `packages/agent/src/stream-fn.ts`.

use std::sync::{Arc, Mutex, OnceLock};

use pi_ai::event_stream::AssistantMessageEventStream;
use pi_ai::types::{Context, Model, SimpleStreamOptions};


static DEFAULT_STREAM_FN: OnceLock<Mutex<Option<Arc<dyn Fn(&Model, &Context, Option<&SimpleStreamOptions>) -> AssistantMessageEventStream + Send + Sync>>>> =
    OnceLock::new();

fn registry() -> &'static Mutex<Option<Arc<dyn Fn(&Model, &Context, Option<&SimpleStreamOptions>) -> AssistantMessageEventStream + Send + Sync>>> {
    DEFAULT_STREAM_FN.get_or_init(|| Mutex::new(None))
}

/// Configure the fallback used by Agent and low-level loops when callers
/// omit stream_fn.
pub fn set_default_stream_fn(
    stream_fn: Option<Arc<dyn Fn(&Model, &Context, Option<&SimpleStreamOptions>) -> AssistantMessageEventStream + Send + Sync>>,
) {
    *registry().lock().unwrap() = stream_fn;
}

pub fn get_default_stream_fn() -> Result<Arc<dyn Fn(&Model, &Context, Option<&SimpleStreamOptions>) -> AssistantMessageEventStream + Send + Sync>, String> {
    let guard = registry().lock().unwrap();
    guard
        .clone()
        .ok_or_else(|| "No default stream function configured. Pass streamFn explicitly or call setDefaultStreamFn().".to_string())
}

