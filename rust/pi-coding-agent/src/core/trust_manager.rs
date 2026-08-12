//! Project trust store, port of `core/trust-manager.ts`. The proper-lockfile
//! cross-process lock is a process-wide mutex (single-process CLI).

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use crate::config::CONFIG_DIR_NAME;

pub type ProjectTrustDecision = Option<bool>;

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectTrustStoreEntry {
    pub path: String,
    pub decision: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectTrustUpdate {
    pub path: String,
    pub decision: ProjectTrustDecision,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectTrustOption {
    pub label: String,
    pub trusted: bool,
    pub updates: Vec<ProjectTrustUpdate>,
    pub saved_path: Option<String>,
}

type TrustFile = HashMap<String, Option<bool>>;

const TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES: &[&str] = &[
    "settings.json",
    "extensions",
    "skills",
    "prompts",
    "themes",
    "SYSTEM.md",
    "APPEND_SYSTEM.md",
];

static TRUST_FILE_MUTEX: Mutex<()> = Mutex::new(());

fn canonicalize_path(path: &str) -> String {
    fs::canonicalize(path)
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string())
}

fn resolve_path(input: &str) -> String {
    crate::core::session_paths::resolve_path(input, None)
}

fn normalize_cwd(cwd: &str) -> String {
    canonicalize_path(&resolve_path(cwd))
}

fn find_nearest_trust_entry(data: &TrustFile, cwd: &str) -> Option<ProjectTrustStoreEntry> {
    let mut current_dir = normalize_cwd(cwd);
    loop {
        if let Some(value) = data.get(&current_dir) {
            if let Some(decision) = value {
                return Some(ProjectTrustStoreEntry {
                    path: current_dir.clone(),
                    decision: *decision,
                });
            }
        }
        let parent_dir = Path::new(&current_dir)
            .parent()
            .map(|parent| parent.to_string_lossy().to_string());
        let Some(parent_dir) = parent_dir else {
            return None;
        };
        if parent_dir == current_dir {
            return None;
        }
        current_dir = parent_dir;
    }
}

pub fn get_project_trust_parent_path(cwd: &str) -> Option<String> {
    let trust_path = normalize_cwd(cwd);
    let parent_dir = Path::new(&trust_path)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string());
    match parent_dir {
        Some(parent_dir) if parent_dir != trust_path => Some(parent_dir),
        _ => None,
    }
}

pub fn get_project_trust_options(cwd: &str, include_session_only: bool) -> Vec<ProjectTrustOption> {
    let trust_path = normalize_cwd(cwd);
    let mut trust_options = vec![ProjectTrustOption {
        label: "Trust".to_string(),
        trusted: true,
        updates: vec![ProjectTrustUpdate {
            path: trust_path.clone(),
            decision: Some(true),
        }],
        saved_path: Some(trust_path.clone()),
    }];
    if let Some(parent_path) = get_project_trust_parent_path(cwd) {
        trust_options.push(ProjectTrustOption {
            label: format!("Trust parent folder ({parent_path})"),
            trusted: true,
            updates: vec![
                ProjectTrustUpdate {
                    path: parent_path.clone(),
                    decision: Some(true),
                },
                ProjectTrustUpdate {
                    path: trust_path.clone(),
                    decision: None,
                },
            ],
            saved_path: Some(parent_path),
        });
    }
    if include_session_only {
        trust_options.push(ProjectTrustOption {
            label: "Trust (this session only)".to_string(),
            trusted: true,
            updates: Vec::new(),
            saved_path: None,
        });
    }
    trust_options.push(ProjectTrustOption {
        label: "Do not trust".to_string(),
        trusted: false,
        updates: vec![ProjectTrustUpdate {
            path: trust_path.clone(),
            decision: Some(false),
        }],
        saved_path: Some(trust_path.clone()),
    });
    if include_session_only {
        trust_options.push(ProjectTrustOption {
            label: "Do not trust (this session only)".to_string(),
            trusted: false,
            updates: Vec::new(),
            saved_path: None,
        });
    }
    trust_options
}

fn read_trust_file(path: &str) -> Result<TrustFile, String> {
    if !Path::new(path).exists() {
        return Ok(TrustFile::new());
    }
    let content = fs::read_to_string(path).map_err(|error| format!("Failed to read trust store {path}: {error}"))?;
    let parsed: pi_protocol::Value = pi_ai::utils::json::parse_json_with_repair(&content)
        .map_err(|error| format!("Failed to read trust store {path}: {error}"))?;
    let entries = parsed
        .as_map()
        .ok_or_else(|| format!("Invalid trust store {path}: expected an object"))?;
    let mut data = TrustFile::new();
    for (key, value) in entries {
        let decision = match value {
            pi_protocol::Value::Bool(b) => Some(*b),
            pi_protocol::Value::Null => None,
            _ => {
                return Err(format!(
                    "Invalid trust store {path}: value for {} must be true, false, or null",
                    pi_ai::utils::json::json_stringify(&pi_protocol::Value::String(key.clone()))
                ));
            }
        };
        data.insert(key.clone(), decision);
    }
    Ok(data)
}

