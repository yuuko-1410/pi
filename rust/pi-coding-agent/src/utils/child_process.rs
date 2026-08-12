//! Child process helpers and path utilities, ports of
//! `packages/coding-agent/src/utils/{child-process,paths}.ts`.
//!
//! Synchronous analog: `spawnProcess` returns the child handle after
//! starting; stdout/stderr are captured on reader threads and drained by
//! `wait_for_child`. `spawnProcessSync` blocks and returns the captured
//! output. cross-spawn (Windows quirk handling) is replaced by plain
//! Command (documented).

use std::io::Read;
use std::io::Write;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};

/// Captured output of a synchronous spawn (JS SpawnSyncReturns).
#[derive(Clone, Debug, PartialEq)]
pub struct SyncSpawnResult {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub signal: Option<i32>,
}

pub struct SpawnOptions {
    pub cwd: Option<String>,
    pub env: Option<Vec<(String, String)>>,
    pub timeout_ms: Option<u64>,
    pub stdin: Option<String>,
    pub stdio_ignore: bool,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            cwd: None,
            env: None,
            timeout_ms: None,
            stdin: None,
            stdio_ignore: false,
        }
    }
}

fn build_command(command: &str, args: &[String], options: &SpawnOptions) -> Command {
    let mut builder = Command::new(command);
    builder.args(args);
    if let Some(cwd) = &options.cwd {
        builder.current_dir(cwd);
    }
    if let Some(env) = &options.env {
        builder.envs(env.iter().cloned());
    }
    builder
}

fn spawn_builder(command: &str, args: &[String], options: &SpawnOptions) -> Command {
    let mut builder = build_command(command, args, options);
    if options.stdio_ignore {
        builder
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    } else {
        builder
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    }
    builder
}

/// Spawn a process (JS `spawnProcess`).
pub fn spawn_process(
    command: &str,
    args: &[String],
    options: &SpawnOptions,
) -> Result<Child, String> {
    let mut child = spawn_builder(command, args, options)
        .spawn()
        .map_err(|error| error.to_string())?;
    if let Some(stdin_text) = &options.stdin {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(stdin_text.as_bytes());
        }
    }
    Ok(child)
}

/// Spawn a process synchronously and capture output (JS `spawnProcessSync`).
pub fn spawn_process_sync(
    command: &str,
    args: &[String],
    options: &SpawnOptions,
) -> Result<SyncSpawnResult, String> {
    let mut child = spawn_builder(command, args, options)
        .spawn()
        .map_err(|error| error.to_string())?;
    if let Some(stdin_text) = &options.stdin {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(stdin_text.as_bytes());
        }
    }
    drop(child.stdin.take());

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

    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if let Some(timeout_ms) = options.timeout_ms {
                    if start.elapsed() >= std::time::Duration::from_millis(timeout_ms) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Ok(SyncSpawnResult {
                            status: None,
                            stdout: String::new(),
                            stderr: String::new(),
                            signal: Some(9),
                        });
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) => return Err(error.to_string()),
        }
    };
    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    Ok(SyncSpawnResult {
        status: status.code(),
        stdout,
        stderr,
        signal: status.code().map(|_| None).unwrap_or(None).or_else(|| {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                status.signal()
            }
            #[cfg(not(unix))]
            {
                None
            }
        }),
    })
}

/// Reader thread draining a pipe; join for the captured output.
pub fn drain_pipe(mut pipe: ChildStdout) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buffer = String::new();
        let _ = pipe.read_to_string(&mut buffer);
        buffer
    })
}

/// Re-export the ChildStdout/ChildStderr types for callers that capture
/// pipes and then wait.
pub use std::process::{ChildStderr as ChildStderrReexport, ChildStdout as ChildStdoutReexport};

// ---------------------------------------------------------------------------
// paths.ts
// ---------------------------------------------------------------------------

/// Canonicalize a path following symlinks, falling back to the raw path on
/// failure (JS `canonicalizePath`).
pub fn canonicalize_path(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string())
}

/// File revision identity (dev:ino:size:mtime_ns:ctime_ns), JS
/// `getFileRevision`.
pub fn get_file_revision(path: &str) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(format!(
            "{}:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.size(),
            metadata.mtime_nsec(),
            metadata.ctime_nsec()
        ))
    }
    #[cfg(not(unix))]
    {
        Some(format!(
            "{}:{}:{}",
            path.len(),
            metadata.len(),
            0u64
        ))
    }
}

