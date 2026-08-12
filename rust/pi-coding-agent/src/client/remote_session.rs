//! Remote session wrapper over pi-client, port of `client/remote-session.ts`.
//!
//! ponytail: lifecycle is simplified to unbound/ready/busy/disposed;
//! operations are synchronous (the pi-client request is blocking).

use std::sync::Arc;

use pi_client::client::{CreateSessionOptions, PiClient, PiSessionHandle};
use pi_protocol::schemas::{Command, ModelRef, SessionSnapshot, TranscriptProgress};

use super::transcript::{apply_transcript_snapshot, create_transcript_state, select_transcript, TranscriptState};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RemoteSessionOperation {
    Open,
    Create,
    Submit,
    Abort,
    SetModel,
    SetThinking,
    Reconnect,
}

impl RemoteSessionOperation {
    pub fn as_str(&self) -> &'static str {
        match self {
            RemoteSessionOperation::Open => "open",
            RemoteSessionOperation::Create => "create",
            RemoteSessionOperation::Submit => "submit",
            RemoteSessionOperation::Abort => "abort",
            RemoteSessionOperation::SetModel => "setModel",
            RemoteSessionOperation::SetThinking => "setThinking",
            RemoteSessionOperation::Reconnect => "reconnect",
        }
    }
}

#[derive(Clone, Debug)]
pub enum RemoteSessionLifecycle {
    Unbound,
    Ready,
    Busy { operation: RemoteSessionOperation },
    Disposed,
}

impl RemoteSessionLifecycle {
    pub fn status(&self) -> &'static str {
        match self {
            RemoteSessionLifecycle::Unbound => "unbound",
            RemoteSessionLifecycle::Ready => "ready",
            RemoteSessionLifecycle::Busy { .. } => "busy",
            RemoteSessionLifecycle::Disposed => "disposed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RemoteSessionState {
    pub lifecycle: RemoteSessionLifecycle,
    pub snapshot: Option<SessionSnapshot>,
    pub transcript: Vec<pi_protocol::schemas::TranscriptItem>,
}

#[derive(Clone, Debug)]
pub struct CreateRemoteSessionOptions {
    pub cwd: String,
    pub model: Option<ModelRef>,
    pub thinking_level: Option<String>,
}

pub struct RemoteSession {
    client: Arc<PiClient>,
    handle: Option<PiSessionHandle>,
    lifecycle: RemoteSessionLifecycle,
    transcript: Option<TranscriptState>,
    listeners: Vec<Arc<dyn Fn(&RemoteSessionState) + Send + Sync>>,
    on_progress: Option<Arc<dyn Fn(&TranscriptProgress) + Send + Sync>>,
}

impl RemoteSession {
    pub fn new(client: Arc<PiClient>) -> Self {
        Self {
            client,
            handle: None,
            lifecycle: RemoteSessionLifecycle::Unbound,
            transcript: None,
            listeners: Vec::new(),
            on_progress: None,
        }
    }

    pub fn id(&self) -> Option<String> {
        self.handle.as_ref().map(|h| h.session_id().to_string())
    }

    pub fn state(&self) -> RemoteSessionState {
        let snapshot = self.snapshot();
        let transcript = self
            .transcript
            .as_ref()
            .map(select_transcript)
            .unwrap_or_default();
        RemoteSessionState {
            lifecycle: self.lifecycle.clone(),
            snapshot,
            transcript,
        }
    }

    pub fn snapshot(&self) -> Option<SessionSnapshot> {
        self.transcript.as_ref().map(|t| t.snapshot.clone())
    }

    pub fn lifecycle(&self) -> &RemoteSessionLifecycle {
        &self.lifecycle
    }

    pub fn disposed(&self) -> bool {
        matches!(self.lifecycle, RemoteSessionLifecycle::Disposed)
    }

    /// Subscribe to state changes; returns a subscription id.
    pub fn subscribe(&mut self, listener: Arc<dyn Fn(&RemoteSessionState) + Send + Sync>) -> usize {
        self.listeners.push(listener);
        self.listeners.len() - 1
    }

    pub fn unsubscribe(&mut self, index: usize) {
        if index < self.listeners.len() {
            self.listeners.remove(index);
        }
    }

    /// Register a progress handler (transcript deltas).
    pub fn on_progress(&mut self, handler: Arc<dyn Fn(&TranscriptProgress) + Send + Sync>) {
        self.on_progress = Some(handler);
    }

    fn notify(&self) {
        let state = self.state();
        for listener in &self.listeners {
            listener(&state);
        }
    }

    fn set_busy(&mut self, operation: RemoteSessionOperation) {
        self.lifecycle = RemoteSessionLifecycle::Busy { operation };
    }

    fn attach_snapshot(&mut self, snapshot: SessionSnapshot) {
        self.transcript = Some(apply_transcript_snapshot(&self.transcript.clone().unwrap_or_else(|| create_transcript_state(snapshot.clone())), snapshot));
        self.lifecycle = RemoteSessionLifecycle::Ready;
    }

