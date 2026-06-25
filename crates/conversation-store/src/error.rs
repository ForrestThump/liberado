//! The error surface every [`ConversationStore`](crate::ConversationStore) method shares.

use thiserror::Error;

/// What can go wrong reading or writing the conversation log.
///
/// The two domain-specific variants are the interesting ones: [`NotFound`](StoreError::NotFound)
/// distinguishes "this conversation/node does not exist" from an IO failure, and
/// [`Corrupt`](StoreError::Corrupt) is raised — never swallowed — when a log file violates its own
/// invariants (a first line that isn't a header, or a non-empty line that won't parse). A reader
/// that silently dropped a malformed line would hand the caller a truncated history that looks
/// complete, so we surface it instead.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("conversation store IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("conversation store serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    /// A referenced conversation or node does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// A log file violated its on-disk invariants (bad header line, unparseable record, or a
    /// malformed parent cycle).
    #[error("corrupt conversation log: {0}")]
    Corrupt(String),
}

/// The result type shared by every store operation.
pub type StoreResult<T> = Result<T, StoreError>;
