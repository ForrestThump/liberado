//! Library surface for the ACP bridge package.
//!
//! The agent process lives in the `liberado-acp` binary (`main.rs`). This lib
//! exists so integration tests get a normal package graph (and `CARGO_BIN_EXE_*`)
//! while keeping the crate role as a composition root.

/// Package version string (also printed by `liberado-acp --version`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
