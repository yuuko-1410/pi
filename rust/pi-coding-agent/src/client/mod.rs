//! Programmatic client for remote sessions, port of `client/`.
//!
//! Transcript state (transcript.rs) plus a RemoteSession wrapper over the
//! pi-client (remote_session.rs).

pub mod transcript;
pub mod remote_session;

pub use remote_session::{RemoteSession, RemoteSessionLifecycle, RemoteSessionState};
pub use transcript::{
    apply_transcript_progress, apply_transcript_snapshot, create_transcript_state, select_transcript,
    TranscriptState,
};
