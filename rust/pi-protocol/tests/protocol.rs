//! Port of `packages/protocol/test/protocol.test.ts`.

use pi_protocol::cbor::{decode_cbor, encode_cbor, CborOptions, Value};
use pi_protocol::framing::{encode_frame, FrameDecoder};
use pi_protocol::schemas::{
    AssistantItem, AssistantStatus, ClientMessage, Command, CommandResult, Content, ModelCost, ModelMetadata,
    ModelRef, ProtocolError, ProtocolErrorCode, ResponseEnvelope, ServerMessage, ServerSnapshot, SessionMetadata,
    SessionSnapshot, ToolItem, ToolStatus, Usage, UsageCost,
};
use pi_protocol::{
    encode_client_message, encode_server_message, is_supported_protocol_version, parse_client_message,
    parse_server_message, ClientMessageDecoder, ProtocolValidationError, ServerMessageDecoder, PROTOCOL_VERSION,
};

fn map(entries: &[(&str, Value)]) -> Value {
    Value::Map(entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
}

fn str(s: &str) -> Value {
    Value::String(s.to_string())
}

fn num(n: f64) -> Value {
    Value::Number(n)
}

fn arr(items: &[Value]) -> Value {
    Value::Array(items.to_vec())
}

fn boo(b: bool) -> Value {
    Value::Bool(b)
}

fn null() -> Value {
    Value::Null
}

fn empty_server_snapshot() -> ServerSnapshot {
    ServerSnapshot {
        server_id: "server-1".to_string(),
        protocol_version: PROTOCOL_VERSION,
        revision: 0.0,
        sessions: vec![],
        models: vec![],
    }
}

fn client_hello(version: f64) -> ClientMessage {
    ClientMessage::Hello { version }
}

fn server_hello() -> ServerMessage {
    ServerMessage::Hello {
        connection_id: "connection-1".to_string(),
        snapshot: empty_server_snapshot(),
    }
}

fn item_message(item: Value, kind: &str) -> Value {
    map(&[
        ("type", str("event")),
        (
            "event",
            map(&[
                ("type", str("session_progress")),
                ("sessionId", str("session-1")),
                (
                    "progress",
                    map(&[("type", str(kind)), ("item", item)]),
                ),
            ]),
        ),
    ])
}

fn assistant_item_value(extra: &[(&str, Value)]) -> Value {
    let mut entries = vec![
        ("id", str("assistant-1")),
        ("role", str("assistant")),
        ("content", arr(&[map(&[("type", str("text")), ("text", str("hello"))])])),
        ("model", map(&[("provider", str("test")), ("id", str("model"))])),
        ("timestamp", num(1.0)),
    ];
    entries.extend_from_slice(extra);
    map(&entries)
}

fn tool_item_value(extra: &[(&str, Value)]) -> Value {
    let mut entries = vec![
        ("id", str("tool-1")),
        ("role", str("tool")),
        ("toolCallId", str("call-1")),
        ("toolName", str("read")),
        ("input", map(&[])),
        ("content", arr(&[])),
        ("timestamp", num(1.0)),
    ];
    entries.extend_from_slice(extra);
    map(&entries)
}

fn usage() -> Usage {
    Usage {
        input: 1.0,
        output: 2.0,
        cache_read: 3.0,
        cache_write: 4.0,
        reasoning: Some(5.0),
        total_tokens: 15.0,
        cost: UsageCost {
            input: 0.1,
            output: 0.2,
            cache_read: 0.3,
            cache_write: 0.4,
            total: 1.0,
        },
    }
}

#[test]
fn uses_protocol_version_1() {
    assert_eq!(PROTOCOL_VERSION, 1.0);
    assert!(is_supported_protocol_version(1.0));
    assert!(!is_supported_protocol_version(2.0));
    assert!(!is_supported_protocol_version(2.5));
}

#[test]
fn accepts_integer_client_hello_versions_for_negotiation() {
    for version in [0.0, PROTOCOL_VERSION, PROTOCOL_VERSION + 1.0] {
        let message = client_hello(version);
        assert_eq!(parse_client_message(&message.to_value()).unwrap(), message);
    }
}

#[test]
fn rejects_a_handshake_with_wrong_version_type_or_extra_fields() {
    let cases: Vec<Value> = vec![
        // JSON strings are not wire messages (no plain-object shape).
        str("{\"type\":\"hello\",\"version\":1}"),
        // string version
        map(&[("type", str("hello")), ("version", str("1"))]),
        // fractional version
        map(&[("type", str("hello")), ("version", num(1.5))]),
        // credential field
        map(&[
            ("type", str("hello")),
            ("version", num(1.0)),
            ("token", str("secret")),
        ]),
        // unknown field
        map(&[
            ("type", str("hello")),
            ("version", num(1.0)),
            ("extra", boo(true)),
        ]),
    ];
    for case in cases {
        let error = parse_client_message(&case).unwrap_err();
        assert!(matches!(error, ProtocolValidationError(_)));
    }
}

#[test]
fn rejects_image_input_while_the_mvp_remains_text_only() {
    let case = map(&[
        ("type", str("request")),
        ("id", str("request-1")),
        (
            "request",
            map(&[
                ("command", str("prompt")),
                ("sessionId", str("session-1")),
                ("text", str("inspect")),
                ("images", arr(&[map(&[("type", str("image")), ("data", str("abc")), ("mimeType", str("image/png"))])])),
            ]),
        ),
    ]);
    assert!(parse_client_message(&case).is_err());
}

#[test]
fn parses_a_server_handshake_snapshot() {
    let message = server_hello();
    assert_eq!(parse_server_message(&message.to_value()).unwrap(), message);
}

#[test]
fn represents_listed_sessions_as_durable_metadata() {
    let message = ServerMessage::Response(ResponseEnvelope::Ok {
        id: "request-1".to_string(),
        result: CommandResult::List {
            sessions: vec![SessionMetadata {
                id: "session-1".to_string(),
                created_at: 1.0,
                updated_at: Some(2.0),
                parent_session_id: Some("parent-1".to_string()),
                session_name: Some("Named session".to_string()),
                cwd: Some("/workspace".to_string()),
            }],
        },
    });
    let parsed = parse_server_message(&message.to_value()).unwrap();
    assert_eq!(parsed, message);

    // Missing required metadata fields (phase instead of cwd) must be rejected.
    let bad = map(&[
        ("type", str("response")),
        ("id", str("request-1")),
        ("ok", boo(true)),
        (
            "result",
            map(&[
                ("command", str("list")),
                (
                    "sessions",
                    arr(&[map(&[
                        ("id", str("session-1")),
                        ("createdAt", num(1.0)),
                        ("phase", str("idle")),
                    ])]),
                ),
            ]),
        ),
    ]);
    assert!(parse_server_message(&bad).is_err());
}

#[test]
fn accepts_the_error_codes() {
    for code in ["not_implemented", "internal_error"] {
        let message = ServerMessage::Response(ResponseEnvelope::Err {
            id: "request-1".to_string(),
            error: ProtocolError {
                code: ProtocolErrorCode::parse(code).expect("validated error code"),
                message: "safe".to_string(),
                details: None,
            },
        });
        let parsed = parse_server_message(&message.to_value()).unwrap();
        assert_eq!(parsed, message);
    }
}

#[test]
fn rejects_invalid_server_messages() {
    let cases: Vec<Value> = vec![
        map(&[
            ("type", str("hello")),
            ("version", num(2.0)),
            ("connectionId", str("connection-1")),
            ("snapshot", empty_server_snapshot().to_value()),
        ]),
        map(&[
            ("type", str("hello_error")),
            ("error", map(&[("code", str("auth")), ("message", str("Authentication failed"))])),
        ]),
        map(&[
            ("type", str("response")),
            ("id", str("request-1")),
            ("ok", boo(true)),
            ("result", map(&[("command", str("unknown"))])),
        ]),
        map(&[
            ("type", str("event")),
            ("event", map(&[("type", str("session_removed")), ("sessionId", num(42.0))])),
        ]),
    ];
    for case in cases {
        assert!(parse_server_message(&case).is_err(), "expected rejection for {case:?}");
    }
}

#[test]
fn validates_nested_json_tool_details() {
    // Constructed standalone (not via tool_item_value) so no duplicate keys
    // mask the nested values the JS test actually validates.
    let case = item_message(
        map(&[
            ("id", str("tool-1")),
            ("role", str("tool")),
            ("toolCallId", str("call-1")),
            ("toolName", str("read")),
            ("input", map(&[("path", str("/tmp/file"))])),
            (
                "content",
                arr(&[map(&[("type", str("text")), ("text", str("done"))])]),
            ),
            (
                "details",
                map(&[("lines", arr(&[num(1.0), num(2.0), num(3.0)])), ("cached", boo(false))]),
            ),
            ("status", str("complete")),
            ("isError", boo(false)),
            ("timestamp", num(1.0)),
        ]),
        "item_finished",
    );
    let parsed = parse_server_message(&case).unwrap();
    // Verify the nested JSON values actually survived validation.
    let pi_protocol::schemas::ServerMessage::Event(pi_protocol::schemas::EventEnvelope {
        event: pi_protocol::schemas::ServerEvent::SessionProgress {
            progress:
                pi_protocol::schemas::TranscriptProgress::ItemFinished {
                    item: pi_protocol::schemas::FinishedItem::ToolComplete(tool),
                },
            ..
        },
    }) = parsed
    else {
        panic!("unexpected message shape");
    };
    assert_eq!(
        tool.input,
        map(&[("path", str("/tmp/file"))]),
        "nested tool input must be preserved"
    );
}

#[test]
fn accepts_consistent_assistant_items() {
    let states: Vec<Vec<(&str, Value)>> = vec![
        vec![("status", str("streaming"))],
        vec![("status", str("complete")), ("stopReason", str("stop"))],
        vec![("status", str("error")), ("stopReason", str("error"))],
        vec![
            ("status", str("error")),
            ("stopReason", str("error")),
            ("errorMessage", str("failed")),
        ],
        vec![("status", str("aborted")), ("stopReason", str("aborted"))],
    ];
    for (i, state) in states.iter().enumerate() {
        let kind = if i == 0 { "item_updated" } else { "item_finished" };
        let case = item_message(assistant_item_value(state), kind);
        assert!(parse_server_message(&case).is_ok(), "expected accept for state {state:?}");
    }
}

#[test]
fn rejects_inconsistent_assistant_items() {
    let states: Vec<Vec<(&str, Value)>> = vec![
        // streaming with stopReason
        vec![("status", str("streaming")), ("stopReason", str("stop"))],
        // complete without stopReason
        vec![("status", str("complete"))],
        // complete with wrong stopReason
        vec![("status", str("complete")), ("stopReason", str("error"))],
        // error with empty errorMessage
        vec![
            ("status", str("error")),
            ("stopReason", str("error")),
            ("errorMessage", str("")),
        ],
        // aborted with wrong stopReason
        vec![("status", str("aborted")), ("stopReason", str("stop"))],
    ];
    for state in states {
        let case = item_message(assistant_item_value(&state), "item_finished");
        assert!(parse_server_message(&case).is_err(), "expected rejection for state {state:?}");
    }
}

#[test]
fn accepts_consistent_tool_items() {
    let states: Vec<Vec<(&str, Value)>> = vec![
        vec![("status", str("running")), ("isError", boo(false))],
        vec![("status", str("complete")), ("isError", boo(false))],
        vec![("status", str("error")), ("isError", boo(true))],
    ];
    for (i, state) in states.iter().enumerate() {
        let kind = if i == 0 { "item_updated" } else { "item_finished" };
        let case = item_message(tool_item_value(state), kind);
        assert!(parse_server_message(&case).is_ok(), "expected accept for state {state:?}");
    }
}

#[test]
fn rejects_nonterminal_items_reported_as_finished() {
    let assistant = assistant_item_value(&[("status", str("streaming"))]);
    assert!(parse_server_message(&item_message(assistant, "item_finished")).is_err());

    let tool = tool_item_value(&[("status", str("running")), ("isError", boo(false))]);
    assert!(parse_server_message(&item_message(tool, "item_finished")).is_err());
}

#[test]
fn rejects_inconsistent_tool_items() {
    let states: Vec<Vec<(&str, Value)>> = vec![
        vec![("status", str("running")), ("isError", boo(true))],
        vec![("status", str("complete")), ("isError", boo(true))],
        vec![("status", str("error")), ("isError", boo(false))],
    ];
    for state in states {
        let case = item_message(tool_item_value(&state), "item_finished");
        assert!(parse_server_message(&case).is_err(), "expected rejection for state {state:?}");
    }
}

#[test]
fn validation_errors_do_not_retain_rejected_payloads() {
    let case = map(&[
        ("type", str("hello")),
        ("version", str("1")),
        ("extra", str(&"x".repeat(2_000_000))),
    ]);
    let error = parse_client_message(&case).unwrap_err();
    // The error is a plain message, no payload retained; message stays small.
    assert!(error.0.len() < 1_000);
}

#[test]
fn encodes_complete_client_and_server_frames() {
    let mut frames = FrameDecoder::new();
    let client_frames = frames
        .push(&encode_client_message(&client_hello(1.0), None).unwrap())
        .unwrap();
    assert_eq!(client_frames.len(), 1);
    let parsed = parse_client_message(&decode_cbor(&client_frames[0], &CborOptions::default()).unwrap()).unwrap();
    assert_eq!(parsed, client_hello(1.0));

    let server_frames = frames
        .push(&encode_server_message(&server_hello(), None).unwrap())
        .unwrap();
    assert_eq!(server_frames.len(), 1);
    let parsed = parse_server_message(&decode_cbor(&server_frames[0], &CborOptions::default()).unwrap()).unwrap();
    assert_eq!(parsed, server_hello());
}

#[test]
fn validates_messages_before_encoding() {
    // JS rejects encodeClientMessage({ type: "hello", version: 1.5 }); the
    // Rust model can carry the same invalid value, so encoding must reject it.
    let error = encode_client_message(&client_hello(1.5), None).unwrap_err();
    assert!(matches!(error, ProtocolValidationError(_)));
}

#[test]
fn enforces_an_outbound_frame_limit_before_returning_encoded_bytes() {
    assert!(encode_client_message(&client_hello(1.0), Some(8)).is_err());
    assert!(encode_server_message(&server_hello(), Some(8)).is_err());
}

#[test]
fn omits_explicit_undefined_optional_properties_on_the_wire() {
    // JS: { command: "create", cwd: undefined, name: undefined }; Rust None
    // fields are simply not encoded, which produces the same bytes.
    let message = ClientMessage::Request {
        id: "request-1".to_string(),
        request: Command::Create {
            cwd: None,
            name: None,
            model: None,
            thinking_level: None,
        },
    };
    let mut frames = FrameDecoder::new();
    let payload = frames
        .push(&encode_client_message(&message, None).unwrap())
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        decode_cbor(&payload, &CborOptions::default()).unwrap(),
        map(&[
            ("type", str("request")),
            ("id", str("request-1")),
            ("request", map(&[("command", str("create"))])),
        ])
    );
}

