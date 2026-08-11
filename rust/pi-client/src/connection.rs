//! Client connection state machine, port of
//! `packages/client/src/connection.ts`.
//!
//! Synchronous analog: `connect` blocks until the server hello arrives or
//! the transport fails; after connect a background reader thread decodes
//! frames and dispatches messages through the configured callbacks.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use pi_protocol::codec::{encode_client_message, ProtocolValidationError, ValidatedMessageDecoder};
use pi_protocol::schemas::{ClientMessage, PROTOCOL_VERSION, ServerMessage, ServerSnapshot};

use crate::errors::{to_disconnected_error, ClientError, PiDisconnectedError, PiServerError};

pub const DEFAULT_MAX_FRAME_LENGTH: u64 = 1 << 24;
const MAX_UINT32: u64 = 0xffff_ffff;

/// Byte transport: sends one byte chunk; deliveries keep invocation order.
pub trait ByteTransport: Send + Sync {
    fn send(&self, chunk: &[u8]) -> Result<(), ClientError>;
    fn close(&self);
}

/// Inbound byte-chunk and terminal-state handlers.
pub trait ByteTransportHandlers: Send + Sync {
    fn on_data(&self, chunk: &[u8]);
    fn on_close(&self);
    fn on_error(&self, error: ClientError);
}

/// Creates a fresh connected, authenticated transport.
pub trait ByteTransportFactory: Send + Sync {
    fn connect_transport(
        &self,
        handlers: Arc<dyn ByteTransportHandlers>,
    ) -> Result<Arc<dyn ByteTransport>, ClientError>;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
}

impl ConnectionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConnectionState::Disconnected => "disconnected",
            ConnectionState::Connecting => "connecting",
            ConnectionState::Connected => "connected",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConnectionStateChange {
    pub state: ConnectionState,
    pub error: Option<ClientError>,
}

/// Connection callbacks (JS ConnectionOptions).
pub struct ConnectionOptions {
    pub transport_factory: Arc<dyn ByteTransportFactory>,
    pub max_frame_length: Option<u64>,
    pub on_handshake: Arc<dyn Fn(&ServerSnapshot) + Send + Sync>,
    pub on_message: Arc<dyn Fn(&ServerMessage) + Send + Sync>,
    pub on_state_change: Arc<dyn Fn(&ConnectionStateChange) + Send + Sync>,
}

/// Builder-style constructor with default no-op callbacks.
impl ConnectionOptions {
    pub fn for_client(transport_factory: Arc<dyn ByteTransportFactory>) -> Self {
        Self {
            transport_factory,
            max_frame_length: None,
            on_handshake: Arc::new(|_| {}),
            on_message: Arc::new(|_| {}),
            on_state_change: Arc::new(|_| {}),
        }
    }
}

struct Lifecycle {
    state: ConnectionState,
    id: u64,
    decoder: Option<ValidatedMessageDecoder<ServerMessage>>,
    transport: Option<Arc<dyn ByteTransport>>,
    handshake_result: Option<Arc<Mutex<Option<Result<ServerSnapshot, ClientError>>>>>,
    handshake_condvar: Option<Arc<Condvar>>,
}

pub struct Connection {
    options: Mutex<Arc<ConnectionOptions>>,
    max_frame_length: u64,
    lifecycle: Mutex<Lifecycle>,
    sequence: AtomicU64,
}

impl Connection {
    /// Swap the runtime callbacks (used to wire client-owned handlers after
    /// construction, avoiding Arc cycles).
    pub fn set_options(&self, options: ConnectionOptions) {
        *self.options.lock().unwrap() = Arc::new(options);
    }

    pub fn transport_factory(&self) -> Arc<dyn ByteTransportFactory> {
        self.options.lock().unwrap().transport_factory.clone()
    }
}

impl Connection {
    pub fn new(options: ConnectionOptions) -> Result<Self, String> {
        let max_frame_length = options.max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
        if max_frame_length < 1 || max_frame_length > MAX_UINT32 {
            return Err(format!(
                "PiClient maxFrameLength must be an integer between 1 and {MAX_UINT32}"
            ));
        }
        Ok(Self {
            options: Mutex::new(Arc::new(options)),
            max_frame_length,
            lifecycle: Mutex::new(Lifecycle {
                state: ConnectionState::Disconnected,
                id: 0,
                decoder: None,
                transport: None,
                handshake_result: None,
                handshake_condvar: None,
            }),
            sequence: AtomicU64::new(0),
        })
    }

    pub fn state(&self) -> ConnectionState {
        self.lifecycle.lock().unwrap().state
    }

    pub fn max_frame_length(&self) -> u64 {
        self.max_frame_length
    }

