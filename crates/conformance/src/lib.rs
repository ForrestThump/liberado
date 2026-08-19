//! Tier 3 live conformance — library half of `liberado-conformance`.
//!
//! Talks HTTP only to a running daemon. Assertion logic lives here so unit tests can exercise
//! pure decisions without a box. The binary (`main.rs`) loads config, runs paths, prints JSON,
//! writes the vault report, and sets the exit code.
//!
//! Operational commands and safety rules are in `docs/impl/live-conformance.md`.

pub mod client;
pub mod config;
pub mod paths;
pub mod report;
pub mod result;

pub use config::ConformanceConfig;
pub use result::{PathId, PathResult, PathStatus, RunReport};
