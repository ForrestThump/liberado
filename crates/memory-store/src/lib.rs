//! Native-Rust, vault-backed memory storage for `liberado-memory-mcp` and the dispatcher.
//!
//! Two isolated stores share this one implementation: **general** memory (user facts and
//! preferences) and **procedural** memory (tool-selection guidance). Cleartext markdown notes are
//! the source of truth (human-readable, git-diffable, editable in Obsidian); each store also
//! maintains a `turbovault-vector` HNSW index scoped to just its own subdirectory for semantic
//! recall. Replaces the old `liberado-tool-helper-mcp`, which proxied every call over HTTP to an
//! external mem0 service — this crate has no such dependency.

mod error;
mod note;
mod store;

pub use error::MemoryError;
pub use note::MemoryNote;
pub use store::{DEFAULT_DEDUP_THRESHOLD, MemoryResult, MemoryStore, MemoryStoreConfig};
