//! Vault adapter errors.
//!
//! This is also the **isolation point** for the Decision 5 §8.1 fallback ladder: today
//! Turbovault surfaces optimistic-concurrency failures as a stringly-typed
//! `Error::ConcurrencyError { reason }`. We convert that into a typed [`VaultError::Conflict`]
//! here, in one place, so callers branch on a real variant. If/when upstream adds a structured
//! `ConcurrentModification`, only this conversion changes.

use thiserror::Error;

/// Errors from the vault adapter.
#[derive(Debug, Error)]
pub enum VaultError {
    /// An optimistic-concurrency check failed: the file changed since it was read. The reason
    /// string carries the expected/actual hashes. Callers re-read and retry (bounded).
    #[error("optimistic concurrency conflict: {0}")]
    Conflict(String),

    /// Any other failure from the Turbovault backend.
    #[error("vault backend error: {0}")]
    Backend(String),
}

impl From<turbovault_core::error::Error> for VaultError {
    fn from(e: turbovault_core::error::Error) -> Self {
        match e {
            turbovault_core::error::Error::ConcurrencyError { reason } => Self::Conflict(reason),
            other => Self::Backend(other.to_string()),
        }
    }
}

/// Convenience alias for vault adapter results.
pub type VaultResult<T> = Result<T, VaultError>;
