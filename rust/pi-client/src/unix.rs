//! Unix-domain socket transport for PiClient, port of
//! `packages/client/src/unix.ts`.
//!
//! Reader thread decodes inbound bytes; writes are synchronous and ordered.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::connection::{ByteTransport, ByteTransportFactory, ByteTransportHandlers, DEFAULT_MAX_FRAME_LENGTH};
use crate::errors::{to_disconnected_error, ClientError};

#[derive(Clone, Debug)]
pub struct UnixTransportOptions {
    pub path: String,
    pub max_pending_bytes: Option<u64>,
}

pub struct UnixTransportFactory {
    options: UnixTransportOptions,
}

impl UnixTransportFactory {
    pub fn new(options: UnixTransportOptions) -> Result<Self, String> {
        if options.path.is_empty() {
            return Err("Unix transport path must not be empty".to_string());
        }
        if options.path.len() > 107 {
            return Err(format!(
                "Unix transport path is too long; maximum is 107 UTF-8 bytes"
            ));
        }
        let max_pending_bytes = options.max_pending_bytes.unwrap_or(DEFAULT_MAX_FRAME_LENGTH * 4);
        if max_pending_bytes < 1 {
            return Err("Unix transport maxPendingBytes must be a positive safe integer".to_string());
        }
        Ok(Self {
            options: UnixTransportOptions {
                max_pending_bytes: Some(max_pending_bytes),
                ..options
            },
        })
    }
}

impl ByteTransportFactory for UnixTransportFactory {
    fn connect_transport(
        &self,
        handlers: Arc<dyn ByteTransportHandlers>,
    ) -> Result<Arc<dyn ByteTransport>, ClientError> {
        let stream = UnixStream::connect(&self.options.path)
            .map_err(|error| to_disconnected_error(&format!("{error}")))?;
        stream.set_nonblocking(false).map_err(|error| to_disconnected_error(&format!("{error}")))?;
        let reader_stream = stream.try_clone().map_err(|error| to_disconnected_error(&format!("{error}")))?;
        let transport = Arc::new(UnixByteTransport {
            stream: Mutex::new(stream),
            max_pending_bytes: self.options.max_pending_bytes.unwrap_or(DEFAULT_MAX_FRAME_LENGTH * 4),
            closed: Arc::new(AtomicBool::new(false)),
            pending_bytes: Mutex::new(0u64),
        });
        let mut reader = ReaderThread {
            stream: reader_stream,
            closed: transport.closed.clone(),
        };
        std::thread::spawn(move || {
            reader.read_loop(handlers);
        });
        Ok(transport)
    }
}

struct ReaderThread {
    stream: UnixStream,
    closed: Arc<AtomicBool>,
}

impl ReaderThread {
    fn read_loop(&mut self, handlers: Arc<dyn ByteTransportHandlers>) {
        let mut buffer = [0u8; 65536];
        loop {
            match self.stream.read(&mut buffer) {
                Ok(0) => {
                    handlers.on_close();
                    return;
                }
                Ok(count) => handlers.on_data(&buffer[..count]),
                Err(error) => {
                    if self.closed.load(Ordering::SeqCst) {
                        return;
                    }
                    handlers.on_error(to_disconnected_error(&format!("{error}")));
                    return;
                }
            }
        }
    }
}

struct UnixByteTransport {
    stream: Mutex<UnixStream>,
    max_pending_bytes: u64,
    closed: Arc<AtomicBool>,
    pending_bytes: Mutex<u64>,
}

impl ByteTransport for UnixByteTransport {
    fn send(&self, chunk: &[u8]) -> Result<(), ClientError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(to_disconnected_error("Unix transport is closed"));
        }
        let mut pending = self.pending_bytes.lock().unwrap();
        if *pending + chunk.len() as u64 > self.max_pending_bytes {
            return Err(to_disconnected_error("Unix transport exceeded its pending byte limit"));
        }
        *pending += chunk.len() as u64;
        let result = self
            .stream
            .lock()
            .unwrap()
            .write_all(chunk)
            .map_err(|error| to_disconnected_error(&format!("{error}")));
        *pending -= chunk.len() as u64;
        result
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.stream.lock().unwrap().shutdown(std::net::Shutdown::Both);
    }
}