#[test]
fn incrementally_decodes_fragmented_and_coalesced_client_messages() {
    let request = ClientMessage::Request {
        id: "request-1".to_string(),
        request: Command::List,
    };
    let first = encode_client_message(&client_hello(1.0), None).unwrap();
    let second = encode_client_message(&request, None).unwrap();
    let mut wire = Vec::with_capacity(first.len() + second.len());
    wire.extend_from_slice(&first);
    wire.extend_from_slice(&second);

    for split in 0..=wire.len() {
        let mut decoder = ClientMessageDecoder::new("client", None).unwrap();
        let messages = [
            decoder.push_messages(&wire[..split]).unwrap(),
            decoder.push_messages(&wire[split..]).unwrap(),
        ]
        .concat();
        decoder.end().unwrap();
        assert_eq!(messages, vec![client_hello(1.0), request.clone()]);
    }
}

#[test]
fn incrementally_decodes_server_messages() {
    let error_message = ServerMessage::HelloError {
        error: ProtocolError {
            code: ProtocolErrorCode::Version,
            message: "Unsupported protocol version".to_string(),
            details: None,
        },
    };
    let mut decoder = ServerMessageDecoder::new("server", None).unwrap();
    assert_eq!(
        decoder.push_messages(&encode_server_message(&error_message, None).unwrap()).unwrap(),
        vec![error_message]
    );
    decoder.end().unwrap();
}

