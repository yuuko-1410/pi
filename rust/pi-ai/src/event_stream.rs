//! Generic event stream with result extraction.
//!
//! Rust port of `packages/ai/src/utils/event-stream.ts`. The JS class is an
//! async-iterable with a push/end producer API; Rust mirrors it with a
//! mutex+condvar queue so the same producer semantics hold:
//! - `push` after completion is ignored;
//! - a completing event settles the final result;
//! - `end(result?)` marks completion and wakes waiters;
//! - iteration drains queued events, then blocks until more arrive or the
//!   stream completes (single-consumer semantics; the JS implementation also
//!   effectively supports one consumer).
//! `result()` blocks until a final result is available (the JS promise never
//! resolves when neither a completing event nor an explicit end result
//! arrives — same behavior here).

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

struct StreamState<T, R> {
    queue: VecDeque<T>,
    waiting: usize,
    done: bool,
    result: Option<R>,
}

pub struct EventStream<T, R> {
    state: Arc<(Mutex<StreamState<T, R>>, Condvar)>,
    is_complete: Arc<dyn Fn(&T) -> bool + Send + Sync>,
    extract_result: Arc<dyn Fn(&T) -> R + Send + Sync>,
}

impl<T, R> Clone for EventStream<T, R> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            is_complete: self.is_complete.clone(),
            extract_result: self.extract_result.clone(),
        }
    }
}

impl<T, R> EventStream<T, R>
where
    R: Clone,
{
    pub fn new(
        is_complete: impl Fn(&T) -> bool + Send + Sync + 'static,
        extract_result: impl Fn(&T) -> R + Send + Sync + 'static,
    ) -> Self {
        Self {
            state: Arc::new((
                Mutex::new(StreamState {
                    queue: VecDeque::new(),
                    waiting: 0,
                    done: false,
                    result: None,
                }),
                Condvar::new(),
            )),
            is_complete: Arc::new(is_complete),
            extract_result: Arc::new(extract_result),
        }
    }

    pub fn push(&self, event: T) {
        let (lock, condvar) = &*self.state;
        let mut state = lock.lock().unwrap();
        if state.done {
            return;
        }
        if (self.is_complete)(&event) {
            state.done = true;
            state.result = Some((self.extract_result)(&event));
        }
        // Mirror the JS waiter-vs-queue split: a waiting consumer is woken
        // and drains from the queue; otherwise the event is queued.
        state.queue.push_back(event);
        if state.waiting > 0 {
            condvar.notify_one();
        }
    }

    pub fn end(&self, result: Option<R>) {
        let (lock, condvar) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.done = true;
        if result.is_some() {
            state.result = result;
        }
        // Notify all waiting consumers that the stream is done.
        condvar.notify_all();
    }

    /// Drains the next event, blocking until one arrives or the stream ends.
    /// Returns `None` after completion once the queue is empty.
    pub fn next(&self) -> Option<T> {
        let (lock, condvar) = &*self.state;
        let mut state = lock.lock().unwrap();
        loop {
            if let Some(event) = state.queue.pop_front() {
                return Some(event);
            }
            if state.done {
                return None;
            }
            state.waiting += 1;
            let guard = condvar.wait(state).unwrap();
            state = guard;
            state.waiting -= 1;
        }
    }

    /// Returns the final result once available, blocking until then. Mirrors
    /// `result(): Promise<R>` which resolves on the completing event or an
    /// explicit `end(result)`.
    pub fn result(&self) -> R {
        let (lock, condvar) = &*self.state;
        let mut state = lock.lock().unwrap();
        loop {
            if let Some(result) = &state.result {
                return result.clone();
            }
            state = condvar.wait(state).unwrap();
        }
    }
}

/// The assistant-message event stream: completes on `done`/`error` events,
/// extracting the final `AssistantMessage`.
pub struct AssistantMessageEventStream {
    inner: EventStream<crate::types::AssistantMessageEvent, crate::types::AssistantMessage>,
}

impl AssistantMessageEventStream {
    pub fn new() -> Self {
        Self {
            inner: EventStream::new(
                |event| {
                    matches!(
                        event,
                        crate::types::AssistantMessageEvent::Done { .. }
                            | crate::types::AssistantMessageEvent::Error { .. }
                    )
                },
                |event| match event {
                    crate::types::AssistantMessageEvent::Done { message, .. }
                    | crate::types::AssistantMessageEvent::Error { error: message, .. } => message.clone(),
                    _ => panic!("Unexpected event type for final result"),
                },
            ),
        }
    }

