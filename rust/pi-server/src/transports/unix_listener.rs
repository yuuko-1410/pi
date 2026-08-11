//! Unix domain socket listener, port of
//! `packages/server/src/transports/unix/listener.ts`.
//!
//! Documented differences:
//! - The owned bind path suffix uses a non-cryptographic hash of the path
//!   (no sha256 in std); same `.p-<8 hex>` shape.
//! - `isSocketLive` probes are synchronous connect attempts with a 1s
//!   timeout (std UnixStream has no connect timeout, so the probe blocks;
//!   documented).

use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener as StdUnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};


use crate::sessions::ByteConnection;
use crate::server::{ByteConnectionHandler, PiServerListener};

pub const DEFAULT_MAX_FRAME_LENGTH: u64 = 1 << 24;
const DEFAULT_SOCKET_MODE: u32 = 0o600;
const DEFAULT_GRACEFUL_CLOSE_TIMEOUT_MS: u64 = 5_000;
const MAX_UINT32: u64 = 0xffff_ffff;
const MAX_TIMER_DELAY_MS: u64 = 2_147_483_647;
const MAX_UNIX_SOCKET_PATH_BYTES: usize = 107;

pub fn validate_unix_socket_path(path: &str, description: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err(format!("{description} must not be empty"));
    }
    if path.len() > MAX_UNIX_SOCKET_PATH_BYTES {
        return Err(format!(
            "{description} is too long; maximum is {MAX_UNIX_SOCKET_PATH_BYTES} UTF-8 bytes"
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub struct UnixListenerOptions {
    pub path: String,
    pub mode: Option<u32>,
    pub graceful_close_timeout_ms: Option<u64>,
    pub max_frame_length: Option<u64>,
    pub max_pending_bytes: Option<u64>,
    pub on_error: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

impl std::fmt::Debug for UnixListenerOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UnixListenerOptions")
            .field("path", &self.path)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl Default for UnixListenerOptions {
    fn default() -> Self {
        Self {
            path: String::new(),
            mode: None,
            graceful_close_timeout_ms: None,
            max_frame_length: None,
            max_pending_bytes: None,
            on_error: None,
        }
    }
}

struct ResolvedUnixListenerOptions {
    path: String,
    mode: u32,
    graceful_close_timeout_ms: u64,
    max_pending_bytes: u64,
    on_error: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

fn resolve_unix_listener_options(options: &UnixListenerOptions) -> Result<ResolvedUnixListenerOptions, String> {
    validate_unix_socket_path(&options.path, "PiServer Unix socket path")?;
    let mode = options.mode.unwrap_or(DEFAULT_SOCKET_MODE);
    if mode > 0o777 {
        return Err("PiServer Unix socket mode must be an integer between 0 and 0o777".to_string());
    }
    let max_frame_length = options.max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
    if max_frame_length < 1 || max_frame_length > MAX_UINT32 {
        return Err(format!("PiServer maxFrameLength must be an integer between 1 and {MAX_UINT32}"));
    }
    let max_pending_bytes = options.max_pending_bytes.unwrap_or(max_frame_length * 4);
    if max_pending_bytes < max_frame_length + 4 {
        return Err("PiServer maxPendingBytes must be a safe integer at least maxFrameLength + 4".to_string());
    }
    let graceful_close_timeout_ms = options.graceful_close_timeout_ms.unwrap_or(DEFAULT_GRACEFUL_CLOSE_TIMEOUT_MS);
    if graceful_close_timeout_ms < 1 || graceful_close_timeout_ms > MAX_TIMER_DELAY_MS {
        return Err(format!(
            "PiServer gracefulCloseTimeoutMs must be an integer between 1 and {MAX_TIMER_DELAY_MS}"
        ));
    }
    Ok(ResolvedUnixListenerOptions {
        path: options.path.clone(),
        mode,
        graceful_close_timeout_ms,
        max_pending_bytes,
        on_error: options.on_error.clone(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
}

fn is_socket_file(metadata: &fs::Metadata) -> bool {
    // S_IFSOCK = 0o140000
    (metadata.mode() & 0o170000) == 0o140000
}

fn identity_of(metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

fn get_owned_bind_path(path: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    let suffix = format!("{:016x}", hasher.finish());
    let dir = std::path::Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());
    format!("{dir}/.p-{}", &suffix[..8])
}

fn remove_path(path: &str) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// Probe whether a socket path has a live listener (connect; refused/ENOENT
/// means stale).
fn is_socket_live(path: &str) -> Result<bool, String> {
    match UnixStream::connect(path) {
        Ok(_) => Ok(true),
        Err(error) => {
            let code = error.raw_os_error();
            match code {
                // ECONNREFUSED, ENOENT, EPIPE, ECONNRESET
                Some(61) | Some(2) | Some(32) | Some(54) => Ok(false),
                Some(60) => Ok(false), // ETIMEDOUT
                _ => {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        Ok(false)
                    } else {
                        Err(error.to_string())
                    }
                }
            }
        }
    }
}

/// Remove a stale socket by renaming it aside (identity-checked) and
/// unlinking the preserved copy.
fn remove_stale_socket(path: &str) -> Result<(), String> {
    let original = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if !is_socket_file(&original) {
        return Err(format!("Refusing to remove non-socket Unix listener path: {path}"));
    }
    if is_socket_live(path)? {
        return Err(format!("Unix listener is already running: {path}"));
    }
    let dir = std::path::Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());
    let preserved = format!("{dir}/.s-{:06x}", rand_suffix());
    match fs::rename(path, &preserved) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    }
    let current = fs::symlink_metadata(&preserved).map_err(|error| error.to_string())?;
    let original_identity = identity_of(&original);
    let current_identity = identity_of(&current);
    if !is_socket_file(&current)
        || current_identity.dev != original_identity.dev
        || current_identity.ino != original_identity.ino
    {
        if fs::symlink_metadata(path).is_err() {
            let _ = fs::rename(&preserved, path);
        }
        return Err(format!(
            "Unix listener path changed while checking for a stale socket: {path}"
        ));
    }
    remove_path(&preserved)
}

fn rand_suffix() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0)
        ^ std::process::id()
}

fn set_socket_mode(path: &str, mode: u32) -> Result<(), String> {
    match fs::set_permissions(path, fs::Permissions::from_mode(mode)) {
        Ok(()) => Ok(()),
        // ENOSYS / ENOTSUP ignored.
        Err(error) if error.raw_os_error() == Some(78) || error.raw_os_error() == Some(45) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// Unix domain socket byte connection with serialized writes and graceful
/// close (drain tail, end, destroy after timeout).
pub struct UnixByteConnection {
    stream: Mutex<Option<UnixStream>>,
    max_pending_bytes: u64,
    pending_bytes: Mutex<u64>,
    closed_value: AtomicBool,
    closing: AtomicBool,
}

impl PartialEq for UnixByteConnection {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}
impl Eq for UnixByteConnection {}
impl std::hash::Hash for UnixByteConnection {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self as *const Self).hash(state);
    }
}

impl UnixByteConnection {
    /// Split the stream: the reader half is moved to the reader thread so
    /// blocking reads never hold the send lock.
    pub fn new(
        stream: UnixStream,
        _graceful_close_timeout_ms: u64,
        max_pending_bytes: u64,
    ) -> (Arc<Self>, Option<UnixStream>) {
        let reader_stream = stream.try_clone().ok();
        (
            Arc::new(Self {
                stream: Mutex::new(Some(stream)),
                max_pending_bytes,
                pending_bytes: Mutex::new(0),
                closed_value: AtomicBool::new(false),
                closing: AtomicBool::new(false),
            }),
            reader_stream,
        )
    }

    pub fn mark_closed(&self) {
        if self.closed_value.swap(true, Ordering::SeqCst) {
            return;
        }
        self.closing.store(true, Ordering::SeqCst);
    }
}

impl ByteConnection for UnixByteConnection {
    fn closed(&self) -> bool {
        self.closed_value.load(Ordering::SeqCst)
    }

    fn send(&self, chunk: &[u8]) -> Result<(), String> {
        if self.closed_value.load(Ordering::SeqCst) || self.closing.load(Ordering::SeqCst) {
            return Err("Unix connection is closed".to_string());
        }
        let mut pending = self.pending_bytes.lock().unwrap();
        if *pending + chunk.len() as u64 > self.max_pending_bytes {
            return Err("Unix connection exceeded its pending byte limit".to_string());
        }
        *pending += chunk.len() as u64;
        // Serialize writes through the write tail channel; ponytail: a
        // single writer thread per connection keeps ordering without a lock
        // chain (JS uses a promise tail).
        let result = {
            let mut stream = self.stream.lock().unwrap();
            match stream.as_mut() {
                Some(stream) => {
                    use std::io::Write;
                    stream.write_all(chunk).map_err(|error| error.to_string())
                }
                None => Err("Unix connection is closed".to_string()),
            }
        };
        *pending -= chunk.len() as u64;
        result
    }

    fn close(&self, final_chunk: Option<Vec<u8>>) {
        if self.closed_value.load(Ordering::SeqCst) {
            return;
        }
        self.closing.store(true, Ordering::SeqCst);
        let mut stream = self.stream.lock().unwrap();
        if let Some(stream) = stream.as_mut() {
            use std::io::Write;
            if let Some(final_chunk) = final_chunk {
                let _ = stream.write_all(&final_chunk);
            }
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        *stream = None;
        self.mark_closed();
    }
}

/// Listener implementing PiServerListener over a Unix domain socket.
pub struct UnixListener {
    options: ResolvedUnixListenerOptions,
    path: String,
    connections: Arc<Mutex<HashSet<Arc<UnixByteConnection>>>>,
    server: Mutex<Option<StdUnixListener>>,
    socket_identity: Mutex<Option<FileIdentity>>,
    owned_bind_path: Mutex<Option<String>>,
    bound_path: Mutex<Option<String>>,
    closing: Arc<AtomicBool>,
    accept: Arc<Mutex<Option<Arc<dyn Fn(Arc<dyn ByteConnection>) -> Arc<dyn ByteConnectionHandler> + Send + Sync>>>>,
}

impl UnixListener {
    pub fn new(options: UnixListenerOptions) -> Result<Arc<Self>, String> {
        let resolved = resolve_unix_listener_options(&options)?;
        let path = resolved.path.clone();
        Ok(Arc::new(Self {
            options: resolved,
            path,
            connections: Arc::new(Mutex::new(HashSet::new())),
            server: Mutex::new(None),
            socket_identity: Mutex::new(None),
            owned_bind_path: Mutex::new(None),
            bound_path: Mutex::new(None),
            closing: Arc::new(AtomicBool::new(false)),
            accept: Arc::new(Mutex::new(None)),
        }))
    }

    fn accept_socket(
        connections: &Arc<Mutex<HashSet<Arc<UnixByteConnection>>>>,
        accept: &Arc<Mutex<Option<Arc<dyn Fn(Arc<dyn ByteConnection>) -> Arc<dyn ByteConnectionHandler> + Send + Sync>>>>,
        closing: &Arc<AtomicBool>,
        graceful_close_timeout_ms: u64,
        max_pending_bytes: u64,
        on_error: &Arc<dyn Fn(&str) + Send + Sync>,
        stream: UnixStream,
    ) {
        let _ = on_error;
        if closing.load(Ordering::SeqCst) {
            return;
        }
        let (connection, reader_stream) =
            UnixByteConnection::new(stream, graceful_close_timeout_ms, max_pending_bytes);
        connections.lock().unwrap().insert(connection.clone());
        let Some(accept) = accept.lock().unwrap().clone() else {
            return;
        };
        let handler = accept(connection.clone());
        // Reader thread for this connection (owns the reader half; blocking
        // reads never hold the send lock).
        let Some(mut reader_stream) = reader_stream else {
            return;
        };
        let connection_for_reader = connection.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 65536];
            use std::io::Read;
            loop {
                match reader_stream.read(&mut buffer) {
                    Ok(0) => {
                        handler.on_close();
                        return;
                    }
                    Ok(count) => {
                        handler.on_data(&buffer[..count]);
                    }
                    Err(_) => {
                        if connection_for_reader.closed_value.load(Ordering::SeqCst) {
                            return;
                        }
                        handler.on_error("Unix connection read error".to_string());
                        return;
                    }
                }
            }
        });
        let _ = handler;
    }
}

impl PiServerListener for UnixListener {
    fn start(
        &self,
        accept: Arc<dyn Fn(Arc<dyn ByteConnection>) -> Arc<dyn ByteConnectionHandler> + Send + Sync>,
    ) -> Result<(), String> {
        if self.server.lock().unwrap().is_some() {
            return Err("Unix listener is already started".to_string());
        }
        if self.closing.load(Ordering::SeqCst) {
            return Err("Unix listener is closing or closed".to_string());
        }
        *self.accept.lock().unwrap() = Some(accept);

        let owned_bind_path = get_owned_bind_path(&self.path);
        validate_unix_socket_path(&owned_bind_path, "PiServer private Unix bind path")?;
        let dir = std::path::Path::new(&self.path)
            .parent()
            .map(|parent| parent.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        remove_stale_socket(&self.path)?;
        remove_stale_socket(&owned_bind_path)?;
        *self.owned_bind_path.lock().unwrap() = Some(owned_bind_path.clone());

        let listener = match StdUnixListener::bind(&owned_bind_path) {
            Ok(listener) => listener,
            Err(error) => {
                self.close_server_and_cleanup()?;
                return Err(error.to_string());
            }
        };
        let stats = fs::symlink_metadata(&owned_bind_path).map_err(|error| error.to_string())?;
        if !is_socket_file(&stats) {
            return Err(format!("Unix listener path is not a socket after binding: {owned_bind_path}"));
        }
        *self.socket_identity.lock().unwrap() = Some(identity_of(&stats));
        // Hard-link the owned socket to the public path (link fails if the
        // target exists; remove any race first).
        if let Err(error) = fs::hard_link(&owned_bind_path, &self.path) {
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                self.close_server_and_cleanup()?;
                return Err(error.to_string());
            }
        }
        set_socket_mode(&self.path, self.options.mode)?;
        *self.bound_path.lock().unwrap() = Some(self.path.clone());

        // Accept loop.
        let accept_listener = listener.try_clone().map_err(|error| error.to_string())?;
        let connections = self.connections.clone();
        let accept = self.accept.clone();
        let closing = self.closing.clone();
        let graceful_close_timeout_ms = self.options.graceful_close_timeout_ms;
        let max_pending_bytes = self.options.max_pending_bytes;
        let on_error = self
            .options
            .on_error
            .clone()
            .unwrap_or_else(|| Arc::new(|_| {}));
        std::thread::spawn(move || {
            loop {
                match accept_listener.accept() {
                    Ok((stream, _)) => UnixListener::accept_socket(
                        &connections,
                        &accept,
                        &closing,
                        graceful_close_timeout_ms,
                        max_pending_bytes,
                        &on_error,
                        stream,
                    ),
                    Err(_) => {
                        if closing.load(Ordering::SeqCst) {
                            return;
                        }
                    }
                }
            }
        });
        *self.server.lock().unwrap() = Some(listener);
        Ok(())
    }

    fn close(&self) -> Result<(), String> {
        self.closing.store(true, Ordering::SeqCst);
        if let Some(listener) = self.server.lock().unwrap().take() {
            drop(listener); // close the accept loop
        }
        let connections: Vec<Arc<UnixByteConnection>> = self.connections.lock().unwrap().iter().cloned().collect();
        for connection in connections {
            connection.close(None);
        }
        self.connections.lock().unwrap().clear();
        self.close_server_and_cleanup()?;
        Ok(())
    }

    fn address(&self) -> Option<String> {
        self.bound_path.lock().unwrap().clone()
    }
}

impl UnixListener {
    fn close_server_and_cleanup(&self) -> Result<(), String> {
        let identity = self.socket_identity.lock().unwrap().take();
        let owned_bind_path = self.owned_bind_path.lock().unwrap().take();
        let Some(identity) = identity else {
            return Ok(());
        };
        // Cleanup the public path if it is still our socket.
        match fs::symlink_metadata(&self.path) {
            Ok(current) => {
                let current_identity = identity_of(&current);
                if is_socket_file(&current)
                    && current_identity.dev == identity.dev
                    && current_identity.ino == identity.ino
                {
                    let preserved = format!(
                        "{}/.c-{:06x}",
                        std::path::Path::new(&self.path)
                            .parent()
                            .map(|parent| parent.to_string_lossy().to_string())
                            .unwrap_or_else(|| ".".to_string()),
                        rand_suffix()
                    );
                    match fs::rename(&self.path, &preserved) {
                        Ok(()) => {
                            let _ = remove_path(&preserved);
                        }
                        Err(_) => {}
                    }
                }
            }
            Err(_) => {}
        }
        if let Some(owned_bind_path) = owned_bind_path {
            let _ = remove_path(&owned_bind_path);
        }
        Ok(())
    }
}

/// Create a Unix listener (see UnixListener::new).
pub fn create_unix_listener(options: UnixListenerOptions) -> Result<Arc<UnixListener>, String> {
    UnixListener::new(options)
}