#[test]
fn rejects_invalid_framed_client_input() {
    let wires: Vec<Vec<u8>> = vec![
        encode_frame(&[]).unwrap(), // empty CBOR payload
        encode_frame(&[0xff]).unwrap(), // malformed CBOR
        encode_frame(
            &encode_cbor(
                &map(&[
                    ("type", str("hello")),
                    ("version", num(1.0)),
                    ("extra", boo(true)),
                ]),
                &CborOptions::default(),
            )
            .unwrap(),
        )
        .unwrap(), // schema-invalid CBOR
    ];
    for wire in wires {
        let mut decoder = ClientMessageDecoder::new("client", None).unwrap();
        assert!(decoder.push_messages(&wire).is_err());
        let error = decoder.push_messages(&encode_client_message(&client_hello(1.0), None).unwrap()).unwrap_err();
        assert!(error.0.to_lowercase().contains("failed"), "{error:?}");
    }
}

#[test]
fn rejects_cbor_byte_strings_nested_in_json_valued_fields() {
    let wire = encode_frame(
        &encode_cbor(
            &map(&[
                ("type", str("response")),
                ("id", str("request-1")),
                ("ok", boo(false)),
                (
                    "error",
                    map(&[
                        ("code", str("invalid_request")),
                        ("message", str("invalid")),
                        ("details", map(&[("nested", Value::Bytes(vec![1, 2, 3]))])),
                    ]),
                ),
            ]),
            &CborOptions::default(),
        )
        .unwrap(),
    )
    .unwrap();
    let mut decoder = ServerMessageDecoder::new("server", None).unwrap();
    assert!(decoder.push_messages(&wire).is_err());
}

