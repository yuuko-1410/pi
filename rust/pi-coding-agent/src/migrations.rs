//! One-time startup migrations, port of
//! `packages/coding-agent/src/migrations.ts`.

use std::path::Path;

use crate::config::{get_agent_dir, get_bin_dir, CONFIG_DIR_NAME};

const MIGRATION_GUIDE_URL: &str =
    "https://github.com/earendil-works/pi-mono/blob/main/packages/coding-agent/CHANGELOG.md#extensions-migration";

fn join(dir: &str, name: &str) -> String {
    Path::new(dir).join(name).to_string_lossy().to_string()
}

/// Migrate legacy oauth.json and settings.json apiKeys to auth.json;
/// returns migrated provider names (JS `migrateAuthToAuthJson`).
pub fn migrate_auth_to_auth_json() -> Vec<String> {
    let agent_dir = get_agent_dir();
    let auth_path = join(&agent_dir, "auth.json");
    let oauth_path = join(&agent_dir, "oauth.json");
    let settings_path = join(&agent_dir, "settings.json");

    if Path::new(&auth_path).exists() {
        return vec![];
    }

    let mut migrated: Vec<(String, String)> = Vec::new();
    let mut providers: Vec<String> = Vec::new();

    if Path::new(&oauth_path).exists() {
        if let Ok(content) = std::fs::read_to_string(&oauth_path) {
            if let Ok(value) = pi_ai::utils::json::parse_json_with_repair::<pi_protocol::cbor::Value>(&content) {
                if let Some(entries) = value.as_map() {
                    for (provider, cred) in entries {
                        migrated.push((provider.clone(), json_stringify(&Value::Map(vec![
                            ("type".to_string(), Value::String("oauth".to_string())),
                            ("cred".to_string(), cred.clone()),
                        ]))));
                        providers.push(provider.clone());
                    }
                }
            }
            let _ = std::fs::rename(&oauth_path, format!("{oauth_path}.migrated"));
        }
    }

    if Path::new(&settings_path).exists() {
        if let Ok(content) = std::fs::read_to_string(&settings_path) {
            if let Ok(value) = pi_ai::utils::json::parse_json_with_repair::<pi_protocol::cbor::Value>(&content) {
                if let Value::Map(mut entries) = value {
                    let api_keys: Vec<(String, Value)> = entries
                        .iter()
                        .find(|(key, _)| key == "apiKeys")
                        .and_then(|(_, value)| value.as_map())
                        .map(|map| map.to_vec())
                        .unwrap_or_default();
                    for (provider, key) in api_keys {
                        if !migrated.iter().any(|(existing, _)| existing.as_str() == provider) {
                            if let Some(key_value) = key.as_str() {
                                migrated.push((provider.clone(), json_stringify(&Value::Map(vec![
                                    ("type".to_string(), Value::String("api_key".to_string())),
                                    ("key".to_string(), Value::String(key_value.to_string())),
                                ]))));
                                providers.push(provider.clone());
                            }
                        }
                    }
                    entries.retain(|(key, _)| key != "apiKeys");
                    if let Ok(serialized) = serde_like_json(&Value::Map(entries)) {
                        let _ = std::fs::write(&settings_path, serialized);
                    }
                }
            }
        }
    }

    if !migrated.is_empty() {
        let auth_value = Value::Map(
            migrated
                .iter()
                .map(|(provider, value)| {
                    (provider.clone(), pi_ai::utils::json::parse_json_with_repair::<pi_protocol::cbor::Value>(value).unwrap_or(Value::Null))
                })
                .collect(),
        );
        if let Ok(serialized) = serde_like_json(&auth_value) {
            let _ = std::fs::write(&auth_path, serialized);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o600));
        }
    }

    providers
}

use pi_protocol::cbor::Value;

fn json_stringify(value: &Value) -> String {
    pi_ai::utils::json::json_stringify(value)
}

fn serde_like_json(value: &Value) -> Result<String, String> {
    Ok(pretty_json(value, 0))
}

fn pretty_json(value: &Value, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let child_pad = " ".repeat(indent + 2);
    match value {
        Value::Map(entries) => {
            if entries.is_empty() {
                return "{}".to_string();
            }
            let inner: Vec<String> = entries
                .iter()
                .map(|(key, value)| format!("{child_pad}{:?}: {}", json_string(&key), pretty_json(value, indent + 2)))
                .collect();
            format!("{{
{}
{pad}}}", inner.join(",
"))
        }
        Value::Array(items) => {
            if items.is_empty() {
                return "[]".to_string();
            }
            let inner: Vec<String> = items
                .iter()
                .map(|item| format!("{child_pad}{}", pretty_json(item, indent + 2)))
                .collect();
            format!("[
{}
{pad}]", inner.join(",
"))
        }
        _ => json_stringify(value),
    }
}

