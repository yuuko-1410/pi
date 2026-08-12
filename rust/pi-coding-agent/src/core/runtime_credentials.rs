//! Runtime credentials overlay, port of `core/runtime-credentials.ts`.
//! Synchronous overlay on top of a `CredentialStore` for non-persistent
//! runtime API keys (the async store trait maps to sync calls).

use pi_ai::auth::{Credential, CredentialInfo, CredentialStore};

/// Async credential store overlay for non-persistent runtime API keys.
pub struct RuntimeCredentials {
    store: Box<dyn CredentialStore>,
    overrides: std::collections::HashMap<String, String>,
}

impl RuntimeCredentials {
    pub fn new(store: Box<dyn CredentialStore>) -> Self {
        Self {
            store,
            overrides: std::collections::HashMap::new(),
        }
    }

    pub fn set_runtime_api_key(&mut self, provider_id: &str, api_key: String) {
        self.overrides.insert(provider_id.to_string(), api_key);
    }

    pub fn remove_runtime_api_key(&mut self, provider_id: &str) {
        self.overrides.remove(provider_id);
    }

    pub fn has_runtime_api_key(&self, provider_id: &str) -> bool {
        self.overrides.contains_key(provider_id)
    }

    pub fn read(&self, provider_id: &str) -> Option<Credential> {
        match self.overrides.get(provider_id) {
            Some(key) => Some(Credential::ApiKey {
                key: Some(key.clone()),
                env: None,
            }),
            None => self.store.read(provider_id),
        }
    }

    pub fn list(&self) -> Vec<CredentialInfo> {
        let mut entries: std::collections::HashMap<String, CredentialInfo> = self
            .store
            .list()
            .into_iter()
            .map(|entry| (entry.provider_id.clone(), entry))
            .collect();
        for provider_id in self.overrides.keys() {
            entries.insert(
                provider_id.clone(),
                CredentialInfo {
                    provider_id: provider_id.clone(),
                    credential_type: "api_key".to_string(),
                },
            );
        }
        entries.into_values().collect()
    }

    pub fn modify(
        &self,
        provider_id: &str,
        update: Box<dyn FnOnce(Option<Credential>) -> Option<Credential> + Send>,
    ) -> Option<Credential> {
        self.store.modify(provider_id, update)
    }

    pub fn delete(&mut self, provider_id: &str) {
        self.store.delete(provider_id);
        self.overrides.remove(provider_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::auth::InMemoryCredentialStore;

    #[test]
    fn override_shadows_store() {
        let mut credentials = RuntimeCredentials::new(Box::new(InMemoryCredentialStore::new()));
        credentials.set_runtime_api_key("anthropic", "runtime-key".into());
        let credential = credentials.read("anthropic").unwrap();
        match credential {
            Credential::ApiKey { key, .. } => assert_eq!(key.as_deref(), Some("runtime-key")),
            _ => panic!("expected api key"),
        }
        assert!(credentials.has_runtime_api_key("anthropic"));
        credentials.remove_runtime_api_key("anthropic");
        assert_eq!(credentials.read("anthropic"), None);
    }

    #[test]
    fn list_merges_overrides() {
        let store = InMemoryCredentialStore::new();
        store.modify(
            "openai",
            Box::new(|_| {
                Some(Credential::ApiKey {
                    key: Some("stored".into()),
                    env: None,
                })
            }),
        );
        let mut credentials = RuntimeCredentials::new(Box::new(store));
        credentials.set_runtime_api_key("anthropic", "runtime".into());
        let list = credentials.list();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|e| e.provider_id == "anthropic"));
        assert!(list.iter().any(|e| e.provider_id == "openai"));
    }

    #[test]
    fn delete_removes_override() {
        let mut credentials = RuntimeCredentials::new(Box::new(InMemoryCredentialStore::new()));
        credentials.set_runtime_api_key("anthropic", "runtime".into());
        credentials.delete("anthropic");
        assert!(!credentials.has_runtime_api_key("anthropic"));
    }
}
