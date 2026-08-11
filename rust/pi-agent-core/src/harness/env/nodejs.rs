//! Std-based execution environment, port of
//! `packages/agent/src/harness/env/nodejs.ts`.
//!
//! Documented differences:
//! - Synchronous: no AbortSignal parameters (callers control threads).
//! - `killProcessTree` kills the direct child only; Node kills the whole
//!   process group (no std process-group kill without libc).
//! - WSL legacy bash path detection is skipped; the shell comes from the
//!   constructor or `$SHELL`, defaulting to `bash`.
//! - `createTempFile` uses a monotonic counter + pid instead of `randomUUID`
//!   (shape equivalent: unique file name).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use super::types::{
    err, ok, ExecutionError, ExecutionErrorCode, FileError, FileErrorCode, FileInfo, FileKind, Result,
    ShellExecOptions,
};

fn fs_map<T>(result: std::io::Result<T>, path: Option<String>) -> std::result::Result<T, FileError> {
    result.map_err(|error| to_file_error(&error, path.as_deref()))
}

fn wrap<T>(result: std::result::Result<T, FileError>) -> Result<T, FileError> {
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error),
    }
}

const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
const MAX_TIMEOUT_SECONDS: f64 = MAX_TIMEOUT_MS / 1000.0;

fn resolve_timeout_ms(timeout: Option<f64>) -> Result<Option<u64>, ExecutionError> {
    let Some(timeout) = timeout else {
        return ok(None);
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return err(ExecutionError::new(
            ExecutionErrorCode::Timeout,
            "Invalid timeout: must be a finite number of seconds",
        ));
    }
    let timeout_ms = timeout * 1000.0;
    if timeout_ms > MAX_TIMEOUT_MS {
        return err(ExecutionError::new(
            ExecutionErrorCode::Timeout,
            format!("Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"),
        ));
    }
    ok(Some(timeout_ms as u64))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Resolve `~`, `~/...`, and `file://` prefixes against cwd for relative
/// paths. Mirrors `resolvePath`.
fn resolve_path(cwd: &str, path: &str) -> String {
    let normalized = path;
    if normalized == "~" {
        return home_dir()
            .map(|home| home.to_string_lossy().to_string())
            .unwrap_or_else(|| normalized.to_string());
    }
    if let Some(rest) = normalized.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    if let Some(rest) = normalized.strip_prefix("file://") {
        // file:///abs/path and file://localhost/abs/path
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        return PathBuf::from(rest).to_string_lossy().to_string();
    }
    let path_buf = PathBuf::from(normalized);
    if path_buf.is_absolute() {
        normalized.to_string()
    } else {
        PathBuf::from(cwd)
            .join(path_buf)
            .to_string_lossy()
            .to_string()
    }
}

fn file_kind_from_metadata(metadata: &fs::Metadata) -> FileKind {
    if metadata.file_type().is_symlink() {
        FileKind::Symlink
    } else if metadata.is_dir() {
        FileKind::Directory
    } else {
        FileKind::File
    }
}

fn file_info_from_metadata(path: &str, metadata: &fs::Metadata) -> FileInfo {
    let name = Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0);
    FileInfo {
        name,
        path: path.to_string(),
        kind: file_kind_from_metadata(metadata),
        size: metadata.len() as f64,
        mtime_ms,
    }
}

/// Map std::io::Error to a FileError with the JS code mapping.
fn to_file_error(error: &std::io::Error, fallback_path: Option<&str>) -> FileError {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => FileErrorCode::NotFound,
        std::io::ErrorKind::PermissionDenied => FileErrorCode::PermissionDenied,
        std::io::ErrorKind::IsADirectory => FileErrorCode::IsDirectory,
        std::io::ErrorKind::NotADirectory => FileErrorCode::NotDirectory,
        std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidData => FileErrorCode::Invalid,
        std::io::ErrorKind::Unsupported => FileErrorCode::NotSupported,
        _ => FileErrorCode::Unknown,
    };
    FileError::new(code, error.to_string(), fallback_path)
}

// ---------------------------------------------------------------------------
// Shell configuration
// ---------------------------------------------------------------------------

struct ShellConfig {
    shell: String,
    args: Vec<String>,
    command_transport: CommandTransport,
}

#[derive(PartialEq)]
enum CommandTransport {
    Args,
    Stdin,
}

fn detect_shell(shell_path: Option<&str>) -> String {
    if let Some(shell_path) = shell_path {
        return shell_path.to_string();
    }
    std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string())
}