fn write_trust_file(path: &str, data: &TrustFile) -> Result<(), String> {
    let mut sorted_keys: Vec<&String> = data.keys().collect();
    sorted_keys.sort();
    let entries: Vec<(String, pi_protocol::Value)> = sorted_keys
        .into_iter()
        .filter_map(|key| {
            let value = data.get(key)?;
            let value = match value {
                Some(true) => pi_protocol::Value::Bool(true),
                Some(false) => pi_protocol::Value::Bool(false),
                None => pi_protocol::Value::Null,
            };
            Some((key.clone(), value))
        })
        .collect();
    if let Some(parent) = Path::new(path).parent() {
        let _ = fs::create_dir_all(parent);
    }
    let serialized = format!("{}\n", pi_ai::utils::json::json_stringify_pretty(&pi_protocol::Value::Map(entries)));
    fs::write(path, serialized).map_err(|error| error.to_string())
}

/// True when cwd has project-local resources gated by project trust.
pub fn has_trust_requiring_project_resources(cwd: &str) -> bool {
    let home_dir = canonicalize_path(&resolve_path(
        &std::env::var("HOME").unwrap_or_default(),
    ));
    let user_agents_skills_dir = Path::new(&home_dir).join(".agents").join("skills");
    let mut current_dir = canonicalize_path(&resolve_path(cwd));

    let config_dir = Path::new(&current_dir).join(CONFIG_DIR_NAME);
    if TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES
        .iter()
        .any(|entry| config_dir.join(entry).exists())
    {
        return true;
    }

    loop {
        let agents_skills_dir = Path::new(&current_dir).join(".agents").join("skills");
        if agents_skills_dir != user_agents_skills_dir && agents_skills_dir.exists() {
            return true;
        }
        let parent_dir = Path::new(&current_dir)
            .parent()
            .map(|parent| parent.to_string_lossy().to_string());
        let Some(parent_dir) = parent_dir else {
            return false;
        };
        if parent_dir == current_dir {
            return false;
        }
        current_dir = parent_dir;
    }
}

/// JSON-file-backed project trust store.
pub struct ProjectTrustStore {
    trust_path: String,
}

impl ProjectTrustStore {
    pub fn new(agent_dir: &str) -> Self {
        Self {
            trust_path: Path::new(&resolve_path(agent_dir)).join("trust.json").to_string_lossy().to_string(),
        }
    }

    pub fn get(&self, cwd: &str) -> ProjectTrustDecision {
        self.get_entry(cwd).map(|entry| entry.decision)
    }

    pub fn get_entry(&self, cwd: &str) -> Option<ProjectTrustStoreEntry> {
        let _guard = TRUST_FILE_MUTEX.lock().unwrap();
        let data = read_trust_file(&self.trust_path).ok()?;
        find_nearest_trust_entry(&data, cwd)
    }

    pub fn set(&self, cwd: &str, decision: ProjectTrustDecision) -> Result<(), String> {
        self.set_many(&[ProjectTrustUpdate {
            path: cwd.to_string(),
            decision,
        }])
    }

    pub fn set_many(&self, decisions: &[ProjectTrustUpdate]) -> Result<(), String> {
        let _guard = TRUST_FILE_MUTEX.lock().unwrap();
        let mut data = read_trust_file(&self.trust_path)?;
        for update in decisions {
            let key = normalize_cwd(&update.path);
            match update.decision {
                None => {
                    data.remove(&key);
                }
                Some(decision) => {
                    data.insert(key, Some(decision));
                }
            }
        }
        write_trust_file(&self.trust_path, &data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_agent_dir() -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-trust-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().to_string()
    }

    #[test]
    fn set_get_cycle() {
        let store = ProjectTrustStore::new(&temp_agent_dir());
        assert_eq!(store.get("/tmp"), None);
        store.set("/tmp", Some(true)).unwrap();
        assert_eq!(store.get("/tmp"), Some(true));
        store.set("/tmp", Some(false)).unwrap();
        assert_eq!(store.get("/tmp"), Some(false));
        store.set("/tmp", None).unwrap();
        assert_eq!(store.get("/tmp"), None);
    }

    #[test]
    fn nearest_entry_walks_up() {
        let store = ProjectTrustStore::new(&temp_agent_dir());
        store.set("/root/proj/sub", Some(true)).unwrap();
        assert_eq!(store.get("/root/proj/sub/deep"), Some(true));
    }

    #[test]
    fn trust_options() {
        let options = get_project_trust_options("/tmp", true);
        let labels: Vec<&str> = options.iter().map(|option| option.label.as_str()).collect();
        assert_eq!(options.len(), 5);
        assert_eq!(options[0].label, "Trust");
        assert!(options[0].trusted);
        // Parent option present only when cwd has a parent (macOS /tmp ->
        // /private/tmp does).
        assert!(labels.contains(&"Trust parent folder (/private)"));
        assert!(labels.contains(&"Trust (this session only)"));
        assert!(labels.contains(&"Do not trust"));
        assert!(labels.contains(&"Do not trust (this session only)"));
        let session_only = options.iter().find(|option| option.label == "Trust (this session only)").unwrap();
        assert!(session_only.updates.is_empty());
    }

    #[test]
    fn trust_requiring_resources() {
        let dir = temp_agent_dir();
        let cwd = Path::new(&dir).join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        assert!(!has_trust_requiring_project_resources(&cwd.to_string_lossy()));

        std::fs::create_dir_all(cwd.join(".pi").join("skills")).unwrap();
        assert!(has_trust_requiring_project_resources(&cwd.to_string_lossy()));
    }

    #[test]
    fn invalid_trust_file_errors() {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-trust-bad-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("trust.json"), "{\"x\": 42}").unwrap();
        let store = ProjectTrustStore::new(&dir.to_string_lossy());
        let result = store.set("/tmp", Some(true));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must be true, false, or null"));
    }
}
