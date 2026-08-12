//! Public library entry, port of `index.ts`.
//!
//! Re-exports the SDK surface used by hosts (extensions, tests, binaries).

pub use crate::core::agent_session::{AgentSession, AgentSessionEvent};
pub use crate::core::model_runtime::ModelRuntime;
pub use crate::core::session_manager::SessionManager;
pub use crate::core::settings_manager::SettingsManager;
pub use crate::core::sdk::{create_agent_session, CreateAgentSessionOptions, CreateAgentSessionResult};
pub use crate::core::slash_commands::{builtin_command_names, SlashCommandInfo};
pub use crate::modes::interactive::interactive_mode::{InteractiveMode, InteractiveModeOptions};