#[test]
fn rejects_truncated_and_oversized_framing_through_the_validated_decoder() {
    let mut truncated = ServerMessageDecoder::new("server", None).unwrap();
    assert_eq!(truncated.push_messages(&[0, 0, 0, 2, 1]).unwrap(), vec![]);
    assert!(truncated.end().is_err());

    let mut oversized = ClientMessageDecoder::new("client", Some(3)).unwrap();
    assert!(oversized.push_messages(&[0, 0, 0, 4]).is_err());
}

// ---------------------------------------------------------------------------
// Full-model round trips (schema coverage beyond the ported JS tests)
// ---------------------------------------------------------------------------

fn model_metadata() -> ModelMetadata {
    ModelMetadata {
        provider: "test".to_string(),
        id: "model-1".to_string(),
        name: "Test Model".to_string(),
        api: "test".to_string(),
        reasoning: true,
        input: vec![pi_protocol::schemas::InputKind::Text],
        context_window: 128_000.0,
        max_tokens: 16_384.0,
        cost: ModelCost {
            input: 0.5,
            output: 1.5,
            cache_read: 0.1,
            cache_write: 0.2,
        },
        supported_thinking_levels: vec!["low".to_string(), "high".to_string()],
        authenticated: true,
    }
}

fn full_snapshot() -> SessionSnapshot {
    SessionSnapshot {
        id: "session-1".to_string(),
        name: Some("Named session".to_string()),
        cwd: "/workspace".to_string(),
        created_at: 1.0,
        updated_at: 2.0,
        phase: "turn".to_string(),
        model: ModelRef {
            provider: "test".to_string(),
            id: "model-1".to_string(),
        },
        thinking_level: "high".to_string(),
        attached: true,
        locked: false,
        revision: 3.0,
        transcript: vec![
            pi_protocol::schemas::TranscriptItem::User(pi_protocol::schemas::UserItem {
                id: "u-1".to_string(),
                content: vec![
                    Content::Text {
                        text: "hello".to_string(),
                    },
                    Content::Image {
                        data: "abc".to_string(),
                        mime_type: "image/png".to_string(),
                    },
                ],
                timestamp: 1.0,
            }),
            pi_protocol::schemas::TranscriptItem::Assistant(AssistantItem {
                id: "a-1".to_string(),
                content: vec![
                    Content::Thinking {
                        thinking: "hmm".to_string(),
                        redacted: Some(true),
                    },
                    Content::ToolCall {
                        tool_call_id: "call-1".to_string(),
                        tool_name: "read".to_string(),
                        input: map(&[("path", str("/tmp/file"))]),
                    },
                ],
                model: ModelRef {
                    provider: "test".to_string(),
                    id: "model-1".to_string(),
                },
                response_model: Some("response-model".to_string()),
                usage: Some(usage()),
                timestamp: 2.0,
                status: AssistantStatus::Complete {
                    stop_reason: "toolUse".to_string(),
                },
            }),
            pi_protocol::schemas::TranscriptItem::Tool(ToolItem {
                id: "t-1".to_string(),
                tool_call_id: "call-1".to_string(),
                tool_name: "read".to_string(),
                input: map(&[("path", str("/tmp/file"))]),
                content: vec![Content::Text {
                    text: "done".to_string(),
                }],
                details: Some(map(&[("cached", boo(false))])),
                usage: Some(usage()),
                timestamp: 3.0,
                status: ToolStatus::Error,
            }),
        ],
        queued_steer: vec![],
        queued_steer_count: 0.0,
    }
}

