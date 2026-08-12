//! CredentialStore implementation backed by auth.json, port of
//! `core/auth-storage.ts`.
//!
//! ponytail: the JS proper-lockfile cross-process file lock is replaced by a
//! process-wide mutex (pi is a single-process CLI; the extension runtime
//! shares this process). Multi-process writers could interleave; add a real
//! file lock when a second pi process ever writes auth.json concurrently.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use pi_ai::auth::{Credential, CredentialInfo, CredentialStore};
use pi_protocol::Value;

use super::resolve_config_value::{is_command_config_value, resolve_config_value};

type AuthStorageData = HashMap<String, Credential>;

pub struct LockResult<T> {
    pub result: T,
    pub next: Option<String>,
}

/// Backend abstraction over the auth file (file or in-memory).
pub enum AuthStorageBackend {
    File(FileAuthStorageBackend),
    InMemory(InMemoryAuthStorageBackend),
}

impl AuthStorageBackend {
    pub fn with_lock<T>(&self, update: &mut dyn FnMut(Option<&str>) -> LockResult<T>) -> T {
        match self {
            AuthStorageBackend::File(backend) => backend.with_lock(update),
            AuthStorageBackend::InMemory(backend) => backend.with_lock(update),
        }
    }
}

/// Process-wide serialization for file writes.
static AUTH_FILE_MUTEX: Mutex<()> = Mutex::new(());

/// File revision string, port of getFileRevision (dev:ino:size:mtimeNs:ctimeNs).
pub fn get_file_revision(path: &str) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(path).ok()?;
        Some(format!(
            "{}:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime_nsec(),
            metadata.ctime_nsec()
        ))
    }
    #[cfg(not(unix))]
    {
        let metadata = fs::metadata(path).ok()?;
        let modified = metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some(format!("0:0:{}:{}:0", metadata.len(), modified.as_nanos()))
    }
}

/// Resolve a config value with provider-scoped env (ProviderEnv as a map).
fn resolve_config_value_with_env(config: &str, env: &Option<Vec<(String, String)>>) -> Option<String> {
    let env_map = env.as_ref().map(|entries| {
        let mut map = HashMap::new();
        for (key, value) in entries {
            map.insert(key.clone(), value.clone());
        }
        map
    });
    resolve_config_value(config, env_map.as_ref())
}

fn default_auth_path() -> String {
    crate::config::get_agent_dir() + "/auth.json"
}

fn ensure_parent_dir(path: &str) {
    if let Some(parent) = Path::new(path).parent() {
        let _ = fs::create_dir_all(parent);
    }
}

fn ensure_file_exists(path: &str) {
    if !Path::new(path).exists() {
        let _ = fs::write(path, "{}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }
    }
}

pub struct FileAuthStorageBackend {
    auth_path: String,
}

impl FileAuthStorageBackend {
    pub fn new(auth_path: Option<String>) -> Self {
        Self {
            auth_path: auth_path.unwrap_or_else(default_auth_path),
        }
    }
}

impl FileAuthStorageBackend {
    fn with_lock<T>(&self, update: &mut dyn FnMut(Option<&str>) -> LockResult<T>) -> T {
        let _guard = AUTH_FILE_MUTEX.lock().unwrap();
        ensure_parent_dir(&self.auth_path);
        ensure_file_exists(&self.auth_path);

        let current = fs::read_to_string(&self.auth_path).ok();
        let LockResult { result, next } = update(current.as_deref());
        if let Some(next) = next {
            let mut file = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&self.auth_path)
                .expect("open auth.json for write");
            let _ = file.write_all(next.as_bytes());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&self.auth_path, fs::Permissions::from_mode(0o600));
            }
        }
        result
    }
}

pub struct InMemoryAuthStorageBackend {
    value: Mutex<Option<String>>,
}

impl InMemoryAuthStorageBackend {
    pub fn new() -> Self {
        Self {
            value: Mutex::new(None),
        }
    }
}

impl Default for InMemoryAuthStorageBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryAuthStorageBackend {
    fn with_lock<T>(&self, update: &mut dyn FnMut(Option<&str>) -> LockResult<T>) -> T {
        let mut value = self.value.lock().unwrap();
        let LockResult { result, next } = update(value.as_deref());
        if let Some(next) = next {
            *value = Some(next);
        }
        result
    }
}

// ---------------------------------------------------------------------------
// Credential JSON
// ---------------------------------------------------------------------------

