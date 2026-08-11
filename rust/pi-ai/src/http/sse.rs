//! Server-Sent Events parser (incremental).
//!
//! Implements the SSE wire format used by OpenAI-compatible streaming:
//! lines are `field: value` (or `field:value`), an empty line terminates an
//! event, multiple `data:` lines are joined with `\n`, `event:` names the
//! event, comments (`: ...`) are ignored, and a trailing incomplete event is
//! discarded at end of stream.

#[derive(Clone, Debug, PartialEq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    current: Option<PendingEvent>,
}

struct PendingEvent {
    event: Option<String>,
    data_lines: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds bytes and returns any complete events (in order).
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        loop {
            let Some(newline) = self.buffer.iter().position(|b| *b == b'\n') else {
                break;
            };
            let mut line: Vec<u8> = self.buffer.drain(..=newline).collect();
            line.pop(); // strip \n
            if line.last() == Some(&b'\r') {
                line.pop(); // strip \r
            }
            if line.is_empty() {
                // Blank line terminates the current event.
                if let Some(pending) = self.current.take() {
                    events.push(SseEvent {
                        event: pending.event,
                        data: pending.data_lines.join("\n"),
                    });
                }
                continue;
            }
            if line[0] == b':' {
                continue; // comment
            }
            let (field, value) = match line.iter().position(|b| *b == b':') {
                Some(colon) => {
                    let field = String::from_utf8_lossy(&line[..colon]).to_string();
                    let mut value = &line[colon + 1..];
                    if value.first() == Some(&b' ') {
                        value = &value[1..];
                    }
                    (field, String::from_utf8_lossy(value).to_string())
                }
                None => (String::from_utf8_lossy(&line).to_string(), String::new()),
            };
            let current = self.current.get_or_insert_with(|| PendingEvent {
                event: None,
                data_lines: Vec::new(),
            });
            match field.as_str() {
                "event" => current.event = Some(value),
                "data" => current.data_lines.push(value),
                _ => {} // ignored fields (id, retry, ...)
            }
        }
        events
    }

    /// End of stream: discards a trailing incomplete event per SSE spec.
    pub fn end(&mut self) {
        self.buffer.clear();
        self.current = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_and_multi_data_events() {
        let mut parser = SseParser::new();
        let events = parser.push(b"event: response.created\ndata: {\"id\":\"1\"}\n\n");
        assert_eq!(
            events,
            vec![SseEvent {
                event: Some("response.created".to_string()),
                data: "{\"id\":\"1\"}".to_string(),
            }]
        );
    }

    #[test]
    fn parses_without_event_name_and_with_crlf() {
        let mut parser = SseParser::new();
        let events = parser.push(b"data: hello\r\n\r\n");
        assert_eq!(
            events,
            vec![SseEvent {
                event: None,
                data: "hello".to_string(),
            }]
        );
    }

    #[test]
    fn joins_multiple_data_lines() {
        let mut parser = SseParser::new();
        let events = parser.push(b"data: a\ndata: b\n\n");
        assert_eq!(
            events,
            vec![SseEvent {
                event: None,
                data: "a\nb".to_string(),
            }]
        );
    }

    #[test]
    fn handles_fragmented_chunks() {
        let mut parser = SseParser::new();
        let wire = b"event: x\ndata: {\"a\":1}\n\n";
        let mut events = Vec::new();
        for byte in wire {
            events.extend(parser.push(&[*byte]));
        }
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, Some("x".to_string()));
    }

    #[test]
    fn ignores_comments_and_trailing_incomplete_events() {
        let mut parser = SseParser::new();
        let events = parser.push(b": comment\ndata: ok\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "ok");

        // Trailing incomplete event without blank line is discarded at end.
        let events = parser.push(b"data: partial");
        assert!(events.is_empty());
        parser.end();
        let events = parser.push(b"\n\ndata: next\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "next");
    }
}