#[test]
fn rejects_schema_boundary_violations_js_also_rejects() {
    // ModelMetadata contextWindow/maxTokens have minimum 1 in JS.
    let bad_model = map(&[
        ("provider", str("p")),
        ("id", str("m")),
        ("name", str("n")),
        ("api", str("a")),
        ("reasoning", boo(false)),
        ("input", arr(&[str("text")])),
        ("contextWindow", num(0.0)),
        ("maxTokens", num(1.0)),
        ("cost", map(&[("input", num(0.0)), ("output", num(0.0)), ("cacheRead", num(0.0)), ("cacheWrite", num(0.0))])),
        ("supportedThinkingLevels", arr(&[str("low")])),
        ("authenticated", boo(false)),
    ]);
    assert!(parse_server_message(&bad_model).is_err(), "contextWindow 0 must be rejected");

    // ResponseEnvelope ok:true must not carry an error key.
    let cross_ok = map(&[
        ("type", str("response")),
        ("id", str("r")),
        ("ok", boo(true)),
        ("result", map(&[("command", str("list")), ("sessions", arr(&[]))])),
        ("error", map(&[("code", str("busy")), ("message", str("x"))])),
    ]);
    assert!(parse_server_message(&cross_ok).is_err(), "ok:true with error key must be rejected");

    // ResponseEnvelope ok:false must not carry a result key.
    let cross_err = map(&[
        ("type", str("response")),
        ("id", str("r")),
        ("ok", boo(false)),
        ("error", map(&[("code", str("busy")), ("message", str("x"))])),
        ("result", map(&[("command", str("list")), ("sessions", arr(&[]))])),
    ]);
    assert!(parse_server_message(&cross_err).is_err(), "ok:false with result key must be rejected");

    // queuedSteer items must have role "user".
    let bad_steer = map(&[
        ("id", str("s")),
        ("cwd", str("/tmp")),
        ("createdAt", num(1.0)),
        ("updatedAt", num(1.0)),
        ("phase", str("idle")),
        ("model", map(&[("provider", str("p")), ("id", str("m"))])),
        ("thinkingLevel", str("low")),
        ("attached", boo(false)),
        ("locked", boo(false)),
        ("revision", num(0.0)),
        ("transcript", arr(&[])),
        ("queuedSteer", arr(&[map(&[("id", str("x")), ("role", str("assistant")), ("content", arr(&[])), ("timestamp", num(1.0))])])),
        ("queuedSteerCount", num(1.0)),
    ]);
    assert!(parse_server_message(&bad_steer).is_err(), "queuedSteer role must be user");
}

