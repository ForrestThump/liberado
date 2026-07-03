//! # liberado-config-loader
//!
//! A thin config-loading foundation: [`ConfigSource`] trait and [`ChainLoader`] that
//! merges multiple sources in precedence order (see `docs/specs/liberado-config-spec.md`).
//!
//! The trait is generic — any source of TOML content (file, env var, compiled-in default)
//! implements it. [`ChainLoader`] loads each source in sequence and merges the results at
//! the TOML table/key level (later sources override earlier ones). The merged value
//! deserializes into the target config model.
//!
//! ## Usage
//!
//! ```rust
//! use liberado_config_loader::{ConfigSource, ChainLoader, FileSource};
//! use std::path::PathBuf;
//!
//! let loader = ChainLoader::new()
//!     .add_source(Box::new(FileSource::new(PathBuf::from("/etc/liberado/topology.toml"))))
//!     .add_source(Box::new(FileSource::new(PathBuf::from("./config/topology.toml"))));
//! ```

mod chain;
mod file_source;
mod source;
mod validation;

pub use chain::ChainLoader;
pub use file_source::FileSource;
pub use source::{ConfigLoadError, ConfigSource};
pub use validation::validate_merged_config;
