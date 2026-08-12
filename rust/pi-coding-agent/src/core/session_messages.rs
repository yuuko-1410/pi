//! Session-facing agent message bridge.
//!
//! `SessionAgentMessage` aliases the agent-core `AgentMessage`; custom roles
//! parsed from session files that have no Rust struct (unknown roles) are
//! carried by `UnknownMessage`, mirroring how JS passes arbitrary message
//! objects through. `convert_to_llm` drops them, matching the JS default arm.

use pi_agent_core::types::CustomAgentMessage;

pub type SessionAgentMessage = pi_agent_core::types::AgentMessage;

/// A message with a role unknown to the ported code. Preserved verbatim so a
/// session round-trip never loses data; excluded from LLM context (JS
/// convertToLlm default arm returns undefined).
#[derive(Clone, Debug)]
pub struct UnknownMessage {
    pub role: String,
    pub value: pi_protocol::Value,
}

impl CustomAgentMessage for UnknownMessage {}
