//! Live session manager, port of `packages/server/src/sessions.ts`.
//!
//! Synchronous analog: runtime calls block (matching the synchronous agent
//! model); broadcast/dispose scheduling is direct rather than microtask
//! based.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use pi_protocol::schemas::{
    Command, CommandResult, EventEnvelope, ModelRef, SessionMetadata, SessionPhase, SessionSnapshot,
    TranscriptProgress,
};

use crate::errors::PiServerError;

/// One acquired durable session. Conflicting operations must reject rather
/// than queue.
pub trait SessionRuntime: Send + Sync {
    fn snapshot(&self) -> SessionSnapshot;
    fn get_phase(&self) -> SessionPhase;
    fn prompt(&self, text: &str) -> Result<(), PiServerError>;
    fn steer(&self, text: &str) -> Result<(), PiServerError>;
    fn abort(&self) -> Result<(), PiServerError>;
    fn set_model(&self, model: &ModelRef) -> Result<(), PiServerError>;
    fn set_thinking(&self, thinking_level: &str) -> Result<(), PiServerError>;
    fn subscribe(&self, listener: Arc<dyn Fn(&SessionRuntimeEvent) + Send + Sync>) -> Box<dyn Fn() + Send + Sync>;
    fn dispose(&self) -> Result<(), PiServerError>;
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionRuntimeEvent {
    Snapshot,
    Progress { progress: TranscriptProgress },
    Error { error: PiServerError },
}

pub struct CreateSessionOptions {
    pub id: String,
    pub cwd: Option<String>,
    pub name: Option<String>,
    pub model: Option<ModelRef>,
    pub thinking_level: Option<String>,
}

/// Service boundary for durable sessions and exclusively acquired runtimes.
pub trait PiServerService: Send + Sync {
    fn list_sessions(&self) -> Vec<SessionMetadata>;
    fn list_models(&self) -> Vec<pi_protocol::schemas::ModelMetadata>;
    fn create_session(&self, options: &CreateSessionOptions) -> Result<Arc<dyn SessionRuntime>, PiServerError>;
    fn open_session(&self, session_id: &str) -> Result<Arc<dyn SessionRuntime>, PiServerError>;
}

/// Connection state shared with the server (subset used by the manager).
#[derive(Clone, Debug)]
pub struct SessionConnection {
    pub id: String,
    pub disconnected: bool,
    pub stage: String,
    pub closed: bool,
    pub session_ids: Arc<Mutex<Vec<String>>>,
}

impl PartialEq for SessionConnection {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for SessionConnection {}
impl std::hash::Hash for SessionConnection {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

pub type ConnectionState = Arc<SessionConnection>;

pub struct LiveSessionManagerOptions {
    pub service: Arc<dyn PiServerService>,
    pub is_closing: Arc<dyn Fn() -> bool + Send + Sync>,
    pub send_message: Arc<dyn Fn(&ConnectionState, &EventEnvelope) -> bool + Send + Sync>,
    pub close_connection: Arc<dyn Fn(&ConnectionState) + Send + Sync>,
    pub disconnect_connection: Arc<dyn Fn(&ConnectionState) + Send + Sync>,
    pub broadcast_server_snapshot: Arc<dyn Fn() + Send + Sync>,
    pub report_error: Arc<dyn Fn(&PiServerError) + Send + Sync>,
}

#[derive(Clone)]
struct LiveSession {
    id: String,
    runtime: Arc<dyn SessionRuntime>,
    connections: HashSet<ConnectionState>,
    operation_count: usize,
    ready: bool,
    terminal: bool,
    disposing: bool,
}

fn to_metadata(snapshot: &SessionSnapshot) -> SessionMetadata {
    SessionMetadata {
        id: snapshot.id.clone(),
        created_at: snapshot.created_at,
        updated_at: Some(snapshot.updated_at),
        session_name: snapshot.name.clone(),
        parent_session_id: None,
        cwd: Some(snapshot.cwd.clone()),
    }
}

pub struct LiveSessionManager {
    options: Arc<LiveSessionManagerOptions>,
    live_sessions: Mutex<HashMap<String, LiveSession>>,
}

impl LiveSessionManager {
    pub fn new(options: LiveSessionManagerOptions) -> Self {
        Self {
            options: Arc::new(options),
            live_sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn execute_command(self: &Arc<Self>, connection: &ConnectionState, command: &Command) -> Result<CommandResult, PiServerError> {
        match command {
            Command::List => Ok(CommandResult::List {
                sessions: self.list_metadata(),
            }),
            Command::Create {
                cwd,
                name,
                model,
                thinking_level,
            } => {
                let id = uuid_v7();
                let options = CreateSessionOptions {
                    id: id.clone(),
                    cwd: cwd.clone(),
                    name: name.clone(),
                    model: model.clone(),
                    thinking_level: thinking_level.clone(),
                };
                let live = self.acquire(&id, || self.options.service.create_session(&options))?;
                self.attach(connection, &live)?;
                let snapshot = self.for_connection(&self.broadcast_snapshot(&live)?, connection);
                (self.options.broadcast_server_snapshot)();
                Ok(CommandResult::Create { session: snapshot })
            }
            Command::Attach { session_id } => {
                let live = self.acquire(session_id, || self.options.service.open_session(session_id))?;
                self.attach(connection, &live)?;
                let snapshot = self.for_connection(&self.broadcast_snapshot(&live)?, connection);
                (self.options.broadcast_server_snapshot)();
                Ok(CommandResult::Attach { session: snapshot })
            }
            Command::Detach { session_id } => {
                let attached = connection.session_ids.lock().unwrap().iter().any(|id| id == session_id);
                if attached {
                    connection.session_ids.lock().unwrap().retain(|id| id != session_id);
                    let live = self.live_sessions.lock().unwrap().get(session_id).cloned();
                    if let Some(live) = live {
                        self.live_sessions
                            .lock()
                            .unwrap()
                            .get_mut(session_id)
                            .unwrap()
                            .connections
                            .remove(connection);
                        let should_broadcast = {
                            let current = self.live_sessions.lock().unwrap();
                            let live = current.get(session_id).unwrap();
                            live.connections.len() > 0 && !live.terminal && !live.disposing
                        };
                        if should_broadcast {
                            let _ = self.broadcast_snapshot(&live)?;
                        }
                        self.maybe_dispose(&live)?;
                    }
                    (self.options.broadcast_server_snapshot)();
                }
                Ok(CommandResult::Detach {
                    session_id: session_id.clone(),
                })
            }
            Command::Prompt { session_id, text } => {
                let live = self.require_attached(connection, session_id)?;
                let session = self.run_operation(connection, &live, || live.runtime.prompt(text))?;
                Ok(CommandResult::Prompt { session })
            }
            Command::Steer { session_id, text } => {
                let live = self.require_attached(connection, session_id)?;
                let session = self.run_operation(connection, &live, || live.runtime.steer(text))?;
                Ok(CommandResult::Steer { session })
            }
            Command::Abort { session_id } => {
                let live = self.require_attached(connection, session_id)?;
                let session = self.run_operation(connection, &live, || live.runtime.abort())?;
                Ok(CommandResult::Abort { session })
            }
            Command::SetModel { session_id, model } => {
                let live = self.require_attached(connection, session_id)?;
                let session = self.run_operation(connection, &live, || live.runtime.set_model(model))?;
                Ok(CommandResult::SetModel { session })
            }
            Command::SetThinking {
                session_id,
                thinking_level,
            } => {
                let live = self.require_attached(connection, session_id)?;
                let session = self.run_operation(connection, &live, || {
                    live.runtime.set_thinking(thinking_level)
                })?;
                Ok(CommandResult::SetThinking { session })
            }
        }
    }

    pub fn disconnect(&self, connection: &ConnectionState) {
        let session_ids: Vec<String> = connection.session_ids.lock().unwrap().clone();
        connection.session_ids.lock().unwrap().clear();
        let mut sessions: Vec<LiveSession> = Vec::new();
        for id in &session_ids {
            let mut guard = self.live_sessions.lock().unwrap();
            if let Some(live) = guard.get_mut(id) {
                live.connections.remove(connection);
                sessions.push(live.clone());
            }
        }
        for live in sessions {
            if let Err(error) = self.maybe_dispose(&live) {
                (self.options.report_error)(&error);
            }
        }
    }

    pub fn list_metadata(&self) -> Vec<SessionMetadata> {
        let stored = self.options.service.list_sessions();
        let mut live_snapshots: Vec<(String, SessionSnapshot)> = Vec::new();
        let guard = self.live_sessions.lock().unwrap();
        for live in guard.values() {
            if !live.disposing {
                if let Ok(snapshot) = self.normalized_snapshot(live) {
                    live_snapshots.push((live.id.clone(), snapshot));
                }
            }
        }
        drop(guard);
        let mut live_by_id: HashMap<String, SessionSnapshot> = live_snapshots.into_iter().collect();
        let mut metadata: Vec<SessionMetadata> = stored
            .iter()
            .map(|item| match live_by_id.remove(&item.id) {
                Some(snapshot) => {
                    let mut merged = item.clone();
                    let snapshot_meta = to_metadata(&snapshot);
                    merged.created_at = snapshot_meta.created_at;
                    merged.updated_at = snapshot_meta.updated_at;
                    merged.parent_session_id = snapshot_meta.parent_session_id;
                    merged.session_name = snapshot_meta.session_name;
                    merged.cwd = snapshot_meta.cwd;
                    merged
                }
                None => item.clone(),
            })
            .collect();
        for snapshot in live_by_id.values() {
            metadata.push(to_metadata(snapshot));
        }
        metadata
    }

    pub fn close(&self) {
        let sessions: Vec<LiveSession> = self.live_sessions.lock().unwrap().values().cloned().collect();
        self.live_sessions.lock().unwrap().clear();
        for live in sessions {
            if live.disposing {
                continue;
            }
            if let Err(error) = live.runtime.dispose() {
                (self.options.report_error)(&error);
            }
        }
    }

    // ------------------------------------------------------------------

    fn run_operation(
        &self,
        connection: &ConnectionState,
        live: &LiveSession,
        operation: impl FnOnce() -> Result<(), PiServerError>,
    ) -> Result<SessionSnapshot, PiServerError> {
        self.live_sessions.lock().unwrap().get_mut(&live.id).unwrap().operation_count += 1;
        let result = (|| -> Result<SessionSnapshot, PiServerError> {
            operation()?;
            Ok(self.for_connection(&self.broadcast_snapshot(live)?, connection))
        })();
        let count = {
            let mut guard = self.live_sessions.lock().unwrap();
            guard.get_mut(&live.id).unwrap().operation_count -= 1;
            guard.get(&live.id).unwrap().operation_count
        };
        if count == 0 {
            if let Err(error) = self.maybe_dispose(live) {
                (self.options.report_error)(&error);
            }
        }
        result
    }

    fn acquire(
        self: &Arc<Self>,
        id: &str,
        acquire_runtime: impl FnOnce() -> Result<Arc<dyn SessionRuntime>, PiServerError>,
    ) -> Result<LiveSession, PiServerError> {
        loop {
            if let Some(existing) = self.live_sessions.lock().unwrap().get(id).cloned() {
                if existing.terminal {
                    return Err(PiServerError::new(
                        "session_locked",
                        format!("Session runtime is terminating: {id}"),
                        None,
                    ));
                }
                if existing.disposing {
                    // Spin until the dispose finishes; the JS version awaits
                    // the disposing promise.
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                return Ok(existing);
            }
            // No concurrent-open tracking needed: the mutex serializes
            // creation, and the JS openingSessions map exists to dedupe
            // parallel opens, which cannot race here.
            let live = self.create(id, acquire_runtime)?;
            return Ok(live);
        }
    }

    fn create(
        self: &Arc<Self>,
        id: &str,
        acquire_runtime: impl FnOnce() -> Result<Arc<dyn SessionRuntime>, PiServerError>,
    ) -> Result<LiveSession, PiServerError> {
        let runtime = acquire_runtime()?;
        if (self.options.is_closing)() {
            let _ = runtime.dispose();
            return Err(PiServerError::new(
                "invalid_request",
                "PiServer closed while acquiring a session runtime",
                None,
            ));
        }
        let snapshot = runtime.snapshot();
        if snapshot.id != id {
            let _ = runtime.dispose();
            return Err(PiServerError::new(
                "invalid_request",
                format!("Service returned session {} for server-assigned session {id}", snapshot.id),
                None,
            ));
        }
        let live = LiveSession {
            id: id.to_string(),
            runtime,
            connections: HashSet::new(),
            operation_count: 0,
            ready: false,
            terminal: false,
            disposing: false,
        };
        // Subscribe for runtime events.
        let manager = self.clone();
        let live_id = id.to_string();
        let _ = live.runtime.subscribe(Arc::new(move |event| {
            manager.handle_runtime_event(&live_id, event);
        }));
        {
            let mut guard = self.live_sessions.lock().unwrap();
            guard.insert(id.to_string(), live.clone());
            guard.get_mut(id).unwrap().ready = true;
        }
        Ok(live)
    }

    fn handle_runtime_event(&self, live_id: &str, event: &SessionRuntimeEvent) {
        let live = match self.live_sessions.lock().unwrap().get(live_id).cloned() {
            Some(live) => live,
            None => return,
        };
        match event {
            SessionRuntimeEvent::Error { error } => {
                let error = error.clone();
                match self.terminate(&live, &error) {
                    Ok(()) => {}
                    Err(error) => (self.options.report_error)(&error),
                }
                return;
            }
            SessionRuntimeEvent::Progress { progress } => {
                let envelope = EventEnvelope {
                    event: pi_protocol::schemas::ServerEvent::SessionProgress {
                        session_id: live.id.clone(),
                        progress: progress.clone(),
                    },
                };
                let connections: Vec<ConnectionState> = live.connections.iter().cloned().collect();
                for connection in connections {
                    (self.options.send_message)(&connection, &envelope);
                }
            }
            SessionRuntimeEvent::Snapshot => {
                if let Err(error) = self.broadcast_snapshot(&live) {
                    (self.options.report_error)(&error);
                }
            }
        }
        self.schedule_maybe_dispose(&live);
    }

    fn terminate(&self, live: &LiveSession, error: &PiServerError) -> Result<(), PiServerError> {
        {
            let mut guard = self.live_sessions.lock().unwrap();
            let Some(current) = guard.get_mut(&live.id) else {
                return Ok(());
            };
            if current.terminal {
                return Ok(());
            }
            current.terminal = true;
        }
        (self.options.report_error)(error);
        let connections: Vec<ConnectionState> = live.connections.iter().cloned().collect();
        for connection in &connections {
            (self.options.close_connection)(connection);
        }
        for connection in &connections {
            (self.options.disconnect_connection)(connection);
        }
        self.maybe_dispose(live)
    }

    fn normalized_snapshot(&self, live: &LiveSession) -> Result<SessionSnapshot, PiServerError> {
        let mut snapshot = live.runtime.snapshot();
        if snapshot.id != live.id {
            return Err(PiServerError::new(
                "invalid_request",
                format!("Runtime session ID changed from {} to {}", live.id, snapshot.id),
                None,
            ));
        }
        snapshot.phase = live.runtime.get_phase();
        snapshot.attached = live.connections.len() > 0;
        snapshot.locked = true;
        Ok(snapshot)
    }

    fn for_connection(&self, snapshot: &SessionSnapshot, connection: &ConnectionState) -> SessionSnapshot {
        let mut snapshot = snapshot.clone();
        snapshot.attached = connection.session_ids.lock().unwrap().iter().any(|id| id == &snapshot.id);
        snapshot
    }

    fn broadcast_snapshot(&self, live: &LiveSession) -> Result<SessionSnapshot, PiServerError> {
        let snapshot = self.normalized_snapshot(live)?;
        let envelope = EventEnvelope {
            event: pi_protocol::schemas::ServerEvent::SessionSnapshot {
                snapshot: snapshot.clone(),
            },
        };
        let connections: Vec<ConnectionState> = live.connections.iter().cloned().collect();
        for connection in connections {
            (self.options.send_message)(&connection, &envelope);
        }
        Ok(snapshot)
    }

    fn attach(&self, connection: &ConnectionState, live: &LiveSession) -> Result<(), PiServerError> {
        if connection.disconnected || connection.stage != "ready" || connection.closed {
            self.maybe_dispose(live).ok();
            return Err(PiServerError::new(
                "invalid_request",
                "Connection closed while attaching to a session",
                None,
            ));
        }
        connection.session_ids.lock().unwrap().push(live.id.clone());
        self.live_sessions
            .lock()
            .unwrap()
            .get_mut(&live.id)
            .unwrap()
            .connections
            .insert(connection.clone());
        Ok(())
    }

    fn require_attached(&self, connection: &ConnectionState, session_id: &str) -> Result<LiveSession, PiServerError> {
        if !connection.session_ids.lock().unwrap().iter().any(|id| id == session_id) {
            return Err(PiServerError::new(
                "invalid_request",
                format!("Connection is not attached to session {session_id}"),
                None,
            ));
        }
        let guard = self.live_sessions.lock().unwrap();
        match guard.get(session_id) {
            Some(live) if !live.terminal && !live.disposing => Ok(live.clone()),
            _ => Err(PiServerError::new(
                "not_found",
                format!("Session is not live: {session_id}"),
                None,
            )),
        }
    }

    fn schedule_maybe_dispose(&self, live: &LiveSession) {
        if let Err(error) = self.maybe_dispose(live) {
            (self.options.report_error)(&error);
        }
    }

    fn maybe_dispose(&self, live: &LiveSession) -> Result<(), PiServerError> {
        {
            let guard = self.live_sessions.lock().unwrap();
            let Some(current) = guard.get(&live.id) else {
                return Ok(());
            };
            if (self.options.is_closing)()
                || !current.ready
                || current.disposing
                || current.connections.len() > 0
                || current.operation_count > 0
                || (!current.terminal && live.runtime.get_phase() != "idle")
            {
                return Ok(());
            }
        }
        {
            let mut guard = self.live_sessions.lock().unwrap();
            guard.get_mut(&live.id).unwrap().disposing = true;
        }
        let result = live.runtime.dispose();
        self.live_sessions.lock().unwrap().remove(&live.id);
        if !(self.options.is_closing)() {
            (self.options.broadcast_server_snapshot)();
        }
        result
    }
}

fn uuid_v7() -> String {
    pi_ai::utils::uuid::uuidv7()
}