/// True when the value is NOT a package source or remote URL protocol (JS
/// `isLocalPath`).
pub fn is_local_path(value: &str) -> bool {
    let trimmed = value.trim();
    !(trimmed.starts_with("npm:")
        || trimmed.starts_with("git:")
        || trimmed.starts_with("github:")
        || trimmed.starts_with("http:")
        || trimmed.starts_with("https:")
        || trimmed.starts_with("ssh:"))
}

const UNICODE_SPACES: &[char] = &[
    '\u{00A0}', '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}',
    '\u{2007}', '\u{2008}', '\u{2009}', '\u{200A}', '\u{202F}', '\u{205F}', '\u{3000}',
];

#[derive(Clone, Debug, Default)]
pub struct PathInputOptions {
    pub trim: bool,
    pub expand_tilde: bool,
    pub home_dir: Option<String>,
    pub strip_at_prefix: bool,
    pub normalize_unicode_spaces: bool,
}

fn home_dir() -> Option<String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| home.to_string_lossy().to_string())
}

fn file_url_to_path(url: &str) -> String {
    // file:///abs/path or file://localhost/abs/path
    let rest = url.strip_prefix("file://").unwrap_or(url);
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    // Percent-decode.
    let decoded = percent_decode(rest);
    std::path::PathBuf::from(decoded).to_string_lossy().to_string()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_val(bytes[index + 1]), hex_val(bytes[index + 2])) {
                result.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        result.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&result).to_string()
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Normalize a path (JS `normalizePath`).
pub fn normalize_path(input: &str, options: &PathInputOptions) -> String {
    let mut normalized = if options.trim {
        input.trim().to_string()
    } else {
        input.to_string()
    };
    if options.normalize_unicode_spaces {
        for space in UNICODE_SPACES {
            normalized = normalized.replace(*space, " ");
        }
    }
    if options.strip_at_prefix && normalized.starts_with('@') {
        normalized = normalized[1..].to_string();
    }
    if options.expand_tilde || !options.expand_tilde && options.home_dir.is_none() {
        if normalized == "~" {
            if let Some(home) = &options.home_dir {
                return home.clone();
            }
            if let Some(home) = home_dir() {
                return home;
            }
        }
        if let Some(rest) = normalized.strip_prefix("~/") {
            let home = options.home_dir.clone().or_else(home_dir);
            if let Some(home) = home {
                return std::path::Path::new(&home)
                    .join(rest)
                    .to_string_lossy()
                    .to_string();
            }
        }
    }
    if normalized.starts_with("file://") {
        return file_url_to_path(&normalized);
    }
    normalized
}

/// Resolve a path against a base directory (JS `resolvePath`).
pub fn resolve_path(input: &str, base_dir: &str, options: &PathInputOptions) -> String {
    let normalized = normalize_path(input, options);
    let normalized_base = normalize_path(base_dir, &PathInputOptions::default());
    let path = std::path::Path::new(&normalized);
    if path.is_absolute() {
        path.to_string_lossy().to_string()
    } else {
        std::path::Path::new(&normalized_base)
            .join(path)
            .to_string_lossy()
            .to_string()
    }
}

/// Relative path when inside cwd (JS `getCwdRelativePath`).
pub fn get_cwd_relative_path(file_path: &str, cwd: &str) -> Option<String> {
    let resolved_cwd = resolve_path(cwd, cwd, &PathInputOptions::default());
    let resolved_path = resolve_path(file_path, &resolved_cwd, &PathInputOptions::default());
    let relative = std::path::Path::new(&resolved_path)
        .strip_prefix(&resolved_cwd)
        .ok()?
        .to_string_lossy()
        .to_string();
    let relative = if relative.is_empty() {
        ".".to_string()
    } else {
        relative
    };
    if relative == ".." || relative.starts_with("../") {
        None
    } else {
        Some(relative)
    }
}

/// Format a path relative to cwd or absolute with forward slashes (JS
/// `formatPathRelativeToCwdOrAbsolute`).
pub fn format_path_relative_to_cwd_or_absolute(file_path: &str, cwd: &str) -> String {
    let absolute = resolve_path(file_path, cwd, &PathInputOptions::default());
    get_cwd_relative_path(&absolute, cwd)
        .unwrap_or(absolute)
        .replace('\\', "/")
}

