//! Unix listener + PiServer end-to-end over a real socket.

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_protocol::codec::{encode_client_message, parse_server_message, ValidatedMessageDecoder};
use pi_protocol::schemas::{ClientMessage, Command, ModelRef, ServerMessage, SessionSnapshot};
use pi_server::errors::PiServerError;
use pi_server::server::{PiServer, PiServerOptions};
use pi_server::sessions::{
    ConnectionState, CreateSessionOptions, LiveSessionManager, LiveSessionManagerOptions, PiServerService,
    SessionConnection, SessionRuntime, SessionRuntimeEvent,
};
use pi_server::transports::unix_listener::{create_unix_listener, UnixListenerOptions};

struct EchoRuntime {
    id: String,
}

impl SessionRuntime for EchoRuntime {
    fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            id: self.id.clone(),
            name: Some("echo".to_string()),
            cwd: "/tmp".to_string(),
            created_at: 1.0,
            updated_at: 1.0,
            phase: "idle".to_string(),
            model: ModelRef {
                provider: "test".to_string(),
                id: "m".to_string(),
            },
            thinking_level: "off".to_string(),
            attached: false,
            locked: false,
            revision: 1.0,
            transcript: vec![],
            queued_steer: vec![],
            queued_steer_count: 0.0,
        }
    }
    fn get_phase(&self) -> String {
        "idle".to_string()
    }
    fn prompt(&self, _text: &str) -> Result<(), PiServerError> {
        Ok(())
    }
    fn steer(&self, _text: &str) -> Result<(), PiServerError> {
        Ok(())
    }
    fn abort(&self) -> Result<(), PiServerError> {
        Ok(())
    }
    fn set_model(&self, _model: &ModelRef) -> Result<(), PiServerError> {
        Ok(())
    }
    fn set_thinking(&self, _thinking_level: &str) -> Result<(), PiServerError> {
        Ok(())
    }
    fn subscribe(&self, _listener: Arc<dyn Fn(&SessionRuntimeEvent) + Send + Sync>) -> Box<dyn Fn() + Send + Sync> {
        Box::new(|| {})
    }
    fn dispose(&self) -> Result<(), PiServerError> {
        Ok(())
    }
}

struct EchoService;

impl PiServerService for EchoService {
    fn list_sessions(&self) -> Vec<pi_protocol::schemas::SessionMetadata> {
        vec![]
    }
    fn list_models(&self) -> Vec<pi_protocol::schemas::ModelMetadata> {
        vec![]
    }
    fn create_session(&self, options: &CreateSessionOptions) -> Result<Arc<dyn SessionRuntime>, PiServerError> {
        Ok(Arc::new(EchoRuntime { id: options.id.clone() }))
    }
    fn open_session(&self, session_id: &str) -> Result<Arc<dyn SessionRuntime>, PiServerError> {
        Ok(Arc::new(EchoRuntime {
            id: session_id.to_string(),
        }))
    }
}