    pub fn push(&self, event: crate::types::AssistantMessageEvent) {
        self.inner.push(event);
    }

    pub fn end(&self, result: Option<crate::types::AssistantMessage>) {
        self.inner.end(result);
    }

    pub fn next(&self) -> Option<crate::types::AssistantMessageEvent> {
        self.inner.next()
    }

    pub fn result(&self) -> crate::types::AssistantMessage {
        self.inner.result()
    }
}

impl Default for AssistantMessageEventStream {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for AssistantMessageEventStream {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// Factory function for AssistantMessageEventStream (for use in extensions).
pub fn create_assistant_message_event_stream() -> AssistantMessageEventStream {
    AssistantMessageEventStream::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AssistantMessage, AssistantMessageEvent, StopReason, Usage, UsageCost, Content,
    };

    fn message(stop_reason: StopReason) -> AssistantMessage {
        AssistantMessage {
            content: vec![Content::Text(crate::types::TextContent {
                text: "hello".to_string(),
                text_signature: None,
            })],
            api: "test".to_string(),
            provider: "test".to_string(),
            model: "m".to_string(),
            response_model: None,
            response_id: None,
            usage: Usage {
                input: 1.0,
                output: 1.0,
                cache_read: 0.0,
                cache_write: 0.0,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: 2.0,
                cost: UsageCost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    total: 0.0,
                },
            },
            stop_reason,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 1.0,
        }
    }

    #[test]
    fn queues_events_then_drains_in_order() {
        let stream = AssistantMessageEventStream::new();
        stream.push(AssistantMessageEvent::Start {
            partial: message(StopReason::Pending),
        });
        stream.push(AssistantMessageEvent::TextStart {
            content_index: 0.0,
            partial: message(StopReason::Pending),
        });
        stream.end(None);

        let events: Vec<_> = std::iter::from_fn(|| stream.next()).collect();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], AssistantMessageEvent::Start { .. }));
        assert!(matches!(events[1], AssistantMessageEvent::TextStart { .. }));
    }

    #[test]
    fn push_after_end_is_ignored() {
        let stream = AssistantMessageEventStream::new();
        stream.push(AssistantMessageEvent::Start {
            partial: message(StopReason::Pending),
        });
        stream.end(None);
        // Events queued before end are still delivered; pushes after end are
        // ignored (mirroring the JS `if (this.done) return` guard).
        stream.push(AssistantMessageEvent::TextStart {
            content_index: 0.0,
            partial: message(StopReason::Pending),
        });
        assert!(matches!(stream.next(), Some(AssistantMessageEvent::Start { .. })));
        assert_eq!(stream.next(), None);
    }

    #[test]
    fn completing_event_sets_the_final_result() {
        let stream = AssistantMessageEventStream::new();
        let done = message(StopReason::Stop);
        stream.push(AssistantMessageEvent::Done {
            reason: "stop".to_string(),
            message: done.clone(),
        });
        assert_eq!(stream.result().stop_reason, StopReason::Stop);
        // The completing event is still delivered to the consumer.
        assert!(matches!(
            stream.next(),
            Some(AssistantMessageEvent::Done { .. })
        ));
        assert_eq!(stream.next(), None);
    }

    #[test]
    fn error_event_extracts_the_error_message() {
        let stream = AssistantMessageEventStream::new();
        let error = message(StopReason::Error);
        stream.push(AssistantMessageEvent::Error {
            reason: "error".to_string(),
            error: error.clone(),
        });
        assert_eq!(stream.result().stop_reason, StopReason::Error);
    }

    #[test]
    fn end_with_result_sets_the_final_result() {
        let stream = AssistantMessageEventStream::new();
        let final_message = message(StopReason::Aborted);
        stream.end(Some(final_message));
        assert_eq!(stream.result().stop_reason, StopReason::Aborted);
    }

    #[test]
    fn end_without_result_leaves_result_pending_but_stream_ends() {
        let stream = AssistantMessageEventStream::new();
        stream.push(AssistantMessageEvent::Start {
            partial: message(StopReason::Pending),
        });
        stream.end(None);
        // The queued event is still delivered, then the stream ends. The
        // JS `result()` promise never resolves in this case; the Rust
        // `result()` would block forever, which is intentionally not tested.
        assert!(matches!(stream.next(), Some(AssistantMessageEvent::Start { .. })));
        assert_eq!(stream.next(), None);
    }
}
