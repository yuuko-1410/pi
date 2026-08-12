//! Server-side harness adapters, port of `server/`.

pub mod harness;

pub use harness::{
    build_coding_agent_harness_system_prompt, create_coding_agent_harness,
    default_harness_tools, CodingAgentHarnessTool, CreateCodingAgentHarnessOptions,
};