#[test]
fn round_trips_every_command_and_result_variant() {
    let commands = vec![
        Command::List,
        Command::Create {
            cwd: Some("/tmp".to_string()),
            name: Some("n".to_string()),
            model: Some(ModelRef {
                provider: "p".to_string(),
                id: "m".to_string(),
            }),
            thinking_level: Some("high".to_string()),
        },
        Command::Attach {
            session_id: "s".to_string(),
        },
        Command::Detach {
            session_id: "s".to_string(),
        },
        Command::Prompt {
            session_id: "s".to_string(),
            text: "t".to_string(),
        },
        Command::Steer {
            session_id: "s".to_string(),
            text: "t".to_string(),
        },
        Command::Abort {
            session_id: "s".to_string(),
        },
        Command::SetModel {
            session_id: "s".to_string(),
            model: ModelRef {
                provider: "p".to_string(),
                id: "m".to_string(),
            },
        },
        Command::SetThinking {
            session_id: "s".to_string(),
            thinking_level: "max".to_string(),
        },
    ];
    for command in commands {
        let message = ClientMessage::Request {
            id: "r".to_string(),
            request: command,
        };
        let parsed = parse_client_message(&message.to_value()).unwrap();
        assert_eq!(parsed, message);
    }

    let snapshot = full_snapshot();
    let results = vec![
        CommandResult::List {
            sessions: vec![SessionMetadata {
                id: "s".to_string(),
                created_at: 1.0,
                updated_at: Some(2.0),
                parent_session_id: Some("p".to_string()),
                session_name: Some("n".to_string()),
                cwd: Some("/tmp".to_string()),
            }],
        },
        CommandResult::Create {
            session: snapshot.clone(),
        },
        CommandResult::Attach {
            session: snapshot.clone(),
        },
        CommandResult::Prompt {
            session: snapshot.clone(),
        },
        CommandResult::Steer {
            session: snapshot.clone(),
        },
        CommandResult::Abort {
            session: snapshot.clone(),
        },
        CommandResult::SetModel {
            session: snapshot.clone(),
        },
        CommandResult::SetThinking {
            session: snapshot.clone(),
        },
        CommandResult::Detach {
            session_id: "s".to_string(),
        },
    ];
    for result in results {
        let message = ServerMessage::Response(ResponseEnvelope::Ok {
            id: "r".to_string(),
            result,
        });
        let parsed = parse_server_message(&message.to_value()).unwrap();
        assert_eq!(parsed, message);
    }
}

