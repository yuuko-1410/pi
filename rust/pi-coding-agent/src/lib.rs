//! Pi coding agent, port of `packages/coding-agent`.
//!
//! Synchronous analog: child processes use std::process::Command; promises
//! become blocking calls; AbortSignal parameters are omitted (callers
//! control threads).

pub mod core;
pub mod modes;
pub mod config;
pub mod migrations;
pub mod utils;
pub mod app;
pub mod cli;