fn get_bash_shell_config(shell: &str) -> ShellConfig {
    let name = Path::new(shell)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let is_fish = name == "fish";
    let args = if is_fish {
        vec!["--command".to_string()]
    } else {
        vec!["-c".to_string()]
    };
    ShellConfig {
        shell: shell.to_string(),
        args,
        command_transport: CommandTransport::Args,
    }
}

/// Shell command transport: args for posix shells, stdin for zsh (which
/// warns on -c with non-interactive invocation).
fn get_shell_config(shell_path: Option<&str>) -> ShellConfig {
    let shell = detect_shell(shell_path);
    let name = Path::new(&shell)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    if name == "zsh" {
        ShellConfig {
            shell,
            args: vec![],
            command_transport: CommandTransport::Stdin,
        }
    } else {
        get_bash_shell_config(&shell)
    }
}

fn get_shell_env(
    shell_env: Option<&[(String, String)]>,
    options_env: Option<&[(String, String)]>,
    inherit_env: Option<bool>,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = if inherit_env.unwrap_or(true) {
        std::env::vars().collect()
    } else {
        Vec::new()
    };
    if let Some(shell_env) = shell_env {
        for (key, value) in shell_env {
            if let Some(entry) = env.iter_mut().find(|(existing_key, _)| existing_key == key) {
                entry.1 = value.clone();
            } else {
                env.push((key.clone(), value.clone()));
            }
        }
    }
    if let Some(options_env) = options_env {
        for (key, value) in options_env {
            if let Some(entry) = env.iter_mut().find(|(existing_key, _)| existing_key == key) {
                entry.1 = value.clone();
            } else {
                env.push((key.clone(), value.clone()));
            }
        }
    }
    env
}

