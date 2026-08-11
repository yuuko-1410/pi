//! Benchmarks the Rust protocol implementation: encode + incremental decode
//! of a realistic session_progress event, mirroring
//! `scripts/bench-protocol-node.mjs` (same messages, same loop shape).
//!
//! Run from `rust/pi-protocol`:
//!   cargo run --release --example bench [--iterations N]
//!
//! Prints a JSON report to stdout. Zero external dependencies; memory stats
//! come from /proc (Linux only, matching the "no deps" constraint).

use std::time::Instant;

use pi_protocol::schemas::{
    AssistantItem, AssistantStatus, Content, EventEnvelope, FinishedItem, ModelRef, ServerEvent, ServerMessage,
    ToolItem, ToolStatus, TranscriptProgress, Usage, UsageCost,
};
use pi_protocol::{encode_server_message, ServerMessageDecoder, Value};

fn parse_iterations() -> usize {
    let args: Vec<String> = std::env::args().collect();
    let mut iterations = 10_000;
    if let Some(index) = args.iter().position(|arg| arg == "--iterations") {
        match args.get(index + 1).and_then(|raw| raw.parse::<usize>().ok()) {
            Some(n) => iterations = n,
            None => {
                eprintln!("Invalid --iterations value");
                std::process::exit(2);
            }
        }
    }
    iterations
}

fn tool_message() -> ServerMessage {
    ServerMessage::Event(EventEnvelope {
        event: ServerEvent::SessionProgress {
            session_id: "session-1".to_string(),
            progress: TranscriptProgress::ItemFinished {
                item: FinishedItem::ToolComplete(ToolItem {
                    id: "tool-1".to_string(),
                    tool_call_id: "call-1".to_string(),
                    tool_name: "read".to_string(),
                    input: Value::Map(vec![
                        ("path".to_string(), Value::String("/tmp/file".to_string())),
                        (
                            "lines".to_string(),
                            Value::Array(vec![Value::Number(1.0), Value::Number(2.0), Value::Number(3.0)]),
                        ),
                    ]),
                    content: vec![
                        Content::Text {
                            text: "line 1\nline 2\nline 3\n".to_string(),
                        },
                        Content::Text {
                            text: "(3 lines)".to_string(),
                        },
                    ],
                    details: Some(Value::Map(vec![
                        ("cached".to_string(), Value::Bool(false)),
                        ("size".to_string(), Value::Number(1234.0)),
                    ])),
                    usage: None,
                    timestamp: 1.0,
                    status: ToolStatus::Complete,
                }),
            },
        },
    })
}

fn assistant_message() -> ServerMessage {
    ServerMessage::Event(EventEnvelope {
        event: ServerEvent::SessionProgress {
            session_id: "session-1".to_string(),
            progress: TranscriptProgress::ItemFinished {
                item: FinishedItem::AssistantComplete(AssistantItem {
                    id: "assistant-1".to_string(),
                    content: vec![Content::Text {
                        text: "Here is the summary.".to_string(),
                    }],
                    model: ModelRef {
                        provider: "test".to_string(),
                        id: "model-1".to_string(),
                    },
                    response_model: Some("test-model".to_string()),
                    usage: Some(Usage {
                        input: 42.0,
                        output: 7.0,
                        cache_read: 0.0,
                        cache_write: 0.0,
                        reasoning: None,
                        total_tokens: 49.0,
                        cost: UsageCost {
                            input: 0.021,
                            output: 0.028,
                            cache_read: 0.0,
                            cache_write: 0.0,
                            total: 0.049,
                        },
                    }),
                    timestamp: 2.0,
                    status: AssistantStatus::Complete {
                        stop_reason: "stop".to_string(),
                    },
                }),
            },
        },
    })
}

/// Current RSS in kB from /proc/self/status (VmRSS), or None off-Linux.
fn current_rss_kb() -> Option<u64> {
    proc_status_kb("VmRSS:")
}

/// Peak RSS in kB from /proc/self/status (VmHWM), or None off-Linux.
fn peak_rss_kb() -> Option<u64> {
    proc_status_kb("VmHWM:")
}

fn proc_status_kb(label: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with(label))?;
    let value = line.split_whitespace().nth(1)?;
    value.parse().ok()
}

fn main() {
    let iterations = parse_iterations();
    let messages = [tool_message(), assistant_message()];

    // Warmup: pre-encode both message shapes before measuring.
    let warmup = iterations.min(1_000);
    for i in 0..warmup {
        encode_server_message(&messages[i % 2], None).expect("warmup encode");
    }

    let start = Instant::now();
    let mut decoder = ServerMessageDecoder::new("server", None).expect("decoder");
    let mut decoded_messages = 0usize;
    for i in 0..iterations {
        let frame = encode_server_message(&messages[i % 2], None).expect("encode");
        decoded_messages += decoder.push_messages(&frame).expect("decode").len();
    }
    decoder.end().expect("end");
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    if decoded_messages != iterations {
        eprintln!(
            "Expected {iterations} decoded messages, got {decoded_messages}"
        );
        std::process::exit(1);
    }

    let platform = std::env::consts::OS;
    let (rss_kb, rss_peak_kb) = match (current_rss_kb(), peak_rss_kb()) {
        (Some(current), Some(peak)) => (Some(current), Some(peak)),
        _ => (None, None),
    };

    let report = serde_json_lite_report(&[
        ("package", json_string("pi-protocol")),
        ("runtime", json_string(&format!("rustc ({} bits)", std::mem::size_of::<usize>() * 8))),
        ("platform", json_string(platform)),
        ("iterations", json_number(iterations as f64)),
        ("messagesEncoded", json_number(iterations as f64)),
        ("messagesDecoded", json_number(decoded_messages as f64)),
        ("elapsed_ms", json_number(round2(elapsed_ms))),
        ("messagesPerSecond", json_number(round2(iterations as f64 / elapsed_ms * 1000.0))),
        ("rss_kb", match rss_kb { Some(v) => json_number(v as f64), None => "null".to_string() }),
        ("rss_peak_kb", match rss_peak_kb { Some(v) => json_number(v as f64), None => "null".to_string() }),
    ]);
    println!("{report}");
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn json_string(value: &str) -> String {
    // Values are controlled (no quotes/backslashes), but escape defensively.
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_number(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn serde_json_lite_report(entries: &[(&str, String)]) -> String {
    let mut out = String::from("{\n");
    for (i, (key, value)) in entries.iter().enumerate() {
        out.push_str("  ");
        out.push_str(&json_string(key));
        out.push_str(": ");
        out.push_str(value);
        if i + 1 < entries.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push('}');
    out
}