fn kv(key: &str, value: Value) -> (String, Value) {
    (key.to_string(), value)
}

fn credential_to_json(credential: &Credential) -> Value {
    match credential {
        Credential::ApiKey { key, env } => {
            let mut entries = vec![kv("type", Value::String("api_key".to_string()))];
            if let Some(key) = key {
                entries.push(kv("key", Value::String(key.clone())));
            }
            if let Some(env) = env {
                entries.push((
                    "env".to_string(),
                    Value::Map(
                        env.iter()
                            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                            .collect(),
                    ),
                ));
            }
            Value::Map(entries)
        }
        Credential::OAuth {
            refresh,
            access,
            expires,
            extra,
        } => {
            let mut entries = vec![
                kv("type", Value::String("oauth".to_string())),
                kv("access", Value::String(access.clone())),
                kv("refresh", Value::String(refresh.clone())),
                kv("expires", Value::Number(*expires)),
            ];
            for (key, value) in extra {
                entries.push((key.clone(), value.clone()));
            }
            Value::Map(entries)
        }
    }
}

fn json_to_credential(value: &Value) -> Option<Credential> {
    let entries: Vec<(String, Value)> = value.as_map()?.to_vec();
    let type_name = entries
        .iter()
        .find(|(k, _)| k == "type")
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("");
    match type_name {
        "api_key" => {
            let key = entries
                .iter()
                .find(|(k, _)| k == "key")
                .and_then(|(_, v)| v.as_str())
                .map(|value| value.to_string());
            let env = entries
                .iter()
                .find(|(k, _)| k == "env")
                .and_then(|(_, v)| v.as_map())
                .map(|map| {
                    map.iter()
                        .filter_map(|(k, v)| v.as_str().map(|value| (k.clone(), value.to_string())))
                        .collect::<Vec<(String, String)>>()
                })
                .filter(|map| !map.is_empty());
            Some(Credential::ApiKey { key, env })
        }
        "oauth" => {
            let access = entries
                .iter()
                .find(|(k, _)| k == "access")
                .and_then(|(_, v)| v.as_str())?
                .to_string();
            let refresh = entries
                .iter()
                .find(|(k, _)| k == "refresh")
                .and_then(|(_, v)| v.as_str())?
                .to_string();
            let expires = entries
                .iter()
                .find(|(k, _)| k == "expires")
                .and_then(|(_, v)| v.as_number())?;
            let extra: Vec<(String, Value)> = entries
                .into_iter()
                .filter(|(k, _)| k != "type" && k != "access" && k != "refresh" && k != "expires")
                .collect();
            Some(Credential::OAuth {
                refresh,
                access,
                expires,
                extra,
            })
        }
        _ => None,
    }
}

fn parse_storage_data(content: Option<&str>) -> AuthStorageData {
    let mut data = AuthStorageData::new();
    let Some(content) = content else {
        return data;
    };
    let Ok(value) = pi_ai::utils::json::parse_json_with_repair::<Value>(content) else {
        return data;
    };
    if let Some(entries) = value.as_map() {
        for (provider_id, credential) in entries {
            if let Some(credential) = json_to_credential(credential) {
                data.insert(provider_id.clone(), credential);
            }
        }
    }
    data
}

fn serialize_data(data: &AuthStorageData) -> String {
    let entries: Vec<(String, Value)> = data
        .iter()
        .map(|(provider_id, credential)| (provider_id.clone(), credential_to_json(credential)))
        .collect();
    pi_ai::utils::json::json_stringify_pretty(&Value::Map(entries))
}

// ---------------------------------------------------------------------------
// ReadOnlyAuthStorage
// ---------------------------------------------------------------------------

/// Credential store that reads auth.json without modifying it.
pub struct ReadOnlyAuthStorage {
    auth_path: String,
    data: Option<AuthStorageData>,
}

impl ReadOnlyAuthStorage {
    pub fn new(auth_path: Option<String>) -> Self {
        Self {
            auth_path: auth_path.unwrap_or_else(default_auth_path),
            data: None,
        }
    }

