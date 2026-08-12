//! Model runtime, port of `core/model-runtime.ts`.
//!
//! ponytail: the JS layer wraps pi-ai's full Models/MutableModels object
//! (provider composition, availability refreshes, network catalogs). The
//! Rust runtime keeps the same public surface (model lookup, auth state,
//! runtime API keys, provider registration) over a simplified composition:
//! models come from the builtin catalog port plus models.json overlays, and
//! streaming goes through the agent StreamFn. Availability refresh and
//! remote catalogs are deferred; noted as differences.

use std::collections::{HashMap, HashSet};

use pi_ai::models::models_are_equal;
use pi_ai::types::Model;
use pi_protocol::Value;

use super::auth_storage::AuthStorage;
use super::model_config::ModelConfig;
use super::provider_composer::{
    compose_provider_models, configured_request_auth_status, AuthStatus,
};
use super::resolve_config_value::{resolve_config_value, resolve_config_value_with_env_like};
use super::runtime_credentials::RuntimeCredentials;

/// Credentials changed successfully, but the local snapshot could not be
/// synchronized (JS CredentialSynchronizationError).
pub struct CredentialSynchronizationError {
    pub provider_id: String,
    pub operation: String,
}

impl std::fmt::Display for CredentialSynchronizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Credential {} committed for {}, but local synchronization failed",
            self.operation, self.provider_id
        )
    }
}

impl std::fmt::Debug for CredentialSynchronizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialSynchronizationError")
            .field("provider_id", &self.provider_id)
            .field("operation", &self.operation)
            .finish()
    }
}

impl std::error::Error for CredentialSynchronizationError {}

#[derive(Clone, Debug, Default)]
pub struct CreateModelRuntimeOptions {
    pub auth_path: Option<String>,
    pub models_path: Option<String>,
    pub models_store_path: Option<String>,
}

pub struct ModelRuntime {
    credentials: RuntimeCredentials,
    config: ModelConfig,
    models_path: Option<String>,
    /// Provider id -> composed models (builtin + models.json overlays).
    providers: HashMap<String, Vec<Model>>,
    /// Provider id -> models.json config errors.
    composition_errors: HashMap<String, String>,
    configured_providers: HashSet<String>,
    stored_providers: HashSet<String>,
    /// Provider id -> (api_key resolved at last refresh, env) for runtime use.
    extension_providers: HashMap<String, Value>,
}

impl ModelRuntime {
    pub fn create(options: CreateModelRuntimeOptions) -> ModelRuntime {
        let auth_storage = AuthStorage::create(options.auth_path);
        let credentials = RuntimeCredentials::new(Box::new(auth_storage));

        let models_path = options.models_path.clone();
        let config = match &models_path {
            Some(path) => ModelConfig::load(Some(path)),
            None => ModelConfig::load(None),
        };

        let mut runtime = ModelRuntime {
            credentials,
            config,
            models_path,
            providers: HashMap::new(),
            composition_errors: HashMap::new(),
            configured_providers: HashSet::new(),
            stored_providers: HashSet::new(),
            extension_providers: HashMap::new(),
        };
        runtime.rebuild_providers();
        // Stored credentials and models.json apiKey configs mark providers
        // as configured (the JS availability refresh fills this asynchronously).
        for info in runtime.credentials.list() {
            runtime.stored_providers.insert(info.provider_id.clone());
        }
        for provider_id in runtime.provider_ids() {
            runtime.refresh_configured(&provider_id);
        }
        runtime
    }

    fn provider_ids(&self) -> HashSet<String> {
        let mut ids: HashSet<String> = self.providers.keys().cloned().collect();
        for provider_id in self.config.get_provider_ids() {
            ids.insert(provider_id);
        }
        for provider_id in self.extension_providers.keys() {
            ids.insert(provider_id.clone());
        }
        ids
    }

    fn recompose_provider(&mut self, provider_id: &str) {
        // Builtin catalog models are empty until generate-models runs; the
        // composition starts from models.json providers only.
        let base_models: Vec<Value> = Vec::new();
        let extension = self.extension_providers.get(provider_id).cloned();
        match compose_provider_models(provider_id, &base_models, &self.config, extension.as_ref()) {
            Ok(models) => {
                let parsed: Vec<Model> = models
                    .iter()
                    .filter_map(|value| pi_ai::model_catalog::model_from_json(value).ok())
                    .collect();
                self.providers.insert(provider_id.to_string(), parsed);
                self.composition_errors.remove(provider_id);
            }
            Err(error) => {
                self.composition_errors.insert(provider_id.to_string(), error);
                self.providers.remove(provider_id);
            }
        }
    }