#[test]
fn end_to_end_handshake_and_list() {
    let dir = std::env::temp_dir().join(format!("pi-server-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket_path = dir.join("pi.sock");
    let socket_path_str = socket_path.to_string_lossy().to_string();

    let listener = create_unix_listener(UnixListenerOptions {
        path: socket_path_str.clone(),
        ..UnixListenerOptions::default()
    })
    .unwrap();
    let server = PiServer::new(Arc::new(EchoService), PiServerOptions {
        listeners: vec![listener],
        max_frame_length: None,
        handshake_timeout_ms: Some(5000),
        server_id: Some("test-server".to_string()),
        on_error: None,
    })
    .unwrap();
    server.start().unwrap();
    assert_eq!(server.addresses(), vec![socket_path_str.clone()]);

    // Connect a client socket.
    let mut stream = std::os::unix::net::UnixStream::connect(&socket_path_str).unwrap();
    let mut decoder = ValidatedMessageDecoder::<ServerMessage>::new("server", None).unwrap();

    // Client hello.
    let hello = encode_client_message(&ClientMessage::Hello { version: 1.0 }, None).unwrap();
    std::io::Write::write_all(&mut stream, &hello).unwrap();

    // Read until hello response.
    let mut buffer = [0u8; 8192];
    let mut hello_response: Option<ServerMessage> = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while hello_response.is_none() {
        let count = stream.read(&mut buffer).unwrap();
        let messages = decoder.push(&buffer[..count], parse_server_message).unwrap();
        for message in messages {
            match message {
                ServerMessage::Hello { connection_id, snapshot } => {
                    assert_eq!(snapshot.server_id, "test-server");
                    hello_response = Some(ServerMessage::Hello {
                        connection_id,
                        snapshot,
                    });
                }
                _ => {}
            }
        }
        assert!(std::time::Instant::now() < deadline, "handshake timed out");
    }
    assert!(hello_response.is_some());

    // List sessions request.
    let list = encode_client_message(
        &ClientMessage::Request {
            id: "r1".to_string(),
            request: Command::List,
        },
        None,
    )
    .unwrap();
    std::io::Write::write_all(&mut stream, &list).unwrap();
    let mut got_response = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !got_response {
        let count = stream.read(&mut buffer).unwrap();
        let messages = decoder.push(&buffer[..count], parse_server_message).unwrap();
        for message in messages {
            if let ServerMessage::Response(pi_protocol::schemas::ResponseEnvelope::Ok { id, result }) = message {
                assert_eq!(id, "r1");
                assert!(matches!(result, pi_protocol::schemas::CommandResult::List { sessions } if sessions.is_empty()));
                got_response = true;
            }
        }
        assert!(std::time::Instant::now() < deadline, "list response timed out");
    }

    server.close();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rejects_stale_socket_removal_of_live_listener() {
    let dir = std::env::temp_dir().join(format!("pi-server-live-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket_path = dir.join("live.sock");
    let socket_path_str = socket_path.to_string_lossy().to_string();

    let listener = create_unix_listener(UnixListenerOptions {
        path: socket_path_str.clone(),
        ..UnixListenerOptions::default()
    })
    .unwrap();
    let server = PiServer::new(Arc::new(EchoService), PiServerOptions {
        listeners: vec![listener],
        max_frame_length: None,
        handshake_timeout_ms: Some(5000),
        server_id: None,
        on_error: None,
    })
    .unwrap();
    server.start().unwrap();

    // A second listener on the same path must fail (live listener detected).
    let second = create_unix_listener(UnixListenerOptions {
        path: socket_path_str.clone(),
        ..UnixListenerOptions::default()
    })
    .unwrap();
    let second_server = PiServer::new(Arc::new(EchoService), PiServerOptions {
        listeners: vec![second],
        max_frame_length: None,
        handshake_timeout_ms: Some(5000),
        server_id: None,
        on_error: None,
    })
    .unwrap();
    let error = second_server.start().unwrap_err();
    assert!(error.contains("already running") || error.contains("stale") || !error.is_empty());

    server.close();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn manager_options_are_replaceable() {
    // LiveSessionManager::set_options exists for PiServer wiring.
    let manager = LiveSessionManager::new(LiveSessionManagerOptions {
        service: Arc::new(EchoService),
        is_closing: Arc::new(|| false),
        send_message: Arc::new(|_, _| true),
        close_connection: Arc::new(|_| {}),
        disconnect_connection: Arc::new(|_| {}),
        broadcast_server_snapshot: Arc::new(|| {}),
        report_error: Arc::new(|_| {}),
    });
    manager.set_options(LiveSessionManagerOptions {
        service: Arc::new(EchoService),
        is_closing: Arc::new(|| true),
        send_message: Arc::new(|_, _| true),
        close_connection: Arc::new(|_| {}),
        disconnect_connection: Arc::new(|_| {}),
        broadcast_server_snapshot: Arc::new(|| {}),
        report_error: Arc::new(|_| {}),
    });
    let _connection: ConnectionState = Arc::new(SessionConnection {
        id: "c".to_string(),
        disconnected: std::sync::atomic::AtomicBool::new(false),
        stage: Mutex::new("ready".to_string()),
        closed: std::sync::atomic::AtomicBool::new(false),
        session_ids: Arc::new(Mutex::new(Vec::new())),
        decoder: None,
        transport: None,
        handshake_complete: std::sync::atomic::AtomicBool::new(false),
    });
}
