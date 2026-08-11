//! PiClient integration tests over an in-memory duplex transport.

use std::sync::{Arc, Condvar, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use pi_client::client::PiClient;
use pi_client::connection::{ByteTransport, ByteTransportFactory, ByteTransportHandlers, ConnectionState};
use pi_client::errors::ClientError;
use pi_protocol::codec::{encode_server_message, parse_client_message};
use pi_protocol::schemas::{Command, CommandResult, ClientMessage, ServerMessage};

/// In-memory duplex transport: client side and server side share channels.
struct MemoryTransport {
    rx: Mutex<std::sync::mpsc::Receiver<Vec<u8>>>,
    tx: Mutex<std::sync::mpsc::Sender<Vec<u8>>>,
    closed: AtomicBool,
}

impl ByteTransport for MemoryTransport {
    fn send(&self, chunk: &[u8]) -> Result<(), ClientError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ClientError::Other("closed".to_string()));
        }
        self.tx
            .lock()
            .unwrap()
            .send(chunk.to_vec())
            .map_err(|error| ClientError::Other(format!("{error}")))?;
        Ok(())
    }
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

/// Server loop: reads client messages off the channel, answers hello and
/// requests, and emits events.
fn spawn_server(rx: std::sync::mpsc::Receiver<Vec<u8>>, tx: std::sync::mpsc::Sender<Vec<u8>>) {
    std::thread::spawn(move || {
        let mut decoder =
            pi_protocol::codec::ValidatedMessageDecoder::<ClientMessage>::new("client", None).unwrap();
        let send = |message: &ServerMessage| {
            let frame = encode_server_message(message, None).unwrap();
            let _ = tx.send(frame);
        };
        // Hello first.
        send(&ServerMessage::Hello {
            connection_id: "c1".to_string(),
            snapshot: empty_snapshot(),
        });
        for chunk in rx {
            for message in decoder.push(&chunk, parse_client_message).unwrap() {
                match message {
                    ClientMessage::Hello { .. } => {}
                    ClientMessage::Request { id, request } => {
                        let result = match request {
                            Command::List => CommandResult::List { sessions: vec![] },
                            Command::Create { .. } => {
                                CommandResult::Create { session: empty_session() }
                            }
                            Command::Attach { .. } => {
                                CommandResult::Attach { session: empty_session() }
                            }
                            Command::Detach { session_id } => CommandResult::Detach { session_id },
                            Command::Prompt { session_id, .. } => {
                                CommandResult::Prompt { session: empty_session_named(&session_id) }
                            }
                            _ => CommandResult::Detach {
                                session_id: "s".to_string(),
                            },
                        };
                        send(&ServerMessage::Response(pi_protocol::schemas::ResponseEnvelope::Ok {
                            id,
                            result,
                        }));
                    }
                }
            }
        }
    });
}

fn empty_snapshot() -> pi_protocol::schemas::ServerSnapshot {
    pi_protocol::schemas::ServerSnapshot {
        server_id: "server-1".to_string(),
        protocol_version: 1.0,
        revision: 1.0,
        sessions: vec![],
        models: vec![],
    }
}

fn empty_session() -> pi_protocol::schemas::SessionSnapshot {
    empty_session_named("s1")
}

fn empty_session_named(id: &str) -> pi_protocol::schemas::SessionSnapshot {
    pi_protocol::schemas::SessionSnapshot {
        id: id.to_string(),
        name: Some("test".to_string()),
        cwd: "/tmp".to_string(),
        created_at: 1.0,
        updated_at: 1.0,
        phase: "idle".to_string(),
        model: pi_protocol::schemas::ModelRef {
            provider: "test".to_string(),
            id: "m".to_string(),
        },
        thinking_level: "off".to_string(),
        attached: true,
        locked: false,
        revision: 1.0,
        transcript: vec![],
        queued_steer: vec![],
        queued_steer_count: 0.0,
    }
}

struct MemoryFactory {
    server_tx: Mutex<Option<std::sync::mpsc::Sender<Vec<u8>>>>,
    server_condvar: Condvar,
}

impl MemoryFactory {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            server_tx: Mutex::new(None),
            server_condvar: Condvar::new(),
        })
    }
}

impl ByteTransportFactory for MemoryFactory {
    fn connect_transport(
        &self,
        handlers: Arc<dyn ByteTransportHandlers>,
    ) -> Result<Arc<dyn ByteTransport>, ClientError> {
        let (client_tx, server_rx) = std::sync::mpsc::channel();
        let (server_tx, client_rx) = std::sync::mpsc::channel();
        {
            let mut slot = self.server_tx.lock().unwrap();
            *slot = Some(server_tx.clone());
            self.server_condvar.notify_all();
        }
        spawn_server(server_rx, server_tx);
        let transport = Arc::new(MemoryTransport {
            rx: Mutex::new(client_rx),
            tx: Mutex::new(client_tx),
            closed: AtomicBool::new(false),
        });
        let reader = transport.clone();
        std::thread::spawn(move || {
            // Read inbound bytes and deliver to the client's handlers.
            loop {
                let frame = match reader.rx.lock().unwrap().recv() {
                    Ok(frame) => frame,
                    Err(_) => return,
                };
                if reader.closed.load(Ordering::SeqCst) {
                    return;
                }
                handlers.on_data(&frame);
            }
        });
        Ok(transport)
    }
}

#[test]
fn connects_handshakes_and_lists_sessions() {
    let factory = MemoryFactory::new();
    let client = PiClient::new(factory).unwrap();
    let snapshot = client.connect().unwrap();
    assert_eq!(snapshot.protocol_version, 1.0);
    assert!(client.connected());
    assert_eq!(client.connection_state(), ConnectionState::Connected);

    let sessions = client.list_sessions().unwrap();
    assert!(sessions.is_empty());
}

#[test]
fn creates_session_with_lease() {
    let factory = MemoryFactory::new();
    let client = PiClient::new(factory).unwrap();
    client.connect().unwrap();

    let handle = client
        .create_session(&pi_client::client::CreateSessionOptions::default())
        .unwrap();
    assert_eq!(handle.session_id(), "s1");
    assert!(handle.is_attached());

    // Detach releases the lease.
    handle.detach().unwrap();
    assert!(!handle.is_attached());
}

#[test]
fn dispose_rejects_requests() {
    let factory = MemoryFactory::new();
    let client = PiClient::new(factory).unwrap();
    client.connect().unwrap();
    client.dispose();
    let error = client.list_sessions().unwrap_err();
    assert!(matches!(error, ClientError::Disposed(_)));
}
