//! LiveSessionManager tests over an in-memory service.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use pi_protocol::schemas::{Command, CommandResult, ModelRef, SessionPhase, SessionSnapshot};
use pi_server::errors::PiServerError;
use pi_server::sessions::{
    ConnectionState, CreateSessionOptions, LiveSessionManager, LiveSessionManagerOptions, PiServerService,
    SessionConnection, SessionRuntime, SessionRuntimeEvent,
};

fn snapshot(id: &str, name: Option<String>) -> SessionSnapshot {
    SessionSnapshot {
        id: id.to_string(),
        name,
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

struct MemRuntime {
    id: String,
    phase: Mutex<SessionPhase>,
    disposed: Arc<Mutex<bool>>,
}

impl SessionRuntime for MemRuntime {
    fn snapshot(&self) -> SessionSnapshot {
        snapshot(&self.id, Some("test".to_string()))
    }
    fn get_phase(&self) -> SessionPhase {
        self.phase.lock().unwrap().clone()
    }
    fn prompt(&self, _text: &str) -> Result<(), PiServerError> {
        *self.phase.lock().unwrap() = "prompting".to_string();
        *self.phase.lock().unwrap() = "idle".to_string();
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
        *self.disposed.lock().unwrap() = true;
        Ok(())
    }
}

struct MemService {
    runtimes: Mutex<HashMap<String, Arc<dyn SessionRuntime>>>,
    disposed: Arc<Mutex<bool>>,
}

impl MemService {
    fn new() -> Self {
        Self {
            runtimes: Mutex::new(HashMap::new()),
            disposed: Arc::new(Mutex::new(false)),
        }
    }
}

impl PiServerService for MemService {
    fn list_sessions(&self) -> Vec<pi_protocol::schemas::SessionMetadata> {
        self.runtimes
            .lock()
            .unwrap()
            .keys()
            .map(|id| pi_protocol::schemas::SessionMetadata {
                id: id.clone(),
                created_at: 1.0,
                updated_at: None,
                parent_session_id: None,
                session_name: Some("test".to_string()),
                cwd: Some("/tmp".to_string()),
            })
            .collect()
    }
    fn list_models(&self) -> Vec<pi_protocol::schemas::ModelMetadata> {
        vec![]
    }
    fn create_session(&self, options: &CreateSessionOptions) -> Result<Arc<dyn SessionRuntime>, PiServerError> {
        let runtime = Arc::new(MemRuntime {
            id: options.id.clone(),
            phase: Mutex::new("idle".to_string()),
            disposed: self.disposed.clone(),
        });
        self.runtimes
            .lock()
            .unwrap()
            .insert(options.id.clone(), runtime.clone());
        Ok(runtime)
    }
    fn open_session(&self, session_id: &str) -> Result<Arc<dyn SessionRuntime>, PiServerError> {
        self.runtimes
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .ok_or_else(PiServerError::not_found)
    }
}

fn new_connection(id: &str) -> ConnectionState {
    Arc::new(SessionConnection {
        id: id.to_string(),
        disconnected: false,
        stage: "ready".to_string(),
        closed: false,
        session_ids: Arc::new(Mutex::new(Vec::new())),
    })
}

fn manager(service: Arc<MemService>) -> Arc<LiveSessionManager> {
    let service = service.clone();
    Arc::new(LiveSessionManager::new(LiveSessionManagerOptions {
        service,
        is_closing: Arc::new(|| false),
        send_message: Arc::new(|_, _| true),
        close_connection: Arc::new(|_| {}),
        disconnect_connection: Arc::new(|_| {}),
        broadcast_server_snapshot: Arc::new(|| {}),
        report_error: Arc::new(|_| {}),
    }))
}

#[test]
fn create_attach_prompt_detach_cycle() {
    let service = Arc::new(MemService::new());
    let manager = manager(service.clone());
    let connection = new_connection("c1");

    let result = manager
        .execute_command(&connection, &Command::Create {
            cwd: None,
            name: None,
            model: None,
            thinking_level: None,
        })
        .unwrap();
    let CommandResult::Create { session } = result else {
        panic!("expected create result");
    };
    assert!(session.attached);
    assert!(session.locked);

    let session_id = session.id.clone();
    let result = manager
        .execute_command(&connection, &Command::Prompt {
            session_id: session_id.clone(),
            text: "hi".to_string(),
        })
        .unwrap();
    let CommandResult::Prompt { session } = result else {
        panic!("expected prompt result");
    };
    assert!(session.attached);

    // Detach releases the runtime (no other connections).
    let result = manager
        .execute_command(&connection, &Command::Detach {
            session_id: session_id.clone(),
        })
        .unwrap();
    assert!(matches!(result, CommandResult::Detach { .. }));
    assert!(service.disposed.lock().unwrap().clone());
}

#[test]
fn prompt_without_attach_rejects() {
    let service = Arc::new(MemService::new());
    let manager = manager(service.clone());
    let connection = new_connection("c1");
    let error = manager
        .execute_command(&connection, &Command::Prompt {
            session_id: "missing".to_string(),
            text: "hi".to_string(),
        })
        .unwrap_err();
    assert_eq!(error.code, "invalid_request");
}

#[test]
fn list_metadata_includes_live() {
    let service = Arc::new(MemService::new());
    let manager = manager(service.clone());
    let connection = new_connection("c1");
    manager
        .execute_command(&connection, &Command::Create {
            cwd: None,
            name: None,
            model: None,
            thinking_level: None,
        })
        .unwrap();
    let sessions = manager.list_metadata();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_name.as_deref(), Some("test"));
}

#[test]
fn disconnect_cleans_up_attachments() {
    let service = Arc::new(MemService::new());
    let manager = manager(service.clone());
    let connection = new_connection("c1");
    manager
        .execute_command(&connection, &Command::Create {
            cwd: None,
            name: None,
            model: None,
            thinking_level: None,
        })
        .unwrap();
    manager.disconnect(&connection);
    assert!(service.disposed.lock().unwrap().clone());
}
