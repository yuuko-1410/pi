//! Pi server, port of `packages/server/src/server.ts`.
//!
//! Synchronous analog: listeners run accept loops on their own threads;
//! each connection's reader thread processes frames inline. Requests block
//! until the session runtime returns.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pi_protocol::codec::{
    encode_server_message, is_supported_protocol_version, parse_client_message, ValidatedMessageDecoder,
};
use pi_protocol::schemas::{
    ClientMessage, EventEnvelope, PROTOCOL_VERSION, ProtocolError, ResponseEnvelope, ServerEvent, ServerMessage,
};

use crate::errors::INTERNAL_SERVER_ERROR_MESSAGE;
use crate::sessions::{
    ByteConnection, ConnectionState, LiveSessionManager, LiveSessionManagerOptions, PiServerService,
    SessionConnection,
};
use crate::snapshots::{ServerSnapshotPublisher, ServerSnapshotPublisherOptions};

pub const DEFAULT_MAX_FRAME_LENGTH: u64 = 1 << 24;
const DEFAULT_HANDSHAKE_TIMEOUT_MS: u64 = 5_000;
const MAX_UINT32: u64 = 0xffff_ffff;
const MAX_TIMER_DELAY_MS: u64 = 2_147_483_647;

/// Handler for one accepted connection.
pub trait ByteConnectionHandler: Send + Sync {
    fn on_data(&self, chunk: &[u8]);
    fn on_close(&self);
    fn on_error(&self, error: String);
}

/// A server listener: starts accepting connections and reports them through
/// the acceptor callback.
pub trait PiServerListener: Send + Sync {
    fn start(
        &self,
        accept: Arc<dyn Fn(Arc<dyn ByteConnection>) -> Arc<dyn ByteConnectionHandler> + Send + Sync>,
    ) -> Result<(), String>;
    fn close(&self) -> Result<(), String>;
    fn address(&self) -> Option<String>;
}

pub struct PiServerOptions {
    pub listeners: Vec<Arc<dyn PiServerListener>>,
    pub max_frame_length: Option<u64>,
    pub handshake_timeout_ms: Option<u64>,
    pub server_id: Option<String>,
    pub on_error: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

pub struct PiServer {
    pub id: String,
    listeners: Vec<Arc<dyn PiServerListener>>,
    max_frame_length: u64,
    handshake_timeout_ms: u64,
    connections: Arc<Mutex<Vec<ConnectionState>>>,
    sessions: Arc<LiveSessionManager>,
    snapshots: Arc<ServerSnapshotPublisher>,
    closing: AtomicBool,
    started: AtomicBool,
    on_error: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

impl PiServer {
    pub fn new(service: Arc<dyn PiServerService>, options: PiServerOptions) -> Result<Arc<Self>, String> {
        let (max_frame_length, handshake_timeout_ms) = resolve_options(&options)?;
        let id = options.server_id.clone().unwrap_or_else(uuid_v7);
        let connections = Arc::new(Mutex::new(Vec::<ConnectionState>::new()));
        let on_error = options.on_error.clone();

        // LiveSessionManager with placeholder callbacks; rewired below.
        let sessions = Arc::new(LiveSessionManager::new(LiveSessionManagerOptions {
            service: service.clone(),
            is_closing: Arc::new(|| false),
            send_message: Arc::new(|_, _| true),
            close_connection: Arc::new(|_| {}),
            disconnect_connection: Arc::new(|_| {}),
            broadcast_server_snapshot: Arc::new(|| {}),
            report_error: Arc::new(|_| {}),
        }));

        // Snapshot publisher with placeholder callbacks; rewired below.
        let snapshots = Arc::new(ServerSnapshotPublisher::new(ServerSnapshotPublisherOptions {
            server_id: id.clone(),
            service: service.clone(),
            connections: connections.clone(),
            is_closing: Arc::new(|| false),
            list_sessions: Arc::new(|| vec![]),
            send_message: Arc::new(|_, _| true),
            report_error: Arc::new(|_| {}),
        }));

        let server = Arc::new(Self {
            id,
            listeners: options.listeners.clone(),
            max_frame_length,
            handshake_timeout_ms,
            connections,
            sessions,
            snapshots,
            closing: AtomicBool::new(false),
            started: AtomicBool::new(false),
            on_error,
        });

        // Wire real callbacks.
        server.sessions.set_options(LiveSessionManagerOptions {
            service: server.sessions.options().service.clone(),
            is_closing: {
                let server = server.clone();
                Arc::new(move || server.closing.load(Ordering::SeqCst))
            },
            send_message: {
                let server = server.clone();
                Arc::new(move |connection, message| server.send_event(connection, message))
            },
            close_connection: {
                let server = server.clone();
                Arc::new(move |connection| server.close_connection(connection))
            },
            disconnect_connection: {
                let server = server.clone();
                Arc::new(move |connection| server.disconnect(connection))
            },
            broadcast_server_snapshot: {
                let server = server.clone();
                Arc::new(move || {
                    server.snapshots.broadcast();
                })
            },
            report_error: {
                let server = server.clone();
                Arc::new(move |error| server.report_error(&format!("{error:?}")))
            },
        });
        server.snapshots.set_options(ServerSnapshotPublisherOptions {
            server_id: server.snapshots.options().server_id.clone(),
            service: server.snapshots.options().service.clone(),
            connections: server.snapshots.options().connections.clone(),
            is_closing: {
                let server = server.clone();
                Arc::new(move || server.closing.load(Ordering::SeqCst))
            },
            list_sessions: {
                let server = server.clone();
                Arc::new(move || server.sessions.list_metadata())
            },
            send_message: {
                let server = server.clone();
                Arc::new(move |connection, message| server.send_event(connection, message))
            },
            report_error: {
                let server = server.clone();
                Arc::new(move |error| server.report_error(&format!("{error:?}")))
            },
        });

        Ok(server)
    }