    fn load(&mut self) -> Result<&AuthStorageData, String> {
        if self.data.is_some() {
            return Ok(self.data.as_ref().unwrap());
        }
        let content = match fs::read_to_string(&self.auth_path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.data = Some(AuthStorageData::new());
                return Ok(self.data.as_ref().unwrap());
            }
            Err(error) => {
                return Err(format!("Failed to read auth.json: {error}"));
            }
        };
        let parsed: Value = pi_ai::utils::json::parse_json_with_repair(&content)
            .map_err(|error| format!("Failed to read auth.json: {error}"))?;
        let entries = parsed.as_map().ok_or_else(|| "Invalid auth.json: expected an object".to_string())?;
        let mut data = AuthStorageData::new();
        for (provider_id, credential) in entries {
            let Some(credential) = json_to_credential(credential) else {
                return Err(format!("Invalid auth.json credential for provider \"{provider_id}\""));
            };
            data.insert(provider_id.clone(), credential);
        }
        self.data = Some(data);
        Ok(self.data.as_ref().unwrap())
    }
}

impl CredentialStore for ReadOnlyAuthStorage {
    fn read(&self, provider_id: &str) -> Option<Credential> {
        let mut this = self.clone_inner();
        let credential = this.load().ok()?.get(provider_id)?.clone();
        match &credential {
            Credential::ApiKey { key, env } => {
                let Some(key) = key.clone() else {
                    return Some(credential);
                };
                // Command-configured keys are returned verbatim (JS read-only
                // storage semantics); template keys are resolved.
                if is_command_config_value(&key) {
                    return Some(credential);
                }
                let resolved = resolve_config_value_with_env(&key, env);
                Some(Credential::ApiKey {
                    key: resolved,
                    env: env.clone(),
                })
            }
            _ => Some(credential),
        }
    }

