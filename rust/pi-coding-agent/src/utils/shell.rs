//! Shell configuration and process-tree helpers, port of
//! `packages/coding-agent/src/utils/shell.ts`.

use std::path::Path;

use crate::config::get_bin_dir;
use crate::utils::child_process::spawn_process_sync;

#[derive(Clone, Debug, PartialEq)]
pub struct ShellConfig {
    pub shell: String,
    pub args: Vec<String>,
    pub command_transport: Option<String>,
}

fn is_legacy_wsl_bash_path(path: &str) -> bool {
    let normalized = path.replace('/', "\\").to_lowercase();
    let bytes = normalized.as_bytes();
    // ^[a-z]:\windows\(?:system32|sysnative)\bash\.exe$
    if bytes.len() < 22 || !bytes[1..2].is_ascii() {
        return false;
    }
    let rest = normalized
        .strip_prefix(&format!("{}:\\windows\\", normalized.chars().next().unwrap_or('c')))
        .unwrap_or(&normalized);
    rest == "system32\\bash.exe" || rest == "sysnative\\bash.exe"
}

fn get_bash_shell_config(shell: &str) -> ShellConfig {
    if is_legacy_wsl_bash_path(shell) {
        ShellConfig {
            shell: shell.to_string(),
            args: vec!["-s".to_string()],
            command_transport: Some("stdin".to_string()),
        }
    } else {
        ShellConfig {
            shell: shell.to_string(),
            args: vec!["-c".to_string()],
            command_transport: None,
        }
    }
}

fn find_bash_on_path() -> Option<String> {
    let result = spawn_process_sync("which", &["bash".to_string()], &Default::default()).ok()?;
    if result.status == Some(0) {
        result.stdout.trim().split('\n').next().map(|line| line.trim().to_string())
    } else {
        None
    }
}

/// Resolve shell configuration (JS `getShellConfig`).
pub fn get_shell_config(custom_shell_path: Option<&str>) -> Result<ShellConfig, String> {
    if let Some(custom_shell_path) = custom_shell_path {
        if Path::new(custom_shell_path).exists() {
            return Ok(get_bash_shell_config(custom_shell_path));
        }
        return Err(format!("Custom shell path not found: {custom_shell_path}"));
    }

    if Path::new("/bin/bash").exists() {
        return Ok(get_bash_shell_config("/bin/bash"));
    }
    if let Some(bash_on_path) = find_bash_on_path() {
        return Ok(get_bash_shell_config(&bash_on_path));
    }
    Ok(ShellConfig {
        shell: "sh".to_string(),
        args: vec!["-c".to_string()],
        command_transport: None,
    })
}

/// Environment with the bin dir prepended to PATH (JS `getShellEnv`).
pub fn get_shell_env() -> Vec<(String, String)> {
    let bin_dir = get_bin_dir();
    let path_key = std::env::vars()
        .find(|(key, _)| key.to_lowercase() == "path")
        .map(|(key, _)| key)
        .unwrap_or_else(|| "PATH".to_string());
    let current_path = std::env::var(&path_key).unwrap_or_default();
    let path_entries: Vec<&str> = current_path.split(':').filter(|entry| !entry.is_empty()).collect();
    let has_bin_dir = path_entries.iter().any(|entry| *entry == bin_dir);
    let updated_path = if has_bin_dir {
        current_path
    } else {
        let mut combined = bin_dir.clone();
        if !current_path.is_empty() {
            combined.push(':');
            combined.push_str(&current_path);
        }
        combined
    };
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    if let Some(entry) = env.iter_mut().find(|(key, _)| key == &path_key) {
        entry.1 = updated_path;
    } else {
        env.push((path_key, updated_path));
    }
    env
}

static TRACKED_DETACHED_CHILD_PIDS: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

pub fn track_detached_child_pid(pid: u32) {
    TRACKED_DETACHED_CHILD_PIDS.lock().unwrap().push(pid);
}

pub fn untrack_detached_child_pid(pid: u32) {
    TRACKED_DETACHED_CHILD_PIDS.lock().unwrap().retain(|tracked| *tracked != pid);
}

pub fn kill_tracked_detached_children() {
    let pids: Vec<u32> = TRACKED_DETACHED_CHILD_PIDS.lock().unwrap().clone();
    for pid in pids {
        kill_process_tree(pid);
    }
    TRACKED_DETACHED_CHILD_PIDS.lock().unwrap().clear();
}

/// Kill a process and all its children (JS `killProcessTree`): Unix uses
/// the process group (set via process_group(0) by the spawner) and falls
/// back to the direct child; Windows uses taskkill.
pub fn kill_process_tree(pid: u32) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(format!("-{pid}"))
            .status();
        let _ = std::process::Command::new("kill")
            .arg("-KILL")
            .arg(format!("-{pid}"))
            .status();
        let _ = std::process::Command::new("kill")
            .arg("-KILL")
            .arg(pid.to_string())
            .status();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_bash_shell_config() {
        let config = get_shell_config(None).unwrap();
        assert_eq!(config.args, vec!["-c".to_string()]);
    }

    #[test]
    fn custom_shell_path_missing_errors() {
        let error = get_shell_config(Some("/no/such/shell")).unwrap_err();
        assert!(error.contains("Custom shell path not found"));
    }

    #[test]
    fn legacy_wsl_path_uses_stdin() {
        let config = get_bash_shell_config("C:\\Windows\\System32\\bash.exe");
        assert_eq!(config.command_transport.as_deref(), Some("stdin"));
    }

    #[test]
    fn shell_env_includes_bin_dir() {
        let env = get_shell_env();
        let path = env
            .iter()
            .find(|(key, _)| key.to_lowercase() == "path")
            .map(|(_, value)| value.clone())
            .unwrap_or_default();
        assert!(path.contains(&get_bin_dir()));
    }

    #[test]
    fn tracked_children_cleared() {
        track_detached_child_pid(999999);
        assert!(!TRACKED_DETACHED_CHILD_PIDS.lock().unwrap().is_empty());
        untrack_detached_child_pid(999999);
        assert!(TRACKED_DETACHED_CHILD_PIDS.lock().unwrap().is_empty());
    }
}