fn json_string(key: &str) -> String {
    format!("\"{}\"", key.replace('\\', "\\\\").replace('\"', "\\\""))
}

/// Migrate sessions from agent root *.jsonl to session directories (JS
/// `migrateSessionsFromAgentRoot`).
pub fn migrate_sessions_from_agent_root() {
    let agent_dir = get_agent_dir();
    let files: Vec<String> = match std::fs::read_dir(&agent_dir) {
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.path().to_string_lossy().to_string())
            .filter(|path| path.ends_with(".jsonl"))
            .collect(),
        Err(_) => return,
    };
    if files.is_empty() {
        return;
    }

    for file in files {
        let content = match std::fs::read_to_string(&file) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let Some(first_line) = content.split('\n').next() else {
            continue;
        };
        let first_line = first_line.trim();
        if first_line.is_empty() {
            continue;
        }
        let header = match pi_ai::utils::json::parse_json_with_repair::<pi_protocol::cbor::Value>(first_line) {
            Ok(header) => header,
            Err(_) => continue,
        };
        let Some(entries) = header.as_map() else { continue };
        let is_session = entries
            .iter()
            .find(|(key, _)| key == "type")
            .and_then(|(_, value)| value.as_str())
            == Some("session");
        let cwd = entries.iter().find(|(key, _)| key == "cwd").and_then(|(_, value)| value.as_str());
        if !is_session || cwd.is_none() {
            continue;
        }
        let cwd = cwd.unwrap();
        let trimmed: String = cwd.trim_start_matches(['/', '\\']).to_string();
        let safe_path = format!(
            "--{}--",
            trimmed
                .chars()
                .map(|ch| if ch == '/' || ch == '\\' || ch == ':' { '-' } else { ch })
                .collect::<String>()
        );
        let correct_dir = join(&join(&agent_dir, "sessions"), &safe_path);
        if !Path::new(&correct_dir).exists() {
            let _ = std::fs::create_dir_all(&correct_dir);
        }
        let file_name = Path::new(&file)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let new_path = join(&correct_dir, &file_name);
        if Path::new(&new_path).exists() {
            continue;
        }
        let _ = std::fs::rename(&file, &new_path);
    }
}

/// Migrate commands/ to prompts/ (JS `migrateCommandsToPrompts`).
fn migrate_commands_to_prompts(base_dir: &str, label: &str) -> bool {
    let commands_dir = join(base_dir, "commands");
    let prompts_dir = join(base_dir, "prompts");
    if Path::new(&commands_dir).exists() && !Path::new(&prompts_dir).exists() {
        match std::fs::rename(&commands_dir, &prompts_dir) {
            Ok(()) => {
                println!("\x1b[32mMigrated {label} commands/ → prompts/\x1b[0m");
                true
            }
            Err(error) => {
                println!(
                    "\x1b[33mWarning: Could not migrate {label} commands/ to prompts/: {error}\x1b[0m"
                );
                false
            }
        }
    } else {
        false
    }
}

/// Move fd/rg binaries from tools/ to bin/ (JS `migrateToolsToBin`).
fn migrate_tools_to_bin() {
    let agent_dir = get_agent_dir();
    let tools_dir = join(&agent_dir, "tools");
    let bin_dir = get_bin_dir();
    if !Path::new(&tools_dir).exists() {
        return;
    }
    let binaries = ["fd", "rg", "fd.exe", "rg.exe"];
    let mut moved_any = false;
    for bin in binaries {
        let old_path = join(&tools_dir, bin);
        let new_path = join(&bin_dir, bin);
        if Path::new(&old_path).exists() {
            if !Path::new(&new_path).exists() {
                let _ = std::fs::create_dir_all(&bin_dir);
                if std::fs::rename(&old_path, &new_path).is_ok() {
                    moved_any = true;
                }
            } else {
                let _ = std::fs::remove_file(&old_path);
            }
        }
    }
    if moved_any {
        println!("\x1b[32mMigrated managed binaries tools/ → bin/\x1b[0m");
    }
}

