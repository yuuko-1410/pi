//! Pi client, port of `packages/client/src/client.ts`.
//!
//! Synchronous analog of the JS PiClient: requests block until the server
//! responds; session leases and attachment tracking mirror the JS state
//! machine.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex};

use pi_protocol::schemas::{
    Command, CommandResult, EventEnvelope, ResponseEnvelope, ServerEvent, ServerMessage, ServerSnapshot,
    SessionMetadata, SessionSnapshot,
};

use crate::connection::{
    ByteTransportFactory, Connection, ConnectionOptions, ConnectionState, ConnectionStateChange,
};
use crate::errors::{ClientError, PiClientDisposedError, PiDisconnectedError, PiServerError};

pub use pi_protocol::schemas::{ModelRef, ThinkingLevel};

#[derive(Clone, Debug, Default)]
pub struct CreateSessionOptions {
    pub cwd: Option<String>,
    pub name: Option<String>,
    pub model: Option<ModelRef>,
    pub thinking_level: Option<ThinkingLevel>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionLeaseMode {
    Exclusive,
    Shared,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AcquireSessionOptions {
    pub mode: SessionLeaseMode,
}

/// Session handle: a live lease on an attached session.
pub struct PiSessionHandle {
    session_id: String,
    client: Arc<PiClient>,
    generation: u64,
}

impl PiSessionHandle {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn is_attached(&self) -> bool {
        self.client.session_lease_is_active(&self.session_id, self.generation)
    }

    pub fn request(&self, command: Command) -> Result<CommandResult, ClientError> {
        self.client.assert_lease_active(&self.session_id, self.generation)?;
        self.client.request(command)
    }

    pub fn detach(&self) -> Result<(), ClientError> {
        self.client.release_lease(&self.session_id, self.generation, false)
    }

    pub fn dispose(&self) -> Result<(), ClientError> {
        self.client.release_lease(&self.session_id, self.generation, true)
    }
}

pub type Unsubscribe = Box<dyn Fn() + Send + Sync>;

pub struct PiClient {
    connection: Arc<Connection>,
    pending_requests: Mutex<HashMap<String, PendingRequest>>,
    request_sequence: Mutex<u64>,
    session_lease_counts: Mutex<HashMap<String, usize>>,
    exclusive_session_leases: Mutex<HashSet<String>>,
    session_lease_generations: Mutex<HashMap<String, u64>>,
    session_cleanup_required: Mutex<HashSet<String>>,
    attached_session_ids: Mutex<HashSet<String>>,
    session_snapshots: Mutex<HashMap<String, SessionSnapshot>>,
    connection_state_listeners: Mutex<Vec<Arc<dyn Fn(&ConnectionStateChange) + Send + Sync>>>,
    snapshot_listeners: Mutex<Vec<Arc<dyn Fn(&ServerSnapshot) + Send + Sync>>>,
    event_listeners: Mutex<Vec<Arc<dyn Fn(&ServerEvent) + Send + Sync>>>,
    snapshot: Mutex<Option<ServerSnapshot>>,
    disposed: Mutex<bool>,
}

struct PendingRequest {
    command: Command,
    result: Arc<Mutex<Option<Result<CommandResult, ClientError>>>>,
    condvar: Arc<Condvar>,
}

impl PiClient {
    pub fn new(transport_factory: Arc<dyn ByteTransportFactory>) -> Result<Arc<Self>, String> {
        let connection = Connection::new(ConnectionOptions::for_client(transport_factory))?;
        let client = Self::from_connection(Arc::new(connection));
        client.wire_connection();
        Ok(client)
    }

    fn from_connection(connection: Arc<Connection>) -> Arc<Self> {
        Arc::new(Self {
            connection,
            pending_requests: Mutex::new(HashMap::new()),
            request_sequence: Mutex::new(0),
            session_lease_counts: Mutex::new(HashMap::new()),
            exclusive_session_leases: Mutex::new(HashSet::new()),
            session_lease_generations: Mutex::new(HashMap::new()),
            session_cleanup_required: Mutex::new(HashSet::new()),
            attached_session_ids: Mutex::new(HashSet::new()),
            session_snapshots: Mutex::new(HashMap::new()),
            connection_state_listeners: Mutex::new(Vec::new()),
            snapshot_listeners: Mutex::new(Vec::new()),
            event_listeners: Mutex::new(Vec::new()),
            snapshot: Mutex::new(None),
            disposed: Mutex::new(false),
        })
    }

    /// Wire the connection's runtime callbacks to this client (Weak refs so
    /// dispose can drop the cycle).
    fn wire_connection(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let on_message: Arc<dyn Fn(&ServerMessage) + Send + Sync> = Arc::new(move |message| {
            if let Some(client) = weak.upgrade() {
                client.handle_message(message);
            }
        });
        let weak = Arc::downgrade(self);
        let on_state_change: Arc<dyn Fn(&ConnectionStateChange) + Send + Sync> = Arc::new(move |change| {
            if let Some(client) = weak.upgrade() {
                client.handle_connection_state_change(change);
            }
        });
        let weak = Arc::downgrade(self);
        let on_handshake: Arc<dyn Fn(&ServerSnapshot) + Send + Sync> = Arc::new(move |snapshot| {
            if let Some(client) = weak.upgrade() {
                client.handle_handshake(snapshot);
            }
        });
        self.connection.set_options(ConnectionOptions {
            transport_factory: self.connection.transport_factory(),
            max_frame_length: None,
            on_handshake,
            on_message,
            on_state_change,
        });
    }

    fn handle_handshake(&self, snapshot: &ServerSnapshot) {
        *self.snapshot.lock().unwrap() = Some(snapshot.clone());
        let listeners: Vec<Arc<dyn Fn(&ServerSnapshot) + Send + Sync>> =
            self.snapshot_listeners.lock().unwrap().clone();
        for listener in listeners {
            listener(snapshot);
        }
    }

    /// Connect the transport and block for the handshake.
    pub fn connect(&self) -> Result<ServerSnapshot, ClientError> {
        self.assert_not_disposed()?;
        if self.connection.state() == ConnectionState::Disconnected {
            *self.snapshot.lock().unwrap() = None;
        }
        self.connection.connect()
    }

    pub fn reconnect(&self) -> Result<ServerSnapshot, ClientError> {
        self.connect()
    }

    pub fn disconnect(&self, reason: &str) {
        self.connection.disconnect(reason);
    }

    pub fn disposed(&self) -> bool {
        *self.disposed.lock().unwrap()
    }

    pub fn connected(&self) -> bool {
        self.connection.state() == ConnectionState::Connected
    }

    pub fn connection_state(&self) -> ConnectionState {
        self.connection.state()
    }

    pub fn snapshot(&self) -> Option<ServerSnapshot> {
        self.snapshot.lock().unwrap().clone()
    }

    pub fn subscribe(self: &Arc<Self>, listener: Arc<dyn Fn(&ServerSnapshot) + Send + Sync>) -> Unsubscribe {
        self.assert_not_disposed().ok();
        self.snapshot_listeners.lock().unwrap().push(listener.clone());
        let this = self.clone();
        Box::new(move || {
            let mut listeners = this.snapshot_listeners.lock().unwrap();
            listeners.retain(|existing| !Arc::ptr_eq(existing, &listener));
        })
    }

    pub fn on_event(self: &Arc<Self>, listener: Arc<dyn Fn(&ServerEvent) + Send + Sync>) -> Unsubscribe {
        self.assert_not_disposed().ok();
        self.event_listeners.lock().unwrap().push(listener.clone());
        let this = self.clone();
        Box::new(move || {
            let mut listeners = this.event_listeners.lock().unwrap();
            listeners.retain(|existing| !Arc::ptr_eq(existing, &listener));
        })
    }

    pub fn on_connection_state_change(
        self: &Arc<Self>,
        listener: Arc<dyn Fn(&ConnectionStateChange) + Send + Sync>,
    ) -> Unsubscribe {
        self.assert_not_disposed().ok();
        self.connection_state_listeners.lock().unwrap().push(listener.clone());
        let this = self.clone();
        Box::new(move || {
            let mut listeners = this.connection_state_listeners.lock().unwrap();
            listeners.retain(|existing| !Arc::ptr_eq(existing, &listener));
        })
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionMetadata>, ClientError> {
        let result = self.request(Command::List)?;
        match result {
            CommandResult::List { sessions } => Ok(sessions),
            _ => Err(ClientError::Other("Unexpected response command".to_string())),
        }
    }

    pub fn create_session(self: &Arc<Self>, options: &CreateSessionOptions) -> Result<PiSessionHandle, ClientError> {
        let result = self.request(Command::Create {
            cwd: options.cwd.clone(),
            name: options.name.clone(),
            model: options.model.clone(),
            thinking_level: options.thinking_level.clone(),
        })?;
        let session = match result {
            CommandResult::Create { session } => session,
            _ => return Err(ClientError::Other("Unexpected response command".to_string())),
        };
        self.reserve_session_lease(&session.id, SessionLeaseMode::Exclusive)?;
        Ok(self.create_session_lease(&session.id))
    }

    pub fn attach_session(self: &Arc<Self>, session_id: &str) -> Result<PiSessionHandle, ClientError> {
        self.acquire_session(
            session_id,
            AcquireSessionOptions {
                mode: SessionLeaseMode::Shared,
            },
        )
    }

    pub fn acquire_session(
        self: &Arc<Self>,
        session_id: &str,
        options: AcquireSessionOptions,
    ) -> Result<PiSessionHandle, ClientError> {
        self.assert_not_disposed()?;
        let token = self.reserve_session_lease(session_id, options.mode.clone())?;
        let attach_result = (|| -> Result<(), ClientError> {
            if !self.is_session_attached(session_id) {
                // Attach: forget snapshot, request attach, restore on failure.
                let previous = self.forget_session_snapshot(session_id);
                match self.request(Command::Attach {
                    session_id: session_id.to_string(),
                }) {
                    Ok(_) => Ok(()),
                    Err(error) => {
                        if let Some(previous) = previous {
                            self.restore_session_snapshot(previous);
                        }
                        Err(error)
                    }
                }
            } else {
                Ok(())
            }
        })();
        if let Err(error) = attach_result {
            self.release_session_lease(session_id, token);
            return Err(error);
        }
        Ok(self.create_session_lease(session_id))
    }

    /// Send a command and block for the response.
    pub fn request(&self, command: Command) -> Result<CommandResult, ClientError> {
        self.assert_not_disposed()?;
        if !self.connected() {
            return Err(ClientError::Disconnected(PiDisconnectedError::default()));
        }
        let id = {
            let mut sequence = self.request_sequence.lock().unwrap();
            *sequence += 1;
            format!("request-{sequence}")
        };
        let result = Arc::new(Mutex::new(None::<Result<CommandResult, ClientError>>));
        let condvar = Arc::new(Condvar::new());
        self.pending_requests.lock().unwrap().insert(
            id.clone(),
            PendingRequest {
                command: command.clone(),
                result: result.clone(),
                condvar: condvar.clone(),
            },
        );
        let frame = match pi_protocol::codec::encode_client_message(
            &pi_protocol::schemas::ClientMessage::Request {
                id: id.clone(),
                request: command,
            },
            Some(self.connection.max_frame_length()),
        ) {
            Ok(frame) => frame,
            Err(error) => {
                self.take_pending_request(&id);
                return Err(ClientError::Protocol(error));
            }
        };
        if let Err(error) = self.connection.send(&frame) {
            self.take_pending_request(&id);
            return Err(error);
        }
        // Block for the response.
        let mut slot = result.lock().unwrap();
        while slot.is_none() {
            slot = condvar.wait(slot).unwrap();
        }
        slot.take().unwrap()
    }

    pub fn dispose(&self) {
        let mut disposed = self.disposed.lock().unwrap();
        if *disposed {
            return;
        }
        *disposed = true;
        drop(disposed);
        let error = ClientError::Disposed(PiClientDisposedError::default());
        self.reject_pending_requests(error);
        self.connection.disconnect("Pi client is disposed");
        self.snapshot_listeners.lock().unwrap().clear();
        self.event_listeners.lock().unwrap().clear();
        self.connection_state_listeners.lock().unwrap().clear();
    }

    // ------------------------------------------------------------------
    // Internal state helpers
    // ------------------------------------------------------------------

    fn assert_not_disposed(&self) -> Result<(), ClientError> {
        if *self.disposed.lock().unwrap() {
            Err(ClientError::Disposed(PiClientDisposedError::default()))
        } else {
            Ok(())
        }
    }

    fn take_pending_request(&self, id: &str) -> Option<PendingRequest> {
        self.pending_requests.lock().unwrap().remove(id)
    }

    fn reject_pending_requests(&self, error: ClientError) {
        let requests: Vec<PendingRequest> = self.pending_requests.lock().unwrap().drain().map(|(_, request)| request).collect();
        for request in requests {
            *request.result.lock().unwrap() = Some(Err(error.clone()));
            request.condvar.notify_all();
        }
    }

    fn reserve_session_lease(&self, session_id: &str, mode: SessionLeaseMode) -> Result<(), ClientError> {
        let mut counts = self.session_lease_counts.lock().unwrap();
        let count = counts.get(session_id).copied().unwrap_or(0);
        if mode == SessionLeaseMode::Exclusive && count > 0 {
            return Err(ClientError::SessionOwnership(crate::errors::PiSessionOwnershipError {
                session_id: session_id.to_string(),
                message: format!("Session {session_id} already has an active lease"),
            }));
        }
        if mode == SessionLeaseMode::Shared && self.exclusive_session_leases.lock().unwrap().contains(session_id) {
            return Err(ClientError::SessionOwnership(crate::errors::PiSessionOwnershipError {
                session_id: session_id.to_string(),
                message: format!("Session {session_id} has an exclusive lease"),
            }));
        }
        counts.insert(session_id.to_string(), count + 1);
        if mode == SessionLeaseMode::Exclusive {
            self.exclusive_session_leases.lock().unwrap().insert(session_id.to_string());
        }
        Ok(())
    }

    fn release_session_lease(&self, session_id: &str, _token: ()) {
        let mut counts = self.session_lease_counts.lock().unwrap();
        let count = counts.get(session_id).copied().unwrap_or(0);
        if count <= 1 {
            counts.remove(session_id);
        } else {
            counts.insert(session_id.to_string(), count - 1);
        }
        self.exclusive_session_leases.lock().unwrap().remove(session_id);
    }

    fn create_session_lease(self: &Arc<Self>, session_id: &str) -> PiSessionHandle {
        let mut generations = self.session_lease_generations.lock().unwrap();
        let generation = generations.get(session_id).copied().unwrap_or(0);
        generations.insert(session_id.to_string(), generation);
        PiSessionHandle {
            session_id: session_id.to_string(),
            client: self.clone(),
            generation,
        }
    }

    fn session_lease_is_active(&self, session_id: &str, generation: u64) -> bool {
        let current = self.session_lease_generations.lock().unwrap().get(session_id).copied().unwrap_or(0);
        current == generation && self.is_session_attached(session_id)
    }

    fn assert_lease_active(&self, session_id: &str, generation: u64) -> Result<(), ClientError> {
        self.assert_not_disposed()?;
        if !self.connected() {
            return Err(ClientError::Disconnected(PiDisconnectedError::default()));
        }
        if !self.session_lease_is_active(session_id, generation) {
            return Err(ClientError::SessionDetached(crate::errors::PiSessionDetachedError::new(
                session_id,
            )));
        }
        Ok(())
    }

    fn release_lease(&self, session_id: &str, generation: u64, relinquish_on_failure: bool) -> Result<(), ClientError> {
        if !self.session_lease_is_active(session_id, generation) {
            return Ok(());
        }
        let count = self.session_lease_counts.lock().unwrap().get(session_id).copied().unwrap_or(0);
        if count <= 1 {
            let result = self.request(Command::Detach {
                session_id: session_id.to_string(),
            });
            match result {
                Ok(_) => {
                    self.release_session_lease(session_id, ());
                    // The lease is released; invalidate the handle's
                    // generation so is_attached reports false (JS lease
                    // state transitions to "released").
                    let mut generations = self.session_lease_generations.lock().unwrap();
                    let generation = generations.get(session_id).copied().unwrap_or(0) + 1;
                    generations.insert(session_id.to_string(), generation);
                    self.attached_session_ids.lock().unwrap().remove(session_id);
                    Ok(())
                }
                Err(error) => {
                    if relinquish_on_failure {
                        self.release_session_lease(session_id, ());
                        self.session_cleanup_required.lock().unwrap().insert(session_id.to_string());
                        Ok(())
                    } else {
                        Err(error)
                    }
                }
            }
        } else {
            self.release_session_lease(session_id, ());
            Ok(())
        }
    }

    fn is_session_attached(&self, session_id: &str) -> bool {
        self.attached_session_ids.lock().unwrap().contains(session_id)
    }

    fn forget_session_snapshot(&self, session_id: &str) -> Option<SessionSnapshot> {
        self.session_snapshots.lock().unwrap().remove(session_id)
    }

    fn restore_session_snapshot(&self, snapshot: SessionSnapshot) {
        let mut snapshots = self.session_snapshots.lock().unwrap();
        if !snapshots.contains_key(&snapshot.id) {
            snapshots.insert(snapshot.id.clone(), snapshot);
        }
    }

    fn handle_message(&self, message: &ServerMessage) {
        match message {
            ServerMessage::Response(envelope) => self.handle_response(envelope),
            ServerMessage::Event(envelope) => self.handle_event(envelope),
            ServerMessage::Hello { .. } | ServerMessage::HelloError { .. } => {}
        }
    }

    fn handle_response(&self, envelope: &ResponseEnvelope) {
        let (id, ok_result) = match envelope {
            ResponseEnvelope::Ok { id, result } => (id.clone(), Some(result.clone())),
            ResponseEnvelope::Err { id, error: _ } => (id.clone(), None),
        };
        let Some(pending) = self.take_pending_request(&id) else {
            // Response with no matching request: fail the connection.
            self.connection.fail(ClientError::Protocol(
                pi_protocol::codec::ProtocolValidationError("Response has no matching request".to_string()),
            ));
            return;
        };
        let outcome = match ok_result {
            None => {
                let error = match envelope {
                    ResponseEnvelope::Err { error, .. } => error,
                    _ => unreachable!(),
                };
                Err(ClientError::Server(PiServerError::new(error)))
            }
            Some(result) => {
                // Track attached sessions from create/attach/prompt results.
                if let Some(session) = result_session(&result) {
                    self.session_snapshots
                        .lock()
                        .unwrap()
                        .insert(session.id.clone(), session.clone());
                    self.attached_session_ids.lock().unwrap().insert(session.id.clone());
                }
                if result_command_name(&result) != command_name(&pending.command) {
                    let error = ClientError::Protocol(pi_protocol::codec::ProtocolValidationError(format!(
                        "Response command {} does not match {}",
                        result_command_name(&result),
                        command_name(&pending.command)
                    )));
                    self.connection.fail(error.clone());
                    Err(error)
                } else {
                    Ok(result)
                }
            }
        };
        *pending.result.lock().unwrap() = Some(outcome);
        pending.condvar.notify_all();
    }

    fn handle_event(&self, envelope: &EventEnvelope) {
        let event = &envelope.event;
        if let ServerEvent::SessionRemoved { session_id } = event {
            self.invalidate_session_leases(session_id);
        }
        // Deliver to event listeners.
        let listeners: Vec<Arc<dyn Fn(&ServerEvent) + Send + Sync>> = self.event_listeners.lock().unwrap().clone();
        for listener in listeners {
            listener(event);
        }
        // Session snapshots from events update the local state.
        match event {
            ServerEvent::ServerSnapshot { snapshot } => {
                *self.snapshot.lock().unwrap() = Some(snapshot.clone());
                let listeners: Vec<Arc<dyn Fn(&ServerSnapshot) + Send + Sync>> =
                    self.snapshot_listeners.lock().unwrap().clone();
                for listener in listeners {
                    listener(snapshot);
                }
            }
            ServerEvent::SessionSnapshot { snapshot } => {
                self.session_snapshots
                    .lock()
                    .unwrap()
                    .insert(snapshot.id.clone(), snapshot.clone());
                self.attached_session_ids.lock().unwrap().insert(snapshot.id.clone());
            }
            ServerEvent::SessionRemoved { session_id } => {
                self.session_snapshots.lock().unwrap().remove(session_id);
                self.attached_session_ids.lock().unwrap().remove(session_id);
            }
            ServerEvent::SessionProgress { .. } => {}
        }
    }

    fn invalidate_session_leases(&self, session_id: &str) {
        self.session_lease_counts.lock().unwrap().remove(session_id);
        self.exclusive_session_leases.lock().unwrap().remove(session_id);
        self.session_cleanup_required.lock().unwrap().remove(session_id);
        let mut generations = self.session_lease_generations.lock().unwrap();
        let generation = generations.get(session_id).copied().unwrap_or(0) + 1;
        generations.insert(session_id.to_string(), generation);
    }

    fn handle_connection_state_change(&self, change: &ConnectionStateChange) {
        if change.state == ConnectionState::Disconnected {
            self.attached_session_ids.lock().unwrap().clear();
            let sessions: Vec<String> = self.session_lease_counts.lock().unwrap().keys().cloned().collect();
            for session_id in sessions {
                self.invalidate_session_leases(&session_id);
            }
            self.session_cleanup_required.lock().unwrap().clear();
            let error = change
                .error
                .clone()
                .unwrap_or_else(|| ClientError::Disconnected(PiDisconnectedError::default()));
            self.reject_pending_requests(error);
        }
        let listeners: Vec<Arc<dyn Fn(&ConnectionStateChange) + Send + Sync>> =
            self.connection_state_listeners.lock().unwrap().clone();
        for listener in listeners {
            listener(change);
        }
    }
}

/// Command kind names for mismatch checks (JS `result.command !== pending.command`).
fn command_name(command: &Command) -> &'static str {
    match command {
        Command::List => "list",
        Command::Create { .. } => "create",
        Command::Attach { .. } => "attach",
        Command::Detach { .. } => "detach",
        Command::Prompt { .. } => "prompt",
        Command::Steer { .. } => "steer",
        Command::Abort { .. } => "abort",
        Command::SetModel { .. } => "set_model",
        Command::SetThinking { .. } => "set_thinking",
    }
}

fn result_session(result: &CommandResult) -> Option<pi_protocol::schemas::SessionSnapshot> {
    match result {
        CommandResult::Create { session }
        | CommandResult::Attach { session }
        | CommandResult::Prompt { session }
        | CommandResult::Steer { session }
        | CommandResult::Abort { session }
        | CommandResult::SetModel { session }
        | CommandResult::SetThinking { session } => Some(session.clone()),
        _ => None,
    }
}

fn result_command_name(result: &CommandResult) -> &'static str {
    match result {
        CommandResult::List { .. } => "list",
        CommandResult::Create { .. } => "create",
        CommandResult::Attach { .. } => "attach",
        CommandResult::Prompt { .. } => "prompt",
        CommandResult::Steer { .. } => "steer",
        CommandResult::Abort { .. } => "abort",
        CommandResult::SetModel { .. } => "set_model",
        CommandResult::SetThinking { .. } => "set_thinking",
        CommandResult::Detach { .. } => "detach",
    }
}