#[test]
fn round_trips_every_progress_variant_and_event() {
    let snapshot = full_snapshot();
    let events = vec![
        ServerMessage::Event(pi_protocol::schemas::EventEnvelope {
            event: pi_protocol::schemas::ServerEvent::ServerSnapshot {
                snapshot: ServerSnapshot {
                    server_id: "server-1".to_string(),
                    protocol_version: PROTOCOL_VERSION,
                    revision: 1.0,
                    sessions: vec![SessionMetadata {
                        id: "s".to_string(),
                        created_at: 1.0,
                        updated_at: None,
                        parent_session_id: None,
                        session_name: None,
                        cwd: None,
                    }],
                    models: vec![model_metadata()],
                },
            },
        }),
        ServerMessage::Event(pi_protocol::schemas::EventEnvelope {
            event: pi_protocol::schemas::ServerEvent::SessionSnapshot { snapshot: snapshot.clone() },
        }),
        ServerMessage::Event(pi_protocol::schemas::EventEnvelope {
            event: pi_protocol::schemas::ServerEvent::SessionProgress {
                session_id: "session-1".to_string(),
                progress: pi_protocol::schemas::TranscriptProgress::ItemStarted {
                    item: pi_protocol::schemas::TranscriptItem::User(pi_protocol::schemas::UserItem {
                        id: "u-1".to_string(),
                        content: vec![Content::Text {
                            text: "hi".to_string(),
                        }],
                        timestamp: 1.0,
                    }),
                },
            },
        }),
        ServerMessage::Event(pi_protocol::schemas::EventEnvelope {
            event: pi_protocol::schemas::ServerEvent::SessionProgress {
                session_id: "session-1".to_string(),
                progress: pi_protocol::schemas::TranscriptProgress::AssistantDelta {
                    message_id: "a-1".to_string(),
                    content_index: 0.0,
                    kind: "text".to_string(),
                    delta: "hel".to_string(),
                },
            },
        }),
        ServerMessage::Event(pi_protocol::schemas::EventEnvelope {
            event: pi_protocol::schemas::ServerEvent::SessionProgress {
                session_id: "session-1".to_string(),
                progress: pi_protocol::schemas::TranscriptProgress::ItemUpdated {
                    item: pi_protocol::schemas::AssistantOrTool::Assistant(AssistantItem {
                        id: "a-1".to_string(),
                        content: vec![Content::Thinking {
                            thinking: "x".to_string(),
                            redacted: None,
                        }],
                        model: ModelRef {
                            provider: "test".to_string(),
                            id: "model-1".to_string(),
                        },
                        response_model: None,
                        usage: None,
                        timestamp: 2.0,
                        status: AssistantStatus::Streaming,
                    }),
                },
            },
        }),
        ServerMessage::Event(pi_protocol::schemas::EventEnvelope {
            event: pi_protocol::schemas::ServerEvent::SessionProgress {
                session_id: "session-1".to_string(),
                progress: pi_protocol::schemas::TranscriptProgress::ItemFinished {
                    item: pi_protocol::schemas::FinishedItem::ToolComplete(ToolItem {
                        id: "t-1".to_string(),
                        tool_call_id: "call-1".to_string(),
                        tool_name: "read".to_string(),
                        input: null(),
                        content: vec![],
                        details: None,
                        usage: None,
                        timestamp: 3.0,
                        status: ToolStatus::Complete,
                    }),
                },
            },
        }),
        ServerMessage::Event(pi_protocol::schemas::EventEnvelope {
            event: pi_protocol::schemas::ServerEvent::SessionRemoved {
                session_id: "session-1".to_string(),
            },
        }),
    ];
    for event in events {
        let parsed = parse_server_message(&event.to_value()).unwrap();
        assert_eq!(parsed, event);
    }
}