    /// Block until the handshake completes (server hello) or fails. Called
    /// on the shared Arc so the reader thread owns the same instance.
    pub fn connect(self: &Arc<Self>) -> Result<ServerSnapshot, ClientError> {
        let handshake_result = Arc::new(Mutex::new(None::<Result<ServerSnapshot, ClientError>>));
        let handshake_condvar = Arc::new(Condvar::new());
        {
            let mut lifecycle = self.lifecycle.lock().unwrap();
            if lifecycle.state != ConnectionState::Disconnected {
                return Err(to_disconnected_error(&format!(
                    "PiClient is already {}",
                    lifecycle.state.as_str()
                )));
            }
            let id = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
            *lifecycle = Lifecycle {
                state: ConnectionState::Connecting,
                id,
                decoder: match ValidatedMessageDecoder::new("server", Some(self.max_frame_length)) {
                    Ok(decoder) => Some(decoder),
                    Err(_) => None,
                },
                transport: None,
                handshake_result: Some(handshake_result.clone()),
                handshake_condvar: Some(handshake_condvar.clone()),
            };
        }
        (self.options.lock().unwrap().on_state_change)(&ConnectionStateChange {
            state: ConnectionState::Connecting,
            error: None,
        });

        let connection = self.clone();
        let id = self.lifecycle.lock().unwrap().id;
        std::thread::spawn(move || {
            connection.open_transport(id);
        });

        // Block until handshake resolves.
        let mut slot = handshake_result.lock().unwrap();
        while slot.is_none() {
            slot = handshake_condvar.wait(slot).unwrap();
        }
        slot.take().unwrap()
    }

    pub fn disconnect(&self, reason: &str) {
        if self.lifecycle.lock().unwrap().state == ConnectionState::Disconnected {
            return;
        }
        self.fail_and_close(to_disconnected_error(reason));
    }

    pub fn fail(&self, error: ClientError) {
        self.fail_and_close(error);
    }