// ---------------------------------------------------------------------------
// Std execution environment
// ---------------------------------------------------------------------------

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Kill the child and its process group (mirrors Node's killProcessTree).
/// On non-Unix, kills only the direct child.
fn kill_process_tree(pid: u32) {
    #[cfg(unix)]
    let _ = Command::new("kill").arg("-TERM").arg(format!("-{pid}")).status();
    #[cfg(not(unix))]
    let _ = Command::new("kill").arg(pid.to_string()).status();
}

pub struct StdExecutionEnv {
    cwd: String,
    shell_path: Option<String>,
    shell_env: Option<Vec<(String, String)>>,
    active_child_pids: Mutex<Vec<u32>>,
}

impl StdExecutionEnv {
    pub fn new(cwd: &str, shell_path: Option<&str>, shell_env: Option<Vec<(String, String)>>) -> Self {
        Self {
            cwd: cwd.to_string(),
            shell_path: shell_path.map(|shell| shell.to_string()),
            shell_env,
            active_child_pids: Mutex::new(Vec::new()),
        }
    }

    pub fn exec_impl(&self, command: &str, options: &ShellExecOptions) -> Result<ExecResult_, ExecutionError> {
        let timeout_ms = match resolve_timeout_ms(options.timeout) {
            Result::Ok { value } => value,
            Result::Err { error } => return err(error),
        };

        let cwd = match &options.cwd {
            Some(cwd) => resolve_path(&self.cwd, cwd),
            None => self.cwd.clone(),
        };

        if !Path::new(&cwd).exists() {
            return err(ExecutionError::new(
                ExecutionErrorCode::SpawnError,
                format!("Working directory does not exist: {cwd}\nCannot execute bash commands."),
            ));
        }

        let shell_config = get_shell_config(self.shell_path.as_deref());
        let command_from_stdin = shell_config.command_transport == CommandTransport::Stdin;

        let mut command_builder = Command::new(&shell_config.shell);
        if command_from_stdin {
            command_builder.args(&shell_config.args);
        } else {
            command_builder.args(&shell_config.args).arg(command);
        }
        command_builder
            .current_dir(&cwd)
            .envs(get_shell_env(self.shell_env.as_deref(), options.env.as_deref(), options.inherit_env));
        #[cfg(unix)]
        command_builder.process_group(0); // kill -pid below kills the whole tree
        command_builder
            .stdin(if command_from_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match command_builder.spawn() {
            Ok(child) => child,
            Err(error) => {
                return err(ExecutionError::new(ExecutionErrorCode::SpawnError, error.to_string()));
            }
        };
        let pid = child.id();
        self.active_child_pids.lock().unwrap().push(pid);

        if command_from_stdin {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(command.as_bytes());
            }
        }

        // Stream stdout/stderr on reader threads; the parent polls for exit
        // with timeout support.
        let mut stdout_pipe = child.stdout.take().unwrap();
        let mut stderr_pipe = child.stderr.take().unwrap();
        let stdout_thread = std::thread::spawn(move || {
            let mut buffer = String::new();
            let _ = stdout_pipe.read_to_string(&mut buffer);
            buffer
        });
        let stderr_thread = std::thread::spawn(move || {
            let mut buffer = String::new();
            let _ = stderr_pipe.read_to_string(&mut buffer);
            buffer
        });

        let start = Instant::now();
        let mut timed_out = false;
        let exit_code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code().unwrap_or(0),
                Ok(None) => {
                    if let Some(timeout_ms) = timeout_ms {
                        if Instant::now() - start >= Duration::from_millis(timeout_ms) {
                            timed_out = true;
                            kill_process_tree(pid);
                            let _ = child.wait();
                            break -1;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    self.active_child_pids.lock().unwrap().retain(|active| *active != pid);
                    return err(ExecutionError::new(ExecutionErrorCode::SpawnError, error.to_string()));
                }
            }
        };

        self.active_child_pids.lock().unwrap().retain(|active| *active != pid);

        let stdout = stdout_thread.join().unwrap_or_default();
        let stderr = stderr_thread.join().unwrap_or_default();

        if timed_out {
            return err(ExecutionError::new(
                ExecutionErrorCode::Timeout,
                format!("timeout:{}", options.timeout.unwrap_or(0.0)),
            ));
        }

        ok(ExecResult_ {
            stdout,
            stderr,
            exit_code,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExecResult_ {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl From<ExecResult_> for super::types::ExecResult {
    fn from(result: ExecResult_) -> super::types::ExecResult {
        super::types::ExecResult {
            stdout: result.stdout,
            stderr: result.stderr,
            exit_code: result.exit_code,
        }
    }
}

impl super::types::FileSystem for StdExecutionEnv {
    fn cwd(&self) -> &str {
        &self.cwd
    }

    fn absolute_path(&self, path: &str) -> Result<String, FileError> {
        ok(resolve_path(&self.cwd, path))
    }

    fn join_path(&self, parts: &[&str]) -> Result<String, FileError> {
        ok(parts.join(std::path::MAIN_SEPARATOR_STR))
    }

    fn read_text_file(&self, path: &str) -> Result<String, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        wrap(fs_map(fs::read_to_string(&resolved), Some(resolved.clone())))
    }

    fn read_text_lines(&self, path: &str, max_lines: Option<f64>) -> Result<Vec<String>, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        if max_lines.is_some_and(|max_lines| max_lines <= 0.0) {
            return ok(Vec::new());
        }
        let content = match fs_map(fs::read_to_string(&resolved), Some(resolved.clone())) {
            Ok(value) => value,
            Err(error) => return err(error),
        };
        let lines: Vec<String> = content.lines().map(|line| line.to_string()).collect();
        match max_lines {
            Some(max_lines) => ok(lines.into_iter().take(max_lines as usize).collect()),
            None => ok(lines),
        }
    }

    fn read_binary_file(&self, path: &str) -> Result<Vec<u8>, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        wrap(fs_map(fs::read(&resolved), Some(resolved.clone())))
    }

    fn write_file(&self, path: &str, content: &[u8]) -> Result<(), FileError> {
        let resolved = resolve_path(&self.cwd, path);
        if let Some(parent) = Path::new(&resolved).parent() {
            let _ = fs::create_dir_all(parent);
        }
        wrap(fs_map(fs::write(&resolved, content), Some(resolved.clone())))
    }

    fn append_file(&self, path: &str, content: &[u8]) -> Result<(), FileError> {
        let resolved = resolve_path(&self.cwd, path);
        if let Some(parent) = Path::new(&resolved).parent() {
            let _ = fs::create_dir_all(parent);
        }
        use std::io::Write;
        match fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&resolved)
            .and_then(|mut file| file.write_all(content))
        {
            Ok(()) => ok(()),
            Err(error) => err(to_file_error(&error, Some(&resolved))),
        }
    }

    fn rename_file(&self, source_path: &str, destination_path: &str) -> Result<(), FileError> {
        let source = resolve_path(&self.cwd, source_path);
        let destination = resolve_path(&self.cwd, destination_path);
        wrap(fs_map(fs::rename(&source, &destination), Some(source.clone())))
    }

    fn file_info(&self, path: &str) -> Result<FileInfo, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        match fs::symlink_metadata(&resolved) {
            Ok(metadata) => ok(file_info_from_metadata(&resolved, &metadata)),
            Err(error) => err(to_file_error(&error, Some(&resolved))),
        }
    }

    fn list_dir(&self, path: &str) -> Result<Vec<FileInfo>, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let entries = match fs_map(fs::read_dir(&resolved), Some(resolved.clone())) {
            Ok(value) => value,
            Err(error) => return err(error),
        };
        let mut infos = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(value) => value,
                Err(error) => return err(to_file_error(&error, Some(&resolved))),
            };
            let entry_path = entry.path();
            match fs::symlink_metadata(&entry_path) {
                Ok(metadata) => infos.push(file_info_from_metadata(
                    &entry_path.to_string_lossy(),
                    &metadata,
                )),
                Err(error) => return err(to_file_error(&error, Some(&entry_path.to_string_lossy()))),
            }
        }
        ok(infos)
    }

    fn canonical_path(&self, path: &str) -> Result<String, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        match fs::canonicalize(&resolved) {
            Ok(canonical) => ok(canonical.to_string_lossy().to_string()),
            Err(error) => err(to_file_error(&error, Some(&resolved))),
        }
    }

    fn exists(&self, path: &str) -> Result<bool, FileError> {
        match self.file_info(path) {
            Result::Ok { .. } => ok(true),
            Result::Err { error } if error.code == FileErrorCode::NotFound => ok(false),
            Result::Err { error } => err(error),
        }
    }

    fn create_dir(&self, path: &str, recursive: Option<bool>) -> Result<(), FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let result = if recursive.unwrap_or(true) {
            fs::create_dir_all(&resolved)
        } else {
            fs::create_dir(&resolved)
        };
        match result {
            Ok(()) => ok(()),
            Err(error) => err(to_file_error(&error, Some(&resolved))),
        }
    }

    fn remove(&self, path: &str, recursive: Option<bool>, force: Option<bool>) -> Result<(), FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let result = if recursive.unwrap_or(false) {
            fs::remove_dir_all(&resolved)
        } else {
            fs::remove_file(&resolved)
        };
        match result {
            Ok(()) => ok(()),
            Err(error) if force.unwrap_or(false) && error.kind() == std::io::ErrorKind::NotFound => ok(()),
            Err(error) => err(to_file_error(&error, Some(&resolved))),
        }
    }

    fn create_temp_dir(&self, prefix: Option<&str>) -> Result<String, FileError> {
        let prefix = prefix.unwrap_or("tmp-");
        let dir = std::env::temp_dir();
        // mkdtemp-style: retry with counter suffix until exclusive.
        for _ in 0..100 {
            let candidate = dir.join(format!(
                "{prefix}{}-{}",
                std::process::id(),
                TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => return ok(candidate.to_string_lossy().to_string()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return err(to_file_error(&error, None)),
            }
        }
        err(FileError::new(
            FileErrorCode::Unknown,
            "could not create temporary directory",
            None,
        ))
    }

    fn create_temp_file(&self, prefix: Option<&str>, suffix: Option<&str>) -> Result<String, FileError> {
        let dir = match self.create_temp_dir(Some("tmp-")) {
            Result::Ok { value } => value,
            Result::Err { error } => return err(error),
        };
        let file_path = PathBuf::from(&dir).join(format!(
            "{}{}{}",
            prefix.unwrap_or(""),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed),
            suffix.unwrap_or("")
        ));
        match fs::write(&file_path, []) {
            Ok(()) => ok(file_path.to_string_lossy().to_string()),
            Err(error) => err(to_file_error(&error, Some(&file_path.to_string_lossy()))),
        }
    }

    fn cleanup(&self) {
        let pids: Vec<u32> = self.active_child_pids.lock().unwrap().clone();
        for pid in pids {
            kill_process_tree(pid);
        }
        self.active_child_pids.lock().unwrap().clear();
    }
}

