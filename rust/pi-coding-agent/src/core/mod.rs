//! Core coding-agent modules (port of `packages/coding-agent/src/core/`).

pub mod defaults;
pub mod event_bus;
pub mod extensions;
pub mod messages;
pub mod output_guard;
pub mod pi_manifest;
pub mod radius;
pub mod session_cwd;
pub mod session_manager;
pub mod session_messages;
pub mod session_paths;
pub mod session_types;
pub mod settings_manager;
pub mod resolve_config_value;
pub mod auth_storage;
pub mod timings;
pub mod runtime_credentials;
pub mod experimental;
pub mod diagnostics;
pub mod usage_totals;
pub mod cache_stats;
pub mod tools;
pub mod auth_guidance;