    fn rebuild_providers(&mut self) {
        self.providers.clear();
        self.composition_errors.clear();
        for provider_id in self.provider_ids() {
            self.recompose_provider(&provider_id);
        }
    }

    pub fn get_providers(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    pub fn get_models(&self, provider_id: Option<&str>) -> Vec<Model> {
        match provider_id {
            Some(provider_id) => self.providers.get(provider_id).cloned().unwrap_or_default(),
            None => self.providers.values().flatten().cloned().collect(),
        }
    }

    pub fn get_model(&self, provider_id: &str, model_id: &str) -> Option<Model> {
        self.providers.get(provider_id)?.iter().find(|model| model.id == model_id).cloned()
    }

    pub fn get_available_snapshot(&self) -> Vec<Model> {
        self.get_models(None)
            .into_iter()
            .filter(|model| self.configured_providers.contains(&model.provider))
            .collect()
    }

    pub fn get_error(&self) -> Option<String> {
        let mut errors: Vec<String> = Vec::new();
        if let Some(config_error) = self.config.get_error() {
            errors.push(config_error.to_string());
        }
        for (provider_id, error) in &self.composition_errors {
            errors.push(format!("Provider \"{provider_id}\": {error}"));
        }
        if errors.is_empty() {
            None
        } else {
            Some(errors.join("\n\n"))
        }
    }

    pub fn has_configured_auth(&self, provider_id: &str) -> bool {
        self.configured_providers.contains(provider_id)
    }

    pub fn is_using_oauth(&self, provider_id: &str) -> bool {
        // OAuth resolution is deferred in the Rust runtime (no browser
        // flows); a provider whose models.json config sets oauth: "radius"
        // is treated as OAuth-based.
        self.config
            .get_provider(provider_id)
            .is_some_and(|provider| provider.oauth.is_some())
    }

    pub fn get_provider_auth_status(&self, provider_id: &str) -> AuthStatus {
        if self.credentials.has_runtime_api_key(provider_id) {
            return AuthStatus {
                configured: true,
                source: Some("runtime".to_string()),
                label: None,
            };
        }
        if self.stored_providers.contains(provider_id) {
            return AuthStatus {
                configured: true,
                source: Some("stored".to_string()),
                label: None,
            };
        }
        if let Some(status) = configured_request_auth_status(self.config.get_provider(provider_id)) {
            return status;
        }
        AuthStatus {
            configured: false,
            source: None,
            label: None,
        }
    }

    /// Resolve request auth for a model: stored/runtime credential, then
    /// models.json apiKey config, then environment.
    pub fn get_auth(&self, model: &Model) -> Option<ResolvedModelAuth> {
        // 1. Runtime override.
        if self.credentials.has_runtime_api_key(&model.provider) {
            if let Some(pi_ai::auth::Credential::ApiKey { key, .. }) = self.credentials.read(&model.provider) {
                if let Some(key) = key {
                    return Some(ResolvedModelAuth {
                        api_key: Some(key),
                        headers: Vec::new(),
                        base_url: None,
                    });
                }
            }
        }
        // 2. Stored credential.
        if let Some(pi_ai::auth::Credential::ApiKey { key, env }) = self.credentials.read(&model.provider) {
            if let Some(key) = key {
                let resolved = resolve_config_value_with_env_like(&key, env.as_ref());
                return Some(ResolvedModelAuth {
                    api_key: resolved,
                    headers: Vec::new(),
                    base_url: None,
                });
            }
        }
        // 3. models.json apiKey config.
        if let Some(provider_config) = self.config.get_provider(&model.provider) {
            if let Some(api_key) = &provider_config.api_key {
                let resolved = resolve_config_value(api_key, None);
                if resolved.is_some() {
                    return Some(ResolvedModelAuth {
                        api_key: resolved,
                        headers: Vec::new(),
                        base_url: None,
                    });
                }
            }
        }
        // 4. Provider environment variables (fall back to bare env check).
        let env_vars = provider_env_vars(&model.provider);
        for name in env_vars {
            if let Ok(value) = std::env::var(&name) {
                if !value.is_empty() {
                    return Some(ResolvedModelAuth {
                        api_key: Some(value),
                        headers: Vec::new(),
                        base_url: None,
                    });
                }
            }
        }
        None
    }

    pub fn set_runtime_api_key(&mut self, provider_id: &str, api_key: String) {
        self.credentials.set_runtime_api_key(provider_id, api_key);
        self.configured_providers.insert(provider_id.to_string());
    }

    pub fn remove_runtime_api_key(&mut self, provider_id: &str) {
        self.credentials.remove_runtime_api_key(provider_id);
        self.refresh_configured(provider_id);
    }

    pub fn list_credentials(&self) -> Vec<pi_ai::auth::CredentialInfo> {
        self.credentials.list()
    }

    fn refresh_configured(&mut self, provider_id: &str) {
        let has_auth = self.credentials.read(provider_id).is_some()
            || self.credentials.has_runtime_api_key(provider_id)
            || configured_request_auth_status(self.config.get_provider(provider_id))
                .is_some_and(|status| status.configured);
        if has_auth {
            self.configured_providers.insert(provider_id.to_string());
            self.stored_providers.insert(provider_id.to_string());
        } else {
            self.configured_providers.remove(provider_id);
            self.stored_providers.remove(provider_id);
        }
    }

    /// Reload models.json and recompose providers (sync refresh).
    pub fn refresh(&mut self) {
        self.config = match &self.models_path {
            Some(path) => ModelConfig::load(Some(path)),
            None => ModelConfig::load(None),
        };
        self.rebuild_providers();
        for provider_id in self.provider_ids() {
            self.refresh_configured(&provider_id);
        }
    }

    pub fn register_provider(&mut self, provider_id: &str, config: Value) {
        self.extension_providers.insert(provider_id.to_string(), config);
        self.recompose_provider(provider_id);
        self.refresh_configured(provider_id);
    }

    pub fn unregister_provider(&mut self, provider_id: &str) {
        self.extension_providers.remove(provider_id);
        self.recompose_provider(provider_id);
        self.refresh_configured(provider_id);
    }

    /// Model equality helper (id + provider).
    pub fn models_equal(a: Option<&Model>, b: Option<&Model>) -> bool {
        models_are_equal(a, b)
    }
}

impl super::model_resolver::ModelRuntimeLike for ModelRuntime {
    fn get_models(&self) -> Vec<Model> {
        ModelRuntime::get_models(self, None)
    }
    fn get_available_snapshot(&self) -> Vec<Model> {
        ModelRuntime::get_available_snapshot(self)
    }
    fn get_model(&self, provider: &str, model_id: &str) -> Option<Model> {
        ModelRuntime::get_model(self, provider, model_id)
    }
    fn has_configured_auth(&self, provider: &str) -> bool {
        ModelRuntime::has_configured_auth(self, provider)
    }
}

/// Resolved request auth (apiKey/headers/baseUrl), mirroring the JS AuthResult.auth.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolvedModelAuth {
    pub api_key: Option<String>,
    pub headers: Vec<(String, String)>,
    pub base_url: Option<String>,
}