impl super::types::Shell for StdExecutionEnv {
    fn exec(&self, command: &str, options: &ShellExecOptions) -> Result<super::types::ExecResult, ExecutionError> {
        match self.exec_impl(command, options) {
            Result::Ok { value } => ok(value.into()),
            Result::Err { error } => err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::{FileSystem, Shell};
    use super::*;

    #[test]
    fn resolves_paths() {
        let cwd = "/tmp";
        assert_eq!(resolve_path(cwd, "/abs/path"), "/abs/path");
        assert_eq!(resolve_path(cwd, "rel/path"), "/tmp/rel/path");
        assert_eq!(resolve_path(cwd, "file:///etc/hosts"), "/etc/hosts");
        assert_eq!(resolve_path(cwd, "file://localhost/etc/hosts"), "/etc/hosts");
    }

    #[test]
    fn timeout_validation() {
        assert_eq!(resolve_timeout_ms(None).ok_value(), Some(None));
        assert_eq!(resolve_timeout_ms(Some(1.5)).ok_value(), Some(Some(1500)));
        let error = match resolve_timeout_ms(Some(0.0)) {
            Result::Err { error } => error,
            _ => panic!("expected error"),
        };
        assert_eq!(error.code, ExecutionErrorCode::Timeout);
        let error = match resolve_timeout_ms(Some(9999999999.0)) {
            Result::Err { error } => error,
            _ => panic!("expected error"),
        };
        assert!(error.message.contains("maximum"));
    }

    #[test]
    fn file_roundtrip_and_info() {
        let dir = std::env::temp_dir().join(format!("pi-env-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let env = StdExecutionEnv::new(&dir.to_string_lossy(), None, None);

        let written = env.write_file("a.txt", b"hello");
        assert!(written.is_ok());
        assert_eq!(env.read_text_file("a.txt").ok_value().unwrap(), "hello");
        assert_eq!(env.read_binary_file("a.txt").ok_value().unwrap(), b"hello");

        let info = env.file_info("a.txt").ok_value().unwrap();
        assert_eq!(info.name, "a.txt");
        assert_eq!(info.kind, FileKind::File);
        assert_eq!(info.size, 5.0);

        let not_found = match env.file_info("missing.txt") {
            Result::Err { error } => error,
            _ => panic!("expected error"),
        };
        assert_eq!(not_found.code, FileErrorCode::NotFound);
        let expected_path = format!("{}/missing.txt", dir.to_string_lossy());
        assert_eq!(not_found.path, Some(expected_path));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_dir_and_exists() {
        let dir = std::env::temp_dir().join(format!("pi-env-list-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let env = StdExecutionEnv::new(&dir.to_string_lossy(), None, None);
        env.write_file("one.txt", b"1").unwrap();
        env.create_dir("sub", None).unwrap();

        assert!(env.exists("one.txt").ok_value().unwrap());
        assert!(!env.exists("nope.txt").ok_value().unwrap());

        let entries = env.list_dir(".").ok_value().unwrap();
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert!(names.contains(&"one.txt"));
        assert!(names.contains(&"sub"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn exec_runs_bash_command() {
        let env = StdExecutionEnv::new("/tmp", None, None);
        let result = env.exec("echo hello", &ShellExecOptions::default()).ok_value().unwrap();
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "hello");
    }

    #[test]
    fn exec_captures_exit_code() {
        let env = StdExecutionEnv::new("/tmp", None, None);
        let result = env.exec("exit 3", &ShellExecOptions::default()).ok_value().unwrap();
        assert_eq!(result.exit_code, 3);
    }

    #[test]
    fn exec_timeout_kills_command() {
        let env = StdExecutionEnv::new("/tmp", None, None);
        let options = ShellExecOptions {
            timeout: Some(0.2),
            ..ShellExecOptions::default()
        };
        let error = match env.exec("sleep 30", &options) {
            Result::Err { error } => error,
            _ => panic!("expected timeout"),
        };
        assert_eq!(error.code, ExecutionErrorCode::Timeout);
        assert_eq!(error.message, "timeout:0.2");
    }

    #[test]
    fn exec_missing_cwd_errors() {
        let env = StdExecutionEnv::new("/tmp", None, None);
        let options = ShellExecOptions {
            cwd: Some("/definitely/not/a/real/dir-xyz".to_string()),
            ..ShellExecOptions::default()
        };
        let error = match env.exec("echo hi", &options) {
            Result::Err { error } => error,
            _ => panic!("expected error"),
        };
        assert_eq!(error.code, ExecutionErrorCode::SpawnError);
        assert!(error.message.contains("Working directory does not exist"));
    }

    #[test]
    fn temp_files_are_unique() {
        let env = StdExecutionEnv::new("/tmp", None, None);
        let first = env.create_temp_file(Some("pre-"), Some("-suf")).ok_value().unwrap();
        let second = env.create_temp_file(Some("pre-"), Some("-suf")).ok_value().unwrap();
        assert_ne!(first, second);
        let dir = env.create_temp_dir(Some("pi-td-")).ok_value().unwrap();
        assert!(Path::new(&dir).is_dir());
    }
}
