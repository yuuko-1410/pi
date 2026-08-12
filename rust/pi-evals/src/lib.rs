//! Pi evals support, port of `packages/evals`.
//!
//! The vitest-coupled pieces (harness-table, artifacts, reporter) depend on
//! the vitest-evals runtime and are not portable; the comparison summary
//! engine is fully ported.

pub mod summary;
