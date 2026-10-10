//! Failures from argument checks and from the daemon HTTP API.

use std::fmt;

/// A dogfood call that did not complete.
///
/// `Invalid` is a caller mistake (empty message, unknown profile). `Daemon` is
/// transport, HTTP status, or a body the API contract did not match.
#[derive(Debug)]
pub enum DogfoodError {
    /// The tool arguments cannot be sent.
    Invalid(String),
    /// The daemon call failed or returned a body this client will not treat as success.
    Daemon(String),
}

impl fmt::Display for DogfoodError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) | Self::Daemon(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DogfoodError {}