    fn list(&self) -> Vec<CredentialInfo> {
        let mut this = self.clone_inner();
        let data = this.load().ok();
        data.map(|data| {
            data.iter()
                .map(|(provider_id, credential)| CredentialInfo {
                    provider_id: provider_id.clone(),
                    credential_type: credential.type_name().to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
    }

    fn modify(
        &self,
        _provider_id: &str,
        _update: Box<dyn FnOnce(Option<Credential>) -> Option<Credential> + Send>,
    ) -> Option<Credential> {
        panic!("Read-only credential storage cannot modify auth.json");
    }

    fn delete(&self, _provider_id: &str) {
        panic!("Read-only credential storage cannot modify auth.json");
    }
}

impl ReadOnlyAuthStorage {
    fn clone_inner(&self) -> Self {
        Self {
            auth_path: self.auth_path.clone(),
            data: None,
        }
    }
}

// ---------------------------------------------------------------------------
// AuthStorage
// ---------------------------------------------------------------------------

struct AuthFileReadState {
    data: AuthStorageData,
    revision: Option<String>,
}

/// Credential storage backed by a JSON file.
pub struct AuthStorage {
    storage: AuthStorageBackend,
    auth_path: Option<String>,
    read_state: Mutex<AuthFileReadState>,
}

impl AuthStorage {
    fn new(storage: AuthStorageBackend, auth_path: Option<String>) -> Self {
        let storage_instance = Self {
            storage,
            auth_path,
            read_state: Mutex::new(AuthFileReadState {
                data: AuthStorageData::new(),
                revision: None,
            }),
        };
        storage_instance.reload();
        storage_instance
    }

    pub fn create(auth_path: Option<String>) -> AuthStorage {
        let auth_path = auth_path.unwrap_or_else(default_auth_path);
        Self::new(
            AuthStorageBackend::File(FileAuthStorageBackend::new(Some(auth_path.clone()))),
            Some(auth_path),
        )
    }

    pub fn from_storage(storage: AuthStorageBackend) -> AuthStorage {
        Self::new(storage, None)
    }

    pub fn in_memory(data: AuthStorageData) -> AuthStorage {
        let storage = InMemoryAuthStorageBackend::new();
        {
            let serialized = serialize_data(&data);
            let mut update = |_: Option<&str>| LockResult {
                result: (),
                next: Some(serialized.clone()),
            };
            storage.with_lock(&mut update);
        }
        Self::from_storage(AuthStorageBackend::InMemory(storage))
    }

    fn update_read_state(&self, data: AuthStorageData, revision: Option<String>) {
        let mut state = self.read_state.lock().unwrap();
        state.data = data;
        state.revision = revision;
    }

    fn write_state(&self) -> &Self {
        self
    }

    /// Reload credentials from storage.
    pub fn reload(&self) {
        let mut content: Option<String> = None;
        let revision = self.auth_path.as_deref().and_then(get_file_revision);
        {
            let mut update = |current: Option<&str>| {
                content = current.map(|value| value.to_string());
                LockResult {
                    result: (),
                    next: None,
                }
            };
            self.storage.with_lock(&mut update);
        }
        self.update_read_state(parse_storage_data(content.as_deref()), revision);
    }

    fn read_latest_data(&self) -> AuthStorageData {
        if let Some(auth_path) = &self.auth_path {
            if let Some(revision) = get_file_revision(auth_path) {
                let state = self.read_state.lock().unwrap();
                if state.revision.as_deref() == Some(revision.as_str()) {
                    return state.data.clone();
                }
            }
            // Revision changed (or unavailable): reload, mirroring the JS
            // shared-reload path without its concurrency fan-out.
            self.reload();
            return self.read_state.lock().unwrap().data.clone();
        }
        // In-memory backend: always reload from the backend value.
        let mut content: Option<String> = None;
        let mut update = |current: Option<&str>| {
            content = current.map(|value| value.to_string());
            LockResult {
                result: (),
                next: None,
            }
        };
        self.storage.with_lock(&mut update);
        parse_storage_data(content.as_deref())
    }
}

impl CredentialStore for AuthStorage {
    fn read(&self, provider_id: &str) -> Option<Credential> {
        let credential = self.read_latest_data().get(provider_id)?.clone();
        match &credential {
            Credential::ApiKey { key, env } => {
                let key = key.clone()?;
                Some(Credential::ApiKey {
                    key: resolve_config_value_with_env(&key, env),
                    env: env.clone(),
                })
            }
            _ => Some(credential),
        }
    }

    fn list(&self) -> Vec<CredentialInfo> {
        self.read_latest_data()
            .iter()
            .map(|(provider_id, credential)| CredentialInfo {
                provider_id: provider_id.clone(),
                credential_type: credential.type_name().to_string(),
            })
            .collect()
    }

    fn modify(
        &self,
        provider_id: &str,
        update: Box<dyn FnOnce(Option<Credential>) -> Option<Credential> + Send>,
    ) -> Option<Credential> {
        let (mut latest_data, revision) = {
            let state = self.read_state.lock().unwrap();
            (state.data.clone(), state.revision.clone())
        };
        let mut update = Some(update);
        let mut update_fn = |current: Option<&str>| {
            let current_data = parse_storage_data(current);
            let previous = current_data.get(provider_id).cloned();
            let next = update.take().unwrap()(previous.clone());
            if next.is_none() {
                latest_data = current_data;
                return LockResult {
                    result: previous,
                    next: None,
                };
            }
            let mut merged = current_data;
            merged.insert(provider_id.to_string(), next.clone().unwrap());
            latest_data = merged.clone();
            LockResult {
                result: next,
                next: Some(serialize_data(&merged)),
            }
        };
        let result = self.storage.with_lock(&mut update_fn);
        self.write_state().update_read_state(latest_data, revision);
        result
    }

    fn delete(&self, provider_id: &str) {
        let mut latest_data = self.read_state.lock().unwrap().data.clone();
        let mut update = |current: Option<&str>| {
            let mut current_data = parse_storage_data(current);
            current_data.remove(provider_id);
            latest_data = current_data.clone();
            LockResult {
                result: (),
                next: Some(serialize_data(&current_data)),
            }
        };
        self.storage.with_lock(&mut update);
        self.write_state().update_read_state(latest_data, None);
    }
}

/// One-off synchronous read of a stored credential from an auth.json file
/// without resolving configured key values.
pub fn read_stored_credential(provider_id: &str, auth_path: Option<String>) -> Option<Credential> {
    let auth_path = auth_path.unwrap_or_else(default_auth_path);
    let content = fs::read_to_string(auth_path).ok()?;
    let value: Value = pi_ai::utils::json::parse_json_with_repair(&content).ok()?;
    let entries = value.as_map()?;
    let (_, credential) = entries.iter().find(|(key, _)| key == provider_id)?;
    json_to_credential(credential)
}

#[cfg(test)]
mod tests {
    use super::*;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn temp_auth_path() -> String {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-auth-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("auth.json").to_string_lossy().to_string()
    }

    fn api_key(key: &str) -> Credential {
        Credential::ApiKey {
            key: Some(key.to_string()),
            env: None,
        }
    }

    #[test]
    fn read_modify_delete_cycle() {
        let path = temp_auth_path();
        let storage = AuthStorage::create(Some(path.clone()));
        assert_eq!(storage.read("anthropic"), None);

        let credential = storage.modify(
            "anthropic",
            Box::new(|current| {
                assert_eq!(current, None);
                Some(api_key("sk-test"))
            }),
        );
        assert!(matches!(credential, Some(Credential::ApiKey { .. })));
        assert_eq!(
            storage.read("anthropic"),
            Some(api_key("sk-test"))
        );

        let list = storage.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].provider_id, "anthropic");
        assert_eq!(list[0].credential_type, "api_key");

        storage.delete("anthropic");
        assert_eq!(storage.read("anthropic"), None);
        assert!(storage.list().is_empty());
    }

    #[test]
    fn modify_replaces_existing() {
        let path = temp_auth_path();
        let storage = AuthStorage::create(Some(path));
        storage.modify("openai", Box::new(|_| Some(api_key("one"))));
        let result = storage.modify("openai", Box::new(|current| {
            assert_eq!(current, Some(api_key("one")));
            Some(api_key("two"))
        }));
        assert!(matches!(result, Some(Credential::ApiKey { key: Some(key), .. }) if key == "two"));
        assert_eq!(storage.read("openai"), Some(api_key("two")));
    }

    #[test]
    fn reload_picks_up_file_changes() {
        let path = temp_auth_path();
        let storage = AuthStorage::create(Some(path.clone()));
        // External write (same process, direct file write).
        let mut update = |_: Option<&str>| LockResult {
            result: (),
            next: Some("{\"google\": {\"type\": \"api_key\", \"key\": \"g-key\"}}".to_string()),
        };
        AuthStorageBackend::File(FileAuthStorageBackend::new(Some(path.clone()))).with_lock(&mut update);
        assert_eq!(storage.read("google"), Some(api_key("g-key")));
    }

    #[test]
    fn in_memory_storage() {
        let storage = AuthStorage::in_memory(AuthStorageData::new());
        storage.modify("x", Box::new(|_| Some(api_key("v"))));
        assert_eq!(storage.read("x"), Some(api_key("v")));
    }

    #[test]
    fn read_only_storage_resolves_config_commands() {
        let path = temp_auth_path();
        // Stored value that resolves via a shell command.
        let data: AuthStorageData = [(
            "provider".to_string(),
            Credential::ApiKey {
                key: Some("!echo resolved-key".to_string()),
                env: None,
            },
        )]
        .into();
        let storage = AuthStorage::create(Some(path.clone()));
        let credential = data.get("provider").cloned();
        storage.modify("provider", Box::new(move |_| credential));
        drop(storage);

        let read_only = ReadOnlyAuthStorage::new(Some(path));
        match read_only.read("provider") {
            Some(Credential::ApiKey { key, .. }) => {
                // Command-configured keys are returned verbatim by the
                // read-only store (JS semantics).
                assert_eq!(key.as_deref(), Some("!echo resolved-key"));
            }
            other => panic!("expected api key, got {other:?}"),
        }
    }

    #[test]
    fn read_stored_credential_raw() {
        let path = temp_auth_path();
        let storage = AuthStorage::create(Some(path.clone()));
        storage.modify("p", Box::new(|_| Some(api_key("raw"))));
        drop(storage);
        assert_eq!(read_stored_credential("p", Some(path.clone())), Some(api_key("raw")));
        assert_eq!(read_stored_credential("missing", Some(path)), None);
    }

    #[test]
    fn oauth_round_trip() {
        let path = temp_auth_path();
        let storage = AuthStorage::create(Some(path));
        let oauth = Credential::OAuth {
            refresh: "r".into(),
            access: "a".into(),
            expires: 1234.0,
            extra: vec![("scope".to_string(), Value::String("read".to_string()))],
        };
        let stored_oauth = oauth.clone();
        storage.modify("p", Box::new(move |_| Some(stored_oauth)));
        let stored = storage.read("p").unwrap();
        assert_eq!(stored, oauth);
        assert_eq!(storage.list()[0].credential_type, "oauth");
    }

    #[test]
    fn file_revision_changes_with_content() {
        let path = temp_auth_path();
        fs::write(&path, "{}").unwrap();
        let first = get_file_revision(&path).unwrap();
        fs::write(&path, "{\"a\":1}").unwrap();
        let second = get_file_revision(&path).unwrap();
        assert_ne!(first, second);
    }
}