    pub fn addresses(&self) -> Vec<String> {
        self.listeners
            .iter()
            .filter_map(|listener| listener.address())
            .collect()
    }

    pub fn start(self: &Arc<Self>) -> Result<(), String> {
        if self.started.load(Ordering::SeqCst) {
            return Err("PiServer is already started".to_string());
        }
        if self.closing.load(Ordering::SeqCst) {
            return Err("PiServer is closing or closed".to_string());
        }
        for listener in &self.listeners {
            let server = self.clone();
            listener.start(Arc::new(move |connection| server.accept(connection)))?;
        }
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub fn accept(
        self: &Arc<Self>,
        connection: Arc<dyn ByteConnection>,
    ) -> Arc<dyn ByteConnectionHandler> {
        if self.closing.load(Ordering::SeqCst) {
            connection.close(None);
            return Arc::new(ClosedHandler);
        }
        let decoder = match ValidatedMessageDecoder::new("client", Some(self.max_frame_length)) {
            Ok(decoder) => Some(Arc::new(Mutex::new(Some(decoder)))),
            Err(_) => None,
        };
        let state: ConnectionState = Arc::new(SessionConnection {
            id: uuid_v7(),
            disconnected: std::sync::atomic::AtomicBool::new(false),
            stage: Mutex::new("awaitingHello".to_string()),
            closed: std::sync::atomic::AtomicBool::new(false),
            session_ids: Arc::new(Mutex::new(Vec::new())),
            decoder,
            transport: Some(connection.clone()),
            handshake_complete: std::sync::atomic::AtomicBool::new(false),
        });
        self.connections.lock().unwrap().push(state.clone());

        // Handshake timeout.
        let server = self.clone();
        let timeout_state = state.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(server.handshake_timeout_ms));
            let stage = timeout_state.stage.lock().unwrap().clone();
            if stage == "awaitingHello" || stage == "handshaking" {
                server.fail_protocol(&timeout_state, ProtocolError {
                    code: pi_protocol::schemas::ProtocolErrorCode::InvalidRequest,
                    message: "Handshake timeout".to_string(),
                    details: None,
                });
            }
        });

        Arc::new(ServerConnectionHandler {
            server: self.clone(),
            state,
            connection,
        })
    }

    pub fn close(self: &Arc<Self>) {
        self.closing.store(true, Ordering::SeqCst);
        for listener in &self.listeners {
            let _ = listener.close();
        }
        self.close_server_state();
        self.started.store(false, Ordering::SeqCst);
    }

    /// Send a server event; returns false when the connection is gone.
    pub fn send_event(&self, connection: &ConnectionState, envelope: &EventEnvelope) -> bool {
        self.send_server_message(connection, &ServerMessage::Event(EventEnvelope {
            event: envelope.event.clone(),
        }))
    }

    /// Send a server message; returns false when the connection is gone.
    pub fn send_server_message(&self, connection: &ConnectionState, message: &ServerMessage) -> bool {
        if connection.disconnected.load(Ordering::SeqCst) || connection.closed.load(Ordering::SeqCst) {
            return false;
        }
        let frame = match encode_server_message(message, Some(self.max_frame_length)) {
            Ok(frame) => frame,
            Err(error) => {
                self.report_error(&format!("{error:?}"));
                self.close_connection(connection);
                self.disconnect(connection);
                return false;
            }
        };
        let transport = connection.transport.clone().unwrap();
        match transport.send(&frame) {
            Ok(()) => true,
            Err(error) => {
                self.report_error(&error);
                self.close_connection(connection);
                self.disconnect(connection);
                false
            }
        }
    }

