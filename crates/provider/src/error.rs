//! Provider errors and the runtime-resilience contract.
//!
//! Decision 13 requires that malformed structured output never crashes the system: it is
//! treated like low confidence → bounded retry/repair → escalate or `Clarify`. These variants
//! are the typed signals callers (the dispatcher) branch on to implement that.

use thiserror::Error;

/// An error from an inference provider.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// Network/transport failure talking to the provider.
    #[error("provider transport error: {0}")]
    Transport(String),

    /// The provider returned a response with no usable content.
    #[error("provider returned no content")]
    EmptyResponse,

    /// Structured output failed to parse into the requested type. Recoverable: the dispatcher
    /// re-prompts with the schema or escalates to a stricter model (Decision 13).
    #[error("failed to decode structured output: {0}")]
    Decode(String),

    /// The provider rejected the request (bad params, unsupported feature, etc.).
    #[error("provider rejected request: {0}")]
    InvalidRequest(String),

    /// Rate limited; the caller should back off and retry.
    #[error("provider rate limited")]
    RateLimited,

    /// A [`crate::MockProvider`] ran out of scripted responses (test-only condition).
    #[error("mock provider exhausted: no scripted response remaining")]
    MockExhausted,
}

/// Convenience alias for provider results.
pub type ProviderResult<T> = Result<T, ProviderError>;
