//! The shared error type for `liberado-common`.
//!
//! These are *type-level* errors raised by the pure operations in this crate (capability
//! checks, config validation, (de)serialization). Runtime/I/O errors belong to the crates
//! that perform I/O, which wrap or convert these as needed.

use thiserror::Error;

/// Errors produced by `liberado-common` operations.
#[derive(Debug, Error)]
pub enum Error {
    /// A capability check failed: the subject's `CapabilitySet` does not grant the
    /// requested action. This is the Decision 4 containment boundary saying "no".
    #[error("capability denied: {0}")]
    CapabilityDenied(String),

    /// A zone referenced by a capability or write-class is not defined in policy.
    #[error("unknown zone: {0}")]
    UnknownZone(String),

    /// Config failed cross-cutting validation (Decision 14 fail-fast contract).
    #[error("invalid config: {0}")]
    Config(String),

    /// A model was assigned to a role whose capability floor it does not meet
    /// (Decision 13).
    #[error("model '{model}' does not meet the capability floor for role {role}")]
    ModelCapabilityFloor { model: String, role: String },

    /// JSON (de)serialization failed.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Convenience alias for results in this crate.
pub type Result<T> = std::result::Result<T, Error>;