    fn receive(&self, state: &ConnectionState, chunk: &[u8]) {
        if state.disconnected.load(Ordering::SeqCst) || state.stage.lock().unwrap().as_str() == "closing" || state.stage.lock().unwrap().as_str() == "closed" {
            return;
        }
        let messages = {
            let decoder = state.decoder.clone().unwrap();
            let mut decoder = decoder.lock().unwrap();
            match decoder.as_mut().unwrap().push(chunk, parse_client_message) {
                Ok(messages) => messages,
                Err(error) => {
                    let protocol_error = self.to_protocol_error(error);
                    self.fail_protocol(state, protocol_error);
                    return;
                }
            }
        };
        for message in messages {
            if state.disconnected.load(Ordering::SeqCst) || state.stage.lock().unwrap().as_str() == "closing" || state.stage.lock().unwrap().as_str() == "closed" {
                return;
            }
            self.dispatch_message(state, &message);
        }
    }

    fn dispatch_message(&self, state: &ConnectionState, message: &ClientMessage) {
        eprintln!("DBG dispatch {:?} stage {}", message, state.stage.lock().unwrap());
        if state.stage.lock().unwrap().as_str() == "awaitingHello" {
            let ClientMessage::Hello { .. } = message else {
                self.fail_protocol(state, ProtocolError {
                    code: pi_protocol::schemas::ProtocolErrorCode::InvalidRequest,
                    message: "The first client message must be hello".to_string(),
                    details: None,
                });
                return;
            };
            *state.stage.lock().unwrap() = "handshaking".to_string();
            self.finish_handshake(state, message);
            return;
        }
        if matches!(message, ClientMessage::Hello { .. }) {
            self.fail_protocol(state, ProtocolError {
                code: pi_protocol::schemas::ProtocolErrorCode::InvalidRequest,
                message: "hello may only be sent as the first message".to_string(),
                details: None,
            });
            return;
        }
        if state.stage.lock().unwrap().as_str() == "ready" {
            self.handle_request(state, message);
        }
    }

    fn finish_handshake(&self, state: &ConnectionState, hello: &ClientMessage) {
        let version = match hello {
            ClientMessage::Hello { version } => *version,
            _ => return,
        };
        if !is_supported_protocol_version(version) {
            self.fail_protocol(state, ProtocolError {
                code: pi_protocol::schemas::ProtocolErrorCode::Version,
                message: format!("Unsupported protocol version {version}; expected {PROTOCOL_VERSION}"),
                details: None,
            });
            return;
        }
        let snapshot = self.snapshots.get();
        if self.closing.load(Ordering::SeqCst)
            || state.disconnected.load(Ordering::SeqCst)
            || state.stage.lock().unwrap().as_str() != "handshaking"
            || state.closed.load(Ordering::SeqCst)
        {
            return;
        }
        let sent = self.send_server_message(state, &ServerMessage::Hello {
            connection_id: state.id.clone(),
            snapshot: snapshot.clone(),
        });
        if sent && !state.disconnected.load(Ordering::SeqCst) && state.stage.lock().unwrap().as_str() == "handshaking" {
            *state.stage.lock().unwrap() = "ready".to_string();
            state.handshake_complete.store(true, Ordering::SeqCst);
            if snapshot.revision != self.snapshots.current_revision() {
                let current = self.snapshots.get();
                let _ = self.send_server_message(state, &ServerMessage::Event(EventEnvelope {
                    event: ServerEvent::ServerSnapshot { snapshot: current },
                }));
            }
        }
    }

    fn handle_request(&self, state: &ConnectionState, message: &ClientMessage) {
        let ClientMessage::Request { id, request } = message else {
            return;
        };
        let result = self.sessions.execute_command(state, request);
        let response = match result {
            Ok(result) => ServerMessage::Response(ResponseEnvelope::Ok {
                id: id.clone(),
                result,
            }),
            Err(error) => {
                let code = match error.code.as_str() {
                    "busy" => pi_protocol::schemas::ProtocolErrorCode::Busy,
                    "session_locked" => pi_protocol::schemas::ProtocolErrorCode::SessionLocked,
                    "not_found" => pi_protocol::schemas::ProtocolErrorCode::NotFound,
                    "invalid_request" => pi_protocol::schemas::ProtocolErrorCode::InvalidRequest,
                    "not_implemented" => pi_protocol::schemas::ProtocolErrorCode::NotImplemented,
                    _ => pi_protocol::schemas::ProtocolErrorCode::InternalError,
                };
                ServerMessage::Response(ResponseEnvelope::Err {
                    id: id.clone(),
                    error: ProtocolError {
                        code,
                        message: error.message.clone(),
                        details: error.details.clone(),
                    },
                })
            }
        };
        self.send_server_message(state, &response);
    }

