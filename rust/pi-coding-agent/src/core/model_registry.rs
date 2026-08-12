//! Model registry compatibility facade, port of `core/model-registry.ts`.
//! Exposed to extensions; coding-agent internals use ModelRuntime directly.

use pi_ai::types::Model;

use super::model_runtime::{ModelRuntime, ResolvedModelAuth};
use super::provider_composer::{resolve_compatibility_request_config, AuthStatus};

#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedRequestAuth {
    Ok {
        api_key: Option<String>,
        headers: Vec<(String, Option<String>)>,
        base_url: Option<String>,
        env: Option<Vec<(String, String)>>,
    },
    Err { error: String },
}

pub use super::provider_composer::clear_api_key_cache;

/// Synchronous compatibility facade for extensions.
pub struct ModelRegistry {
    runtime: ModelRuntime,
}

impl ModelRegistry {
    pub fn new(runtime: ModelRuntime) -> Self {
        Self { runtime }
    }

    /// Reload models.json.
    pub fn refresh(&mut self) {
        self.runtime.refresh();
    }

    pub fn get_error(&self) -> Option<String> {
        self.runtime.get_error()
    }

    pub fn get_all(&self) -> Vec<Model> {
        self.runtime.get_models(None)
    }

    pub fn get_available(&self) -> Vec<Model> {
        self.runtime.get_available_snapshot()
    }

    pub fn find(&self, provider: &str, model_id: &str) -> Option<Model> {
        self.runtime.get_model(provider, model_id)
    }

    pub fn has_configured_auth(&self, model: &Model) -> bool {
        self.runtime.has_configured_auth(&model.provider)
    }

    /// Resolve api key and headers for a model request.
    pub fn get_api_key_and_headers(&self, model: &Model) -> ResolvedRequestAuth {
        match self.runtime.get_auth(model) {
            Some(ResolvedModelAuth {
                api_key,
                headers,
                base_url,
            }) => ResolvedRequestAuth::Ok {
                api_key,
                headers: headers
                    .iter()
                    .map(|(key, value)| (key.clone(), Some(value.clone())))
                    .collect(),
                base_url,
                env: None,
            },
            None => {
                // No credential: fall back to configured headers only.
                let config = self.runtime.models_json_provider(&model.provider);
                let model_json = crate::core::model_json::model_to_json(model);
                match resolve_compatibility_request_config(&model_json, config) {
                    Ok(compatibility) if compatibility.auth_header => ResolvedRequestAuth::Err {
                        error: format!("No API key found for \"{}\"", model.provider),
                    },
                    Ok(compatibility) => ResolvedRequestAuth::Ok {
                        api_key: None,
                        headers: compatibility.headers.unwrap_or_default(),
                        base_url: None,
                        env: None,
                    },
                    Err(error) => ResolvedRequestAuth::Err { error },
                }
            }
        }
    }

    pub fn get_provider_auth_status(&self, provider: &str) -> AuthStatus {
        self.runtime.get_provider_auth_status(provider)
    }

    pub fn get_provider_display_name(&self, provider: &str) -> String {
        self.runtime
            .get_model(provider, "")
            .map(|_| provider.to_string())
            .unwrap_or_else(|| provider.to_string())
    }

    pub fn get_api_key_for_provider(&self, provider: &str) -> Option<String> {
        let model = self.runtime.get_models(Some(provider)).first()?.clone();
        self.runtime.get_auth(&model).and_then(|auth| auth.api_key)
    }

    pub fn is_using_oauth(&self, model: &Model) -> bool {
        self.runtime.is_using_oauth(&model.provider)
    }

    pub fn register_provider(&mut self, provider_name: &str, config: pi_protocol::Value) {
        self.runtime.register_provider(provider_name, config);
    }

    pub fn unregister_provider(&mut self, provider_name: &str) {
        self.runtime.unregister_provider(provider_name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_reads_runtime_models() {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-registry-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let models_path = dir.join("models.json");
        std::fs::write(
            &models_path,
            r#"{"providers":{"acme":{"baseUrl":"https://a.example","models":[{"id":"m1","api":"openai"}]}}}"#,
        )
        .unwrap();
        let runtime = ModelRuntime::create(super::super::model_runtime::CreateModelRuntimeOptions {
            auth_path: Some(dir.join("auth.json").to_string_lossy().to_string()),
            models_path: Some(models_path.to_string_lossy().to_string()),
            models_store_path: None,
        });
        let registry = ModelRegistry::new(runtime);
        assert_eq!(registry.get_all().len(), 1);
        assert!(registry.find("acme", "m1").is_some());
        assert!(registry.find("acme", "nope").is_none());
        assert!(!registry.has_configured_auth(&registry.find("acme", "m1").unwrap()));
        assert!(registry.get_error().is_none());
    }

    #[test]
    fn get_api_key_without_auth() {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-registry2-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let models_path = dir.join("models.json");
        std::fs::write(
            &models_path,
            r#"{"providers":{"acme":{"baseUrl":"https://a.example","models":[{"id":"m1","api":"openai"}]}}}"#,
        )
        .unwrap();
        let runtime = ModelRuntime::create(super::super::model_runtime::CreateModelRuntimeOptions {
            auth_path: Some(dir.join("auth.json").to_string_lossy().to_string()),
            models_path: Some(models_path.to_string_lossy().to_string()),
            models_store_path: None,
        });
        let registry = ModelRegistry::new(runtime);
        let model = registry.find("acme", "m1").unwrap();
        // No auth configured and no authHeader: OK with no key.
        assert!(matches!(registry.get_api_key_and_headers(&model), ResolvedRequestAuth::Ok { .. }));
    }
}
