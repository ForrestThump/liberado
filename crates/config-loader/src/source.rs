//! The [`ConfigSource`] trait and its associated error type.
//!
//! Any component that can provide raw TOML content — a file on disk, an environment
//! variable, a compiled-in default — implements this trait. Returning `Ok(None)` signals
//! "this source intentionally provides nothing" (absent file, unset env var), which the
//! [`ChainLoader`](crate::ChainLoader) skips rather than treating as an error.

use std::fmt::Debug;

use thiserror::Error;

/// A source of raw TOML configuration content.
///
/// Implementations must be [`Debug`] + [`Send`] + [`Sync`] so they can be stored as
/// trait objects in [`ChainLoader`](crate::ChainLoader).
pub trait ConfigSource: Debug + Send + Sync {
    /// Load the raw TOML string from this source.
    ///
    /// Returns:
    /// - `Ok(Some(content))` — the source has content.
    /// - `Ok(None)` — the source is intentionally absent (file not found, env var unset).
    /// - `Err(e)` — a hard error that should halt loading (permissions, malformed content).
    fn load_raw(&self) -> Result<Option<String>, ConfigLoadError>;

    /// A human-readable description of this source for diagnostics/provenance
    /// (e.g. a file path or env var name).
    fn description(&self) -> &str;
}

/// Errors that can occur when loading config from a [`ConfigSource`] or merging
/// sources in a [`ChainLoader`](crate::ChainLoader).
#[derive(Debug, Error)]
pub enum ConfigLoadError {
    /// An I/O error reading from the source (permissions, broken storage, …).
    /// An absent file is *not* an error — it is signalled via
    /// [`ConfigSource::load_raw`] returning `Ok(None)`.
    #[error("I/O error from '{source}': {inner}")]
    Io {
        /// A label identifying the source that failed.
        source: String,
        /// The underlying I/O error.
        #[source]
        inner: std::io::Error,
    },

    /// The content read from the source is not valid TOML (or cannot be deserialized
    /// into the target type).
    #[error("parse error from '{source}': {inner}")]
    Parse {
        /// A label identifying the source that failed.
        source: String,
        /// The underlying TOML/deserialization error.
        #[source]
        inner: toml::de::Error,
    },

    /// The merged config failed cross-cutting validation (dangling references, missing
    /// secrets, etc.). The message names the offending entry.
    #[error("{0}")]
    Validation(String),
}
