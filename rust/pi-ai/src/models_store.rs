//! Model store, port of `packages/ai/src/models-store.ts`.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::types::Model;

/// Stored catalog entry for one provider.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelsStoreEntry {
    pub models: Vec<Model>,
    /// Unix timestamp from the remote catalog's Last-Modified header.
    pub last_modified: Option<f64>,
    /// Unix timestamp of the last completed remote check.
    pub checked_at: Option<f64>,
    /// Opaque validator from the remote catalog's ETag header, stored
    /// verbatim (quotes included) and echoed back as If-None-Match.
    pub etag: Option<String>,
}

/// Provider-scoped model storage with atomic per-provider updates.
pub trait ModelsStore: Send + Sync {
    fn read(&self, provider_id: &str) -> Option<ModelsStoreEntry>;
    fn write(&self, provider_id: &str, entry: ModelsStoreEntry);
    fn delete(&self, provider_id: &str);
}

/// In-memory model store.
#[derive(Default)]
pub struct InMemoryModelsStore {
    entries: Mutex<HashMap<String, ModelsStoreEntry>>,
}

impl InMemoryModelsStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ModelsStore for InMemoryModelsStore {
    fn read(&self, provider_id: &str) -> Option<ModelsStoreEntry> {
        self.entries.lock().unwrap().get(provider_id).cloned()
    }

    fn write(&self, provider_id: &str, entry: ModelsStoreEntry) {
        self.entries.lock().unwrap().insert(provider_id.to_string(), entry);
    }

    fn delete(&self, provider_id: &str) {
        self.entries.lock().unwrap().remove(provider_id);
    }
}


