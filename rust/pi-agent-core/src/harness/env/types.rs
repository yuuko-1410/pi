//! Harness shared types, port of `packages/agent/src/harness/types.ts`.
//!
//! The JS interfaces are async with AbortSignal parameters; Rust versions
//! are synchronous with no abort signals (callers control threads). Errors
//! keep the JS codes and message shapes.

/// Result of a fallible operation: expected failures are returned instead of
/// thrown.
#[derive(Clone, Debug, PartialEq)]
pub enum Result<TValue, TError> {
    Ok { value: TValue },
    Err { error: TError },
}

pub fn ok<TValue, TError>(value: TValue) -> Result<TValue, TError> {
    Result::Ok { value }
}

pub fn err<TValue, TError>(error: TError) -> Result<TValue, TError> {
    Result::Err { error }
}

impl<TValue, TError> Result<TValue, TError> {
    pub fn is_ok(&self) -> bool {
        matches!(self, Result::Ok { .. })
    }
    pub fn ok_value(self) -> Option<TValue> {
        match self {
            Result::Ok { value } => Some(value),
            Result::Err { .. } => None,
        }
    }

    /// Return the success value or panic with the failure error (tests and
    /// explicit adapter boundaries; mirrors getOrThrow).
    pub fn unwrap(self) -> TValue
    where
        TError: std::fmt::Debug,
    {
        match self {
            Result::Ok { value } => value,
            Result::Err { error } => panic!("called Result::unwrap() on an Err: {error:?}"),
        }
    }
}

/// Kind of filesystem object as addressed by a FileSystem. Symlinks are not
/// followed automatically.
#[derive(Clone, Debug, PartialEq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
}

impl FileKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileKind::File => "file",
            FileKind::Directory => "directory",
            FileKind::Symlink => "symlink",
        }
    }
}

/// Stable, backend-independent file error codes.
#[derive(Clone, Debug, PartialEq)]
pub enum FileErrorCode {
    Aborted,
    NotFound,
    PermissionDenied,
    NotDirectory,
    IsDirectory,
    Invalid,
    NotSupported,
    Unknown,
}

impl FileErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileErrorCode::Aborted => "aborted",
            FileErrorCode::NotFound => "not_found",
            FileErrorCode::PermissionDenied => "permission_denied",
            FileErrorCode::NotDirectory => "not_directory",
            FileErrorCode::IsDirectory => "is_directory",
            FileErrorCode::Invalid => "invalid",
            FileErrorCode::NotSupported => "not_supported",
            FileErrorCode::Unknown => "unknown",
        }
    }
}

/// Error returned by FileSystem file operations.
#[derive(Clone, Debug, PartialEq)]
pub struct FileError {
    pub code: FileErrorCode,
    pub message: String,
    pub path: Option<String>,
}

impl FileError {
    pub fn new(code: FileErrorCode, message: impl Into<String>, path: Option<&str>) -> Self {
        Self {
            code,
            message: message.into(),
            path: path.map(|path| path.to_string()),
        }
    }
}

/// Stable, backend-independent execution error codes.
#[derive(Clone, Debug, PartialEq)]
pub enum ExecutionErrorCode {
    Aborted,
    Timeout,
    ShellUnavailable,
    SpawnError,
    CallbackError,
    Unknown,
}

impl ExecutionErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionErrorCode::Aborted => "aborted",
            ExecutionErrorCode::Timeout => "timeout",
            ExecutionErrorCode::ShellUnavailable => "shell_unavailable",
            ExecutionErrorCode::SpawnError => "spawn_error",
            ExecutionErrorCode::CallbackError => "callback_error",
            ExecutionErrorCode::Unknown => "unknown",
        }
    }
}

/// Error returned by ExecutionEnv.exec.
#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionError {
    pub code: ExecutionErrorCode,
    pub message: String,
}

impl ExecutionError {
    pub fn new(code: ExecutionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Metadata for one filesystem object in a FileSystem.
#[derive(Clone, Debug, PartialEq)]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    pub kind: FileKind,
    pub size: f64,
    pub mtime_ms: f64,
}

/// Options for Shell.exec.
#[derive(Clone, Default)]
pub struct ShellExecOptions {
    pub cwd: Option<String>,
    pub env: Option<Vec<(String, String)>>,
    pub inherit_env: Option<bool>,
    pub timeout: Option<f64>,
    pub on_stdout: Option<std::sync::Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>>,
    pub on_stderr: Option<std::sync::Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>>,
}

impl std::fmt::Debug for ShellExecOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShellExecOptions")
            .field("cwd", &self.cwd)
            .field("env", &self.env)
            .field("inherit_env", &self.inherit_env)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// Filesystem capability used by the harness. Operation methods never throw;
/// failures are encoded in the returned Result.
pub trait FileSystem {
    fn cwd(&self) -> &str;
    fn absolute_path(&self, path: &str) -> Result<String, FileError>;
    fn join_path(&self, parts: &[&str]) -> Result<String, FileError>;
    fn read_text_file(&self, path: &str) -> Result<String, FileError>;
    fn read_text_lines(&self, path: &str, max_lines: Option<f64>) -> Result<Vec<String>, FileError>;
    fn read_binary_file(&self, path: &str) -> Result<Vec<u8>, FileError>;
    fn write_file(&self, path: &str, content: &[u8]) -> Result<(), FileError>;
    fn append_file(&self, path: &str, content: &[u8]) -> Result<(), FileError>;
    fn rename_file(&self, source_path: &str, destination_path: &str) -> Result<(), FileError>;
    fn file_info(&self, path: &str) -> Result<FileInfo, FileError>;
    fn list_dir(&self, path: &str) -> Result<Vec<FileInfo>, FileError>;
    fn canonical_path(&self, path: &str) -> Result<String, FileError>;
    fn exists(&self, path: &str) -> Result<bool, FileError>;
    fn create_dir(&self, path: &str, recursive: Option<bool>) -> Result<(), FileError>;
    fn remove(&self, path: &str, recursive: Option<bool>, force: Option<bool>) -> Result<(), FileError>;
    fn create_temp_dir(&self, prefix: Option<&str>) -> Result<String, FileError>;
    fn create_temp_file(&self, prefix: Option<&str>, suffix: Option<&str>) -> Result<String, FileError>;
    fn cleanup(&self) {}
}

/// Shell execution capability used by the harness.
pub trait Shell {
    fn exec(&self, command: &str, options: &ShellExecOptions) -> Result<ExecResult, ExecutionError>;
    fn cleanup(&self) {}
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Filesystem and process execution environment used by the harness.
pub trait ExecutionEnv: FileSystem + Shell {}
impl<T: FileSystem + Shell> ExecutionEnv for T {}