/// Mark a path ignored by cloud sync (xattr/setfattr, best-effort; JS
/// `markPathIgnoredByCloudSync`).
pub fn mark_path_ignored_by_cloud_sync(path: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = spawn_process_sync(
            "xattr",
            &["-w".to_string(), "com.dropbox.ignored".to_string(), "1".to_string(), path.to_string()],
            &SpawnOptions {
                stdio_ignore: true,
                ..SpawnOptions::default()
            },
        );
    }
    #[cfg(target_os = "linux")]
    {
        let _ = spawn_process_sync(
            "setfattr",
            &["-n".to_string(), "user.com.dropbox.ignored".to_string(), "-v".to_string(), "1".to_string(), path.to_string()],
            &SpawnOptions {
                stdio_ignore: true,
                ..SpawnOptions::default()
            },
        );
    }
}

/// Spawn handle for tracked detached children.
pub struct TrackedChild {
    pid: u32,
    child: Option<Child>,
}

impl TrackedChild {
    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn wait(mut self) -> Result<SyncSpawnResult, String> {
        let stdout = self.child.as_mut().and_then(|child| child.stdout.take());
        let stderr = self.child.as_mut().and_then(|child| child.stderr.take());
        let stdout_handle = stdout.map(drain_stdout);
        let stderr_handle = stderr.map(drain_stderr);
        let status = self.child.as_mut().unwrap().wait().map_err(|error| error.to_string())?;
        #[cfg(unix)]
        let signal = {
            use std::os::unix::process::ExitStatusExt;
            status.signal()
        };
        #[cfg(not(unix))]
        let signal = None;
        Ok(SyncSpawnResult {
            status: status.code(),
            stdout: stdout_handle.map(|handle| handle.join().unwrap_or_default()).unwrap_or_default(),
            stderr: stderr_handle.map(|handle| handle.join().unwrap_or_default()).unwrap_or_default(),
            signal,
        })
    }
}

fn drain_stdout(pipe: ChildStdout) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buffer = String::new();
        let mut pipe = pipe;
        let _ = pipe.read_to_string(&mut buffer);
        buffer
    })
}

fn drain_stderr(pipe: ChildStderr) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buffer = String::new();
        let mut pipe = pipe;
        let _ = pipe.read_to_string(&mut buffer);
        buffer
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_process_sync_captures_output() {
        let result = spawn_process_sync("echo", &["hello".to_string()], &SpawnOptions::default()).unwrap();
        assert_eq!(result.status, Some(0));
        assert_eq!(result.stdout.trim(), "hello");
    }

    #[test]
    fn spawn_process_sync_captures_exit_code() {
        let result = spawn_process_sync("sh", &["-c".to_string(), "exit 3".to_string()], &SpawnOptions::default()).unwrap();
        assert_eq!(result.status, Some(3));
    }

    #[test]
    fn canonicalize_path_falls_back() {
        assert_eq!(canonicalize_path("/definitely/not/here"), "/definitely/not/here");
        let current = canonicalize_path(".");
        assert!(!current.is_empty());
    }

    #[test]
    fn resolves_paths() {
        let resolved = resolve_path("rel/path", "/tmp", &PathInputOptions::default());
        assert_eq!(resolved, "/tmp/rel/path");
        let resolved = resolve_path("/abs", "/tmp", &PathInputOptions::default());
        assert_eq!(resolved, "/abs");
        let expanded = normalize_path("~/x", &PathInputOptions {
            expand_tilde: true,
            ..PathInputOptions::default()
        });
        assert!(!expanded.starts_with("~/"));
    }

    #[test]
    fn file_url_conversion() {
        assert_eq!(file_url_to_path("file:///etc/hosts"), "/etc/hosts");
        assert_eq!(file_url_to_path("file://localhost/tmp/a%20b"), "/tmp/a b");
    }

    #[test]
    fn local_path_detection() {
        assert!(is_local_path("src/main.ts"));
        assert!(is_local_path("./foo"));
        assert!(!is_local_path("npm:pkg"));
        assert!(!is_local_path("https://example.com"));
        assert!(!is_local_path("git:user/repo"));
    }

    #[test]
    fn format_relative_or_absolute() {
        let formatted = format_path_relative_to_cwd_or_absolute("/tmp/x/y.ts", "/tmp/x");
        assert_eq!(formatted, "y.ts");
        let formatted = format_path_relative_to_cwd_or_absolute("/other/z.ts", "/tmp/x");
        assert_eq!(formatted, "/other/z.ts");
    }

    #[test]
    fn strips_unicode_spaces() {
        let normalized = normalize_path("a\u{00A0}b", &PathInputOptions {
            normalize_unicode_spaces: true,
            ..PathInputOptions::default()
        });
        assert_eq!(normalized, "a b");
    }
}