    /// Open an existing session by id.
    pub fn open(&mut self, session_id: &str) -> Result<(), String> {
        if self.disposed() {
            return Err("Remote session is disposed".to_string());
        }
        self.set_busy(RemoteSessionOperation::Open);
        let handle = self
            .client
            .attach_session(session_id)
            .map_err(|error| error.to_string())?;
        let snapshot = handle.request(Command::Attach {
            session_id: session_id.to_string(),
        });
        match snapshot {
            Ok(pi_protocol::schemas::CommandResult::Attach { session }) => {
                self.handle = Some(handle);
                self.attach_snapshot(session);
                self.notify();
                Ok(())
            }
            Ok(_) => Err("unexpected attach result".to_string()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Create a new session.
    pub fn create(&mut self, options: &CreateRemoteSessionOptions) -> Result<(), String> {
        if self.disposed() {
            return Err("Remote session is disposed".to_string());
        }
        self.set_busy(RemoteSessionOperation::Create);
        let handle = self
            .client
            .create_session(&CreateSessionOptions {
                cwd: Some(options.cwd.clone()),
                name: None,
                model: options.model.clone(),
                thinking_level: options.thinking_level.clone(),
            })
            .map_err(|error| error.to_string())?;
        let _session_id = handle.session_id().to_string();
        let result = handle.request(Command::Create {
            cwd: Some(options.cwd.clone()),
            name: None,
            model: options.model.clone(),
            thinking_level: options.thinking_level.clone(),
        });
        match result {
            Ok(pi_protocol::schemas::CommandResult::Create { session }) => {
                self.handle = Some(handle);
                self.attach_snapshot(session);
                self.notify();
                Ok(())
            }
            Ok(_) => Err("unexpected create result".to_string()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn require_handle(&self) -> Result<&PiSessionHandle, String> {
        match &self.handle {
            Some(handle) => Ok(handle),
            None => Err("Remote session is not open".to_string()),
        }
    }

    /// Submit a prompt.
    pub fn submit(&mut self, text: &str) -> Result<SessionSnapshot, String> {
        if self.disposed() {
            return Err("Remote session is disposed".to_string());
        }
        self.set_busy(RemoteSessionOperation::Submit);
        let handle = self.require_handle()?;
        let session_id = handle.session_id().to_string();
        let result = handle.request(Command::Prompt {
            session_id,
            text: text.to_string(),
        });
        match result {
            Ok(pi_protocol::schemas::CommandResult::Prompt { session }) => {
                self.attach_snapshot(session.clone());
                self.notify();
                Ok(session)
            }
            Ok(_) => Err("unexpected prompt result".to_string()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Abort the current run.
    pub fn abort(&mut self) -> Result<(), String> {
        if self.disposed() {
            return Err("Remote session is disposed".to_string());
        }
        self.set_busy(RemoteSessionOperation::Abort);
        let handle = self.require_handle()?;
        let session_id = handle.session_id().to_string();
        let result = handle.request(Command::Abort { session_id });
        match result {
            Ok(pi_protocol::schemas::CommandResult::Abort { session }) => {
                self.attach_snapshot(session);
                self.notify();
                Ok(())
            }
            Ok(_) => Err("unexpected abort result".to_string()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Set the model.
    pub fn set_model(&mut self, model: &ModelRef) -> Result<(), String> {
        if self.disposed() {
            return Err("Remote session is disposed".to_string());
        }
        self.set_busy(RemoteSessionOperation::SetModel);
        let handle = self.require_handle()?;
        let session_id = handle.session_id().to_string();
        let result = handle.request(Command::SetModel {
            session_id,
            model: model.clone(),
        });
        match result {
            Ok(pi_protocol::schemas::CommandResult::SetModel { session }) => {
                self.attach_snapshot(session);
                self.notify();
                Ok(())
            }
            Ok(_) => Err("unexpected setModel result".to_string()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Set the thinking level.
    pub fn set_thinking(&mut self, thinking_level: &str) -> Result<(), String> {
        if self.disposed() {
            return Err("Remote session is disposed".to_string());
        }
        self.set_busy(RemoteSessionOperation::SetThinking);
        let handle = self.require_handle()?;
        let session_id = handle.session_id().to_string();
        let result = handle.request(Command::SetThinking {
            session_id,
            thinking_level: thinking_level.to_string(),
        });
        match result {
            Ok(pi_protocol::schemas::CommandResult::SetThinking { session }) => {
                self.attach_snapshot(session);
                self.notify();
                Ok(())
            }
            Ok(_) => Err("unexpected setThinking result".to_string()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Reconnect and re-acquire the current session.
    pub fn reconnect(&mut self) -> Result<(), String> {
        if self.disposed() {
            return Err("Remote session is disposed".to_string());
        }
        self.set_busy(RemoteSessionOperation::Reconnect);
        self.client.reconnect().map_err(|error| error.to_string())?;
        self.lifecycle = RemoteSessionLifecycle::Ready;
        self.notify();
        Ok(())
    }

    /// Detach and dispose.
    pub fn dispose(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.dispose();
        }
        self.lifecycle = RemoteSessionLifecycle::Disposed;
        self.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_labels() {
        assert_eq!(RemoteSessionOperation::Submit.as_str(), "submit");
        assert_eq!(RemoteSessionOperation::SetThinking.as_str(), "setThinking");
    }

    #[test]
    fn lifecycle_status_labels() {
        assert_eq!(RemoteSessionLifecycle::Unbound.status(), "unbound");
        assert_eq!(RemoteSessionLifecycle::Ready.status(), "ready");
        assert_eq!(
            RemoteSessionLifecycle::Busy {
                operation: RemoteSessionOperation::Abort
            }
            .status(),
            "busy"
        );
        assert_eq!(RemoteSessionLifecycle::Disposed.status(), "disposed");
    }

    #[test]
    fn state_snapshots_without_transcript() {
        let state = RemoteSessionState {
            lifecycle: RemoteSessionLifecycle::Unbound,
            snapshot: None,
            transcript: Vec::new(),
        };
        assert!(state.snapshot.is_none());
        assert!(state.transcript.is_empty());
    }
}
