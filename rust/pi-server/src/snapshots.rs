//! Server snapshot publisher, port of `packages/server/src/snapshots.ts`.

use std::sync::{Arc, Mutex};

use pi_protocol::schemas::{EventEnvelope, PROTOCOL_VERSION, ServerEvent, ServerSnapshot};

use crate::sessions::{ConnectionState, PiServerService};

pub struct ServerSnapshotPublisherOptions {
    pub server_id: String,
    pub service: Arc<dyn PiServerService>,
    pub connections: Arc<Mutex<Vec<ConnectionState>>>,
    pub is_closing: Arc<dyn Fn() -> bool + Send + Sync>,
    pub list_sessions: Arc<dyn Fn() -> Vec<pi_protocol::schemas::SessionMetadata> + Send + Sync>,
    pub send_message: Arc<dyn Fn(&ConnectionState, &EventEnvelope) -> bool + Send + Sync>,
    pub report_error: Arc<dyn Fn(&dyn std::fmt::Debug) + Send + Sync>,
}

pub struct ServerSnapshotPublisher {
    options: Mutex<Arc<ServerSnapshotPublisherOptions>>,
    revision: Mutex<f64>,
}

impl ServerSnapshotPublisher {
    pub fn new(options: ServerSnapshotPublisherOptions) -> Self {
        Self {
            options: Mutex::new(Arc::new(options)),
            revision: Mutex::new(0.0),
        }
    }

    /// Swap the callbacks (used by PiServer to wire self-referential
    /// closures after construction).
    pub fn set_options(&self, options: ServerSnapshotPublisherOptions) {
        *self.options.lock().unwrap() = Arc::new(options);
    }

    pub fn options(&self) -> Arc<ServerSnapshotPublisherOptions> {
        self.options.lock().unwrap().clone()
    }

    pub fn current_revision(&self) -> f64 {
        *self.revision.lock().unwrap()
    }

    pub fn get(&self) -> ServerSnapshot {
        ServerSnapshot {
            server_id: self.options.lock().unwrap().server_id.clone(),
            protocol_version: PROTOCOL_VERSION,
            revision: self.current_revision(),
            sessions: (self.options.lock().unwrap().list_sessions)(),
            models: self.options.lock().unwrap().service.list_models(),
        }
    }

    pub fn broadcast(&self) {
        let ready_connections: Vec<ConnectionState> = self
            .options
            .lock()
            .unwrap()
            .connections
            .lock()
            .unwrap()
            .iter()
            .filter(|connection| {
                connection.stage.lock().unwrap().as_str() == "ready"
                    && !connection.disconnected.load(std::sync::atomic::Ordering::SeqCst)
            })
            .cloned()
            .collect();
        if ready_connections.is_empty() || (self.options.lock().unwrap().is_closing)() {
            return;
        }
        let revision = *self.revision.lock().unwrap() + 1.0;
        *self.revision.lock().unwrap() = revision;
        let mut snapshot = self.get();
        snapshot.revision = revision;
        let envelope = EventEnvelope {
            event: ServerEvent::ServerSnapshot { snapshot },
        };
        for connection in ready_connections {
            (self.options.lock().unwrap().send_message)(&connection, &envelope);
        }
    }
}