/// Check deprecated hooks/ and tools/ dirs (JS `checkDeprecatedExtensionDirs`).
fn check_deprecated_extension_dirs(base_dir: &str, label: &str) -> Vec<String> {
    let hooks_dir = join(base_dir, "hooks");
    let tools_dir = join(base_dir, "tools");
    let mut warnings: Vec<String> = Vec::new();
    if Path::new(&hooks_dir).exists() {
        warnings.push(format!("{label} hooks/ directory found. Hooks have been renamed to extensions."));
    }
    if Path::new(&tools_dir).exists() {
        if let Ok(entries) = std::fs::read_dir(&tools_dir) {
            let custom_tools: Vec<String> = entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .filter(|name| {
                    let lower = name.to_lowercase();
                    lower != "fd"
                        && lower != "rg"
                        && lower != "fd.exe"
                        && lower != "rg.exe"
                        && !name.starts_with('.')
                })
                .collect();
            if !custom_tools.is_empty() {
                warnings.push(format!(
                    "{label} tools/ directory contains custom tools. Custom tools have been merged into extensions."
                ));
            }
        }
    }
    warnings
}

/// Migrate the extension system and collect warnings (JS
/// `migrateExtensionSystem`).
fn migrate_extension_system(cwd: &str) -> Vec<String> {
    let agent_dir = get_agent_dir();
    let project_dir = join(cwd, CONFIG_DIR_NAME);
    migrate_commands_to_prompts(&agent_dir, "Global");
    migrate_commands_to_prompts(&project_dir, "Project");
    let mut warnings = check_deprecated_extension_dirs(&agent_dir, "Global");
    warnings.extend(check_deprecated_extension_dirs(&project_dir, "Project"));
    warnings
}

/// Print deprecation warnings (JS `showDeprecationWarnings`; the keypress
/// wait is a no-op in the synchronous port).
pub fn show_deprecation_warnings(warnings: &[String]) {
    if warnings.is_empty() {
        return;
    }
    for warning in warnings {
        println!("\x1b[33mWarning: {warning}\x1b[0m");
    }
    println!("\x1b[33m\nMove your extensions to the extensions/ directory.\x1b[0m");
    println!("\x1b[33mMigration guide: {MIGRATION_GUIDE_URL}\x1b[0m");
    println!();
}

/// Run all migrations (JS `runMigrations`).
pub fn run_migrations(cwd: &str) -> (Vec<String>, Vec<String>) {
    let migrated_auth_providers = migrate_auth_to_auth_json();
    migrate_sessions_from_agent_root();
    migrate_tools_to_bin();
    let deprecation_warnings = migrate_extension_system(cwd);
    (migrated_auth_providers, deprecation_warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_isolated_agent_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pi-migrate-{}-{:x}", std::process::id(), rand_test_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("PI_CODING_AGENT_DIR", dir.to_string_lossy().to_string());
        dir
    }

    fn rand_test_suffix() -> u32 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0) ^ std::process::id()
    }

    #[test]
    fn migrates_auth_from_oauth_and_settings() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let dir = with_isolated_agent_dir();
        std::fs::write(dir.join("oauth.json"), "{\"openai\": {\"accessToken\": \"tok\"}, \"anthropic\": {\"id\": 1}}").unwrap();
        std::fs::write(dir.join("settings.json"), "{\"apiKeys\": {\"openai\": \"sk-abc\", \"other\": \"key2\"}}").unwrap();

        let providers = migrate_auth_to_auth_json();
        assert!(providers.contains(&"openai".to_string()), "providers: {providers:?}");
        assert!(providers.contains(&"anthropic".to_string()));
        assert!(providers.contains(&"other".to_string()));
        assert!(Path::new(&dir.join("auth.json")).exists());
        assert!(Path::new(&dir.join("oauth.json.migrated")).exists());
        let settings = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(!settings.contains("apiKeys"));

        std::env::remove_var("PI_CODING_AGENT_DIR");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migrates_sessions_from_agent_root() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let dir = with_isolated_agent_dir();
        let jsonl = "{\"type\": \"session\", \"cwd\": \"/home/user/proj\"}\nrest";
        std::fs::write(dir.join("old.jsonl"), jsonl).unwrap();

        migrate_sessions_from_agent_root();
        let target = dir.join("sessions").join("--home-user-proj--").join("old.jsonl");
        assert!(target.exists(), "migrated file should exist at {target:?}");
        assert!(!dir.join("old.jsonl").exists());

        std::env::remove_var("PI_CODING_AGENT_DIR");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migrates_commands_to_prompts() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("pi-migrate-commands-{}-{:x}", std::process::id(), rand_test_suffix()));
        std::fs::create_dir_all(dir.join("commands")).unwrap();
        std::fs::write(dir.join("commands/foo.txt"), "x").unwrap();
        assert!(migrate_commands_to_prompts(&dir.to_string_lossy(), "Test"));
        assert!(dir.join("prompts/foo.txt").exists());
        assert!(!dir.join("commands").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