    fn transport_closed(&self, state: &ConnectionState) {
        if !state.disconnected.load(Ordering::SeqCst) && state.stage.lock().unwrap().as_str() != "closing" {
            // Decoder end validation is best-effort.
        }
        self.disconnect(state);
    }

    fn disconnect(&self, connection: &ConnectionState) {
        if connection.disconnected.load(Ordering::SeqCst) {
            return;
        }
        let handshake_complete = connection.handshake_complete.load(Ordering::SeqCst);
        connection.disconnected.store(true, Ordering::SeqCst);
        *connection.stage.lock().unwrap() = "closed".to_string();
        self.connections
            .lock()
            .unwrap()
            .retain(|existing| !Arc::ptr_eq(existing, connection));
        self.sessions.disconnect(connection);
        if !self.closing.load(Ordering::SeqCst) && handshake_complete {
            self.snapshots.broadcast();
        }
    }

    fn close_connection(&self, connection: &ConnectionState) {
        if let Some(transport) = &connection.transport {
            transport.close(None);
        }
    }

    fn fail_protocol(&self, connection: &ConnectionState, error: ProtocolError) {
        if connection.disconnected.load(Ordering::SeqCst) || connection.stage.lock().unwrap().as_str() == "closing" || connection.stage.lock().unwrap().as_str() == "closed" {
            return;
        }
        *connection.stage.lock().unwrap() = "closing".to_string();
        let message = ServerMessage::HelloError { error };
        let final_frame = encode_server_message(&message, Some(self.max_frame_length)).ok();
        if let Some(transport) = &connection.transport {
            transport.close(final_frame);
        }
        self.disconnect(connection);
    }

    fn close_server_state(&self) {
        let connections: Vec<ConnectionState> = self.connections.lock().unwrap().clone();
        for connection in &connections {
            *connection.stage.lock().unwrap() = "closing".to_string();
        }
        for connection in &connections {
            self.close_connection(connection);
        }
        for connection in &connections {
            self.disconnect(connection);
        }
        self.sessions.close();
        self.connections.lock().unwrap().clear();
    }

    fn to_protocol_error(&self, error: impl std::fmt::Debug) -> ProtocolError {
        let _ = error;
        ProtocolError {
            code: pi_protocol::schemas::ProtocolErrorCode::InternalError,
            message: INTERNAL_SERVER_ERROR_MESSAGE.to_string(),
            details: None,
        }
    }

    fn report_error(&self, message: &str) {
        if let Some(on_error) = &self.on_error {
            on_error(message);
        }
    }
}

struct ClosedHandler;

impl ByteConnectionHandler for ClosedHandler {
    fn on_data(&self, _chunk: &[u8]) {}
    fn on_close(&self) {}
    fn on_error(&self, _error: String) {}
}

struct ServerConnectionHandler {
    server: Arc<PiServer>,
    state: ConnectionState,
    connection: Arc<dyn ByteConnection>,
}

impl ByteConnectionHandler for ServerConnectionHandler {
    fn on_data(&self, chunk: &[u8]) {
        self.server.receive(&self.state, chunk);
    }
    fn on_close(&self) {
        self.server.transport_closed(&self.state);
    }
    fn on_error(&self, error: String) {
        self.server.report_error(&error);
        self.connection.close(None);
        self.server.disconnect(&self.state);
    }
}

fn uuid_v7() -> String {
    pi_ai::utils::uuid::uuidv7()
}

fn resolve_options(options: &PiServerOptions) -> Result<(u64, u64), String> {
    let max_frame_length = options.max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
    if max_frame_length < 1 || max_frame_length > MAX_UINT32 {
        return Err(format!("PiServer maxFrameLength must be an integer between 1 and {MAX_UINT32}"));
    }
    let handshake_timeout_ms = options.handshake_timeout_ms.unwrap_or(DEFAULT_HANDSHAKE_TIMEOUT_MS);
    if handshake_timeout_ms < 1 || handshake_timeout_ms > MAX_TIMER_DELAY_MS {
        return Err(format!(
            "PiServer handshakeTimeoutMs must be an integer between 1 and {MAX_TIMER_DELAY_MS}"
        ));
    }
    Ok((max_frame_length, handshake_timeout_ms))
}
