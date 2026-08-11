//! Pi client errors, port of `packages/client/src/errors.ts`.

use pi_protocol::codec::ProtocolValidationError;

#[derive(Clone, Debug, PartialEq)]
pub struct PiServerError {
    pub code: String,
    pub message: String,
    pub details: Option<pi_protocol::cbor::Value>,
}

impl PiServerError {
    pub fn new(error: &pi_protocol::schemas::ProtocolError) -> Self {
        Self {
            code: error.code.as_str().to_string(),
            message: error.message.clone(),
            details: error.details.clone(),
        }
    }
}

impl std::fmt::Display for PiServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PiDisconnectedError {
    pub message: String,
}

impl PiDisconnectedError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Default for PiDisconnectedError {
    fn default() -> Self {
        Self {
            message: "Pi client is disconnected".to_string(),
        }
    }
}

impl std::fmt::Display for PiDisconnectedError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PiClientDisposedError {
    pub message: String,
}

impl std::fmt::Display for PiClientDisposedError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl Default for PiClientDisposedError {
    fn default() -> Self {
        Self {
            message: "Pi client is disposed".to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PiSessionOwnershipError {
    pub session_id: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PiSessionDetachedError {
    pub session_id: String,
    pub message: String,
}

impl PiSessionDetachedError {
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            message: format!("Session {session_id} is not attached"),
        }
    }
}

/// Client-side error union.
#[derive(Clone, Debug)]
pub enum ClientError {
    Server(PiServerError),
    Disconnected(PiDisconnectedError),
    Disposed(PiClientDisposedError),
    SessionOwnership(PiSessionOwnershipError),
    SessionDetached(PiSessionDetachedError),
    Protocol(ProtocolValidationError),
    Other(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Server(error) => write!(formatter, "{error}"),
            ClientError::Disconnected(error) => write!(formatter, "{error}"),
            ClientError::Disposed(error) => write!(formatter, "{error}"),
            ClientError::SessionOwnership(error) => write!(formatter, "{}", error.message),
            ClientError::SessionDetached(error) => write!(formatter, "{}", error.message),
            ClientError::Protocol(error) => write!(formatter, "{}", error.0),
            ClientError::Other(message) => write!(formatter, "{message}"),
        }
    }
}

impl PartialEq for ClientError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ClientError::Server(a), ClientError::Server(b)) => a == b,
            (ClientError::Disconnected(a), ClientError::Disconnected(b)) => a == b,
            (ClientError::Disposed(a), ClientError::Disposed(b)) => a == b,
            (ClientError::SessionOwnership(a), ClientError::SessionOwnership(b)) => a == b,
            (ClientError::SessionDetached(a), ClientError::SessionDetached(b)) => a == b,
            (ClientError::Protocol(a), ClientError::Protocol(b)) => a.0 == b.0,
            (ClientError::Other(a), ClientError::Other(b)) => a == b,
            _ => false,
        }
    }
}

impl From<ProtocolValidationError> for ClientError {
    fn from(error: ProtocolValidationError) -> Self {
        ClientError::Protocol(error)
    }
}

impl From<PiDisconnectedError> for ClientError {
    fn from(error: PiDisconnectedError) -> Self {
        ClientError::Disconnected(error)
    }
}

pub fn to_disconnected_error(message: &str) -> ClientError {
    ClientError::Disconnected(PiDisconnectedError::new(message))
}