    pub fn send(&self, frame: &[u8]) -> Result<(), ClientError> {
        let transport = {
            let lifecycle = self.lifecycle.lock().unwrap();
            if lifecycle.state != ConnectionState::Connected {
                return Err(ClientError::Disconnected(PiDisconnectedError::default()));
            }
            lifecycle.transport.clone().unwrap()
        };
        match transport.send(frame) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.fail_and_close(error.clone());
                Err(error)
            }
        }
    }

    fn open_transport(self: &Arc<Self>, id: u64) {
        let handlers: Arc<dyn ByteTransportHandlers> = Arc::new(TransportHandlers {
            connection: self.clone(),
            id,
        });
        let transport = match self.options.lock().unwrap().transport_factory.connect_transport(handlers) {
            Ok(transport) => transport,
            Err(error) => {
                self.fail_if_current(id, error);
                return;
            }
        };
        {
            let mut lifecycle = self.lifecycle.lock().unwrap();
            if lifecycle.state != ConnectionState::Connecting || lifecycle.id != id {
                transport.close();
                return;
            }
            lifecycle.transport = Some(transport.clone());
        }
        let hello = match encode_client_message(
            &ClientMessage::Hello {
                version: PROTOCOL_VERSION,
            },
            Some(self.max_frame_length),
        ) {
            Ok(frame) => frame,
            Err(error) => {
                self.fail_and_close_if_current(id, ClientError::Protocol(error));
                return;
            }
        };
        if let Err(error) = transport.send(&hello) {
            self.fail_and_close_if_current(id, to_disconnected_error(&format!("{error}")));
        }
    }

    fn handle_data(&self, id: u64, chunk: &[u8]) {

        {
            let lifecycle = self.lifecycle.lock().unwrap();
            if lifecycle.state == ConnectionState::Disconnected || lifecycle.id != id {
                return;
            }
            if lifecycle.state == ConnectionState::Connecting && lifecycle.transport.is_none() {
                drop(lifecycle);
                self.fail_and_close(ClientError::Protocol(ProtocolValidationError(
                    "Received server data before the client hello was sent".to_string(),
                )));
                return;
            }
        }
        let messages = {
            let mut lifecycle = self.lifecycle.lock().unwrap();
            match lifecycle
                .decoder
                .as_mut()
                .unwrap()
                .push(chunk, pi_protocol::codec::parse_server_message)
            {
                Ok(messages) => messages,
                Err(error) => {
                    drop(lifecycle);
                    self.fail_and_close(ClientError::Protocol(error));
                    return;
                }
            }
        };
        for message in messages {
            let state = self.lifecycle.lock().unwrap().state;
            if state == ConnectionState::Disconnected {
                return;
            }
            self.handle_message(&message);
        }
    }

    fn handle_message(&self, message: &ServerMessage) {
        let lifecycle = self.lifecycle.lock().unwrap();
        if lifecycle.state == ConnectionState::Connecting {
            match message {
                ServerMessage::HelloError { error } => {
                    drop(lifecycle);
                    self.fail_and_close(ClientError::Server(PiServerError::new(error)));
                    return;
                }
                ServerMessage::Hello { snapshot, .. } => {
                    if lifecycle.transport.is_none() {
                        drop(lifecycle);
                        self.fail_and_close(ClientError::Protocol(ProtocolValidationError(
                            "Received server hello before the client hello was sent".to_string(),
                        )));
                        return;
                    }
                    let handshake_result = lifecycle.handshake_result.clone();
                    let handshake_condvar = lifecycle.handshake_condvar.clone();
                    let snapshot = snapshot.clone();
                    // Release the lifecycle lock before re-acquiring it:
                    // std Mutex is not reentrant on the same thread.
                    drop(lifecycle);
                    {
                        let mut lifecycle = self.lifecycle.lock().unwrap();
                        lifecycle.state = ConnectionState::Connected;
                    }
                    (self.options.lock().unwrap().on_handshake)(&snapshot);
                    (self.options.lock().unwrap().on_state_change)(&ConnectionStateChange {
                        state: ConnectionState::Connected,
                        error: None,
                    });
                    if let Some(handshake_result) = handshake_result {
                        *handshake_result.lock().unwrap() = Some(Ok(snapshot));
                        if let Some(condvar) = handshake_condvar {
                            condvar.notify_all();
                        }
                    }
                    return;
                }
                _ => {
                    drop(lifecycle);
                    self.fail_and_close(ClientError::Protocol(ProtocolValidationError(
                        "Expected server hello as first message".to_string(),
                    )));
                    return;
                }
            }
        }
        if lifecycle.state != ConnectionState::Connected {
            return;
        }
        match message {
            ServerMessage::Hello { .. } | ServerMessage::HelloError { .. } => {
                drop(lifecycle);
                self.fail_and_close(ClientError::Protocol(ProtocolValidationError(
                    "Unexpected handshake message".to_string(),
                )));
                return;
            }
            _ => {}
        }
        drop(lifecycle);
        (self.options.lock().unwrap().on_message)(message);
    }

    fn handle_close(&self) {
        let lifecycle = self.lifecycle.lock().unwrap();
        if lifecycle.state == ConnectionState::Disconnected {
            return;
        }
        drop(lifecycle);
        self.fail_inner(to_disconnected_error("Byte transport closed"));
    }

    fn fail_and_close(&self, error: ClientError) {
        let transport = {
            let lifecycle = self.lifecycle.lock().unwrap();
            if lifecycle.state == ConnectionState::Disconnected {
                None
            } else {
                lifecycle.transport.clone()
            }
        };
        self.fail_inner(error);
        if let Some(transport) = transport {
            transport.close();
        }
    }

    fn fail_inner(&self, error: ClientError) {
        let (handshake_result, handshake_condvar) = {
            let mut lifecycle = self.lifecycle.lock().unwrap();
            if lifecycle.state == ConnectionState::Disconnected {
                return;
            }
            let handshake_result = lifecycle.handshake_result.take();
            let handshake_condvar = lifecycle.handshake_condvar.take();
            lifecycle.state = ConnectionState::Disconnected;
            lifecycle.id = 0;
            lifecycle.decoder = None;
            lifecycle.transport = None;
            (handshake_result, handshake_condvar)
        };
        if let Some(handshake_result) = handshake_result {
            *handshake_result.lock().unwrap() = Some(Err(error.clone()));
            if let Some(condvar) = handshake_condvar {
                condvar.notify_all();
            }
        }
        (self.options.lock().unwrap().on_state_change)(&ConnectionStateChange {
            state: ConnectionState::Disconnected,
            error: Some(error),
        });
    }

    fn is_current(&self, id: u64) -> bool {
        let lifecycle = self.lifecycle.lock().unwrap();
        lifecycle.state != ConnectionState::Disconnected && lifecycle.id == id
    }

    fn fail_if_current(&self, id: u64, error: ClientError) {
        if self.is_current(id) {
            self.fail(error);
        }
    }

    fn fail_and_close_if_current(&self, id: u64, error: ClientError) {
        if self.is_current(id) {
            self.fail_and_close(error);
        }
    }
}

struct TransportHandlers {
    connection: Arc<Connection>,
    id: u64,
}

impl ByteTransportHandlers for TransportHandlers {
    fn on_data(&self, chunk: &[u8]) {
        self.connection.handle_data(self.id, chunk);
    }
    fn on_close(&self) {
        self.connection.handle_close();
    }
    fn on_error(&self, error: ClientError) {
        if self.connection.is_current(self.id) {
            self.connection.fail_and_close(error);
        }
    }
}