/// Provider-scoped env var names consulted when no credential/config exists.
fn provider_env_vars(provider: &str) -> Vec<String> {
    // Common provider env vars (port of the builtin provider apiKey env lists).
    let map: &[(&str, &[&str])] = &[
        ("anthropic", &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY", "ANTHROPIC_API_KEY_ENV"]),
        ("openai", &["OPENAI_API_KEY"]),
        ("google", &["GOOGLE_API_KEY"]),
        ("google-vertex", &["GOOGLE_ACCESS_TOKEN"]),
        ("openrouter", &["OPENROUTER_API_KEY"]),
        ("deepseek", &["DEEPSEEK_API_KEY"]),
        ("xai", &["XAI_API_KEY"]),
        ("groq", &["GROQ_API_KEY"]),
        ("mistral", &["MISTRAL_API_KEY"]),
        ("zai", &["ZAI_API_KEY"]),
        ("moonshotai", &["MOONSHOT_API_KEY", "KIMI_API_KEY"]),
        ("qwen-token-plan", &["QWEN_API_KEY", "DASHSCOPE_API_KEY"]),
        ("qwen-token-plan-cn", &["QWEN_API_KEY", "DASHSCOPE_API_KEY"]),
        ("qwen-token-plan-individual", &["QWEN_API_KEY", "DASHSCOPE_API_KEY"]),
        ("github-copilot", &["GITHUB_COPILOT_TOKEN"]),
        ("nvidia", &["NVIDIA_API_KEY"]),
        ("together", &["TOGETHER_API_KEY"]),
        ("fireworks", &["FIREWORKS_API_KEY"]),
        ("cerebras", &["CEREBRAS_API_KEY"]),
        ("minimax", &["MINIMAX_API_KEY"]),
        ("huggingface", &["HF_TOKEN", "HUGGINGFACE_API_KEY"]),
        ("baseten", &["BASETEN_API_KEY"]),
        ("opencode", &["OPENCODE_API_KEY"]),
        ("opencode-go", &["OPENCODE_API_KEY"]),
        ("kimi-coding", &["KIMI_API_KEY"]),
        ("xiaomi", &["XIAOMI_API_KEY"]),
    ];
    map.iter()
        .find(|(name, _)| *name == provider)
        .map(|(_, vars)| vars.iter().map(|value| value.to_string()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_auth_path() -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-mrt-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("auth.json").to_string_lossy().to_string()
    }

    fn temp_models(content: &str) -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("pi-mrt-models-{}-{n}.json", std::process::id()));
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().to_string()
    }

    #[test]
    fn loads_models_from_config() {
        let models_path = temp_models(
            r#"{"providers":{"acme":{"baseUrl":"https://a.example","models":[{"id":"m1","name":"M1","api":"openai","contextWindow":1000}]}}}"#,
        );
        let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            auth_path: Some(temp_auth_path()),
            models_path: Some(models_path),
            models_store_path: None,
        });
        assert!(runtime.get_error().is_none());
        let model = runtime.get_model("acme", "m1").unwrap();
        assert_eq!(model.id, "m1");
        assert_eq!(model.base_url, "https://a.example");
        assert!(!runtime.has_configured_auth("acme"));
    }

    #[test]
    fn runtime_api_key_configures_provider() {
        let models_path = temp_models(
            r#"{"providers":{"acme":{"baseUrl":"https://a.example","models":[{"id":"m1","api":"openai"}]}}}"#,
        );
        let mut runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            auth_path: Some(temp_auth_path()),
            models_path: Some(models_path),
            models_store_path: None,
        });
        runtime.set_runtime_api_key("acme", "rt-key".to_string());
        assert!(runtime.has_configured_auth("acme"));
        let model = runtime.get_model("acme", "m1").unwrap();
        let auth = runtime.get_auth(&model).unwrap();
        assert_eq!(auth.api_key.as_deref(), Some("rt-key"));
        assert_eq!(runtime.get_provider_auth_status("acme").source.as_deref(), Some("runtime"));
        runtime.remove_runtime_api_key("acme");
        assert!(!runtime.has_configured_auth("acme"));
    }

    #[test]
    fn stored_credential_resolves() {
        use pi_ai::auth::CredentialStore;
        let models_path = temp_models(
            r#"{"providers":{"acme":{"baseUrl":"https://a.example","models":[{"id":"m1","api":"openai"}]}}}"#,
        );
        let auth_path = temp_auth_path();
        let storage = AuthStorage::create(Some(auth_path.clone()));
        storage.modify("acme", Box::new(|_| {
            Some(pi_ai::auth::Credential::ApiKey {
                key: Some("stored-key".into()),
                env: None,
            })
        }));
        drop(storage);

        let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            auth_path: Some(auth_path),
            models_path: Some(models_path),
            models_store_path: None,
        });
        assert!(runtime.has_configured_auth("acme"));
        let model = runtime.get_model("acme", "m1").unwrap();
        let auth = runtime.get_auth(&model).unwrap();
        assert_eq!(auth.api_key.as_deref(), Some("stored-key"));
    }

    #[test]
    fn config_command_key_resolves() {
        let models_path = temp_models(
            r#"{"providers":{"acme":{"baseUrl":"https://a.example","apiKey":"!echo config-key","models":[{"id":"m1","api":"openai"}]}}}"#,
        );
        let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            auth_path: Some(temp_auth_path()),
            models_path: Some(models_path),
            models_store_path: None,
        });
        assert!(runtime.has_configured_auth("acme"));
        let model = runtime.get_model("acme", "m1").unwrap();
        let auth = runtime.get_auth(&model).unwrap();
        assert_eq!(auth.api_key.as_deref(), Some("config-key"));
    }

    #[test]
    fn refresh_reloads_config() {
        let models_path = temp_models(r#"{"providers":{}}"#);
        let mut runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            auth_path: Some(temp_auth_path()),
            models_path: Some(models_path.clone()),
            models_store_path: None,
        });
        assert!(runtime.get_model("acme", "m1").is_none());
        std::fs::write(
            &models_path,
            r#"{"providers":{"acme":{"baseUrl":"https://a.example","models":[{"id":"m1","api":"openai"}]}}}"#,
        )
        .unwrap();
        runtime.refresh();
        assert!(runtime.get_model("acme", "m1").is_some());
    }

    #[test]
    fn errors_surface_composition_failures() {
        let models_path = temp_models(
            r#"{"providers":{"acme":{"models":[{"id":"m1","name":"M"}]}}}"#,
        );
        let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            auth_path: Some(temp_auth_path()),
            models_path: Some(models_path),
            models_store_path: None,
        });
        let error = runtime.get_error().unwrap();
        assert!(error.contains("Provider \"acme\""));
    }
}
