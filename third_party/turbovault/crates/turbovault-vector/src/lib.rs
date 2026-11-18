//! Dense-vector semantic search for TurboVault.
//!
//! Provides HNSW-based nearest-neighbour retrieval using local ONNX embeddings
//! (fastembed) and usearch, with SQLite-backed incremental index management.
//!
//! This crate is only compiled when the `vector-search` feature is enabled on
//! the top-level `turbovault` binary.  All heavy dependencies (fastembed, usearch,
//! rusqlite) are behind the `local` feature which is on by default.

pub mod build;
pub mod chunks;
pub mod config;
pub mod embedding;
pub mod error;
pub mod index;
pub mod router;

pub use build::IndexBuilder;
pub use chunks::ChunkStore;
pub use config::VectorConfig;
pub use embedding::{EmbeddingEngine, FastembedEngine, FastembedReranker, Reranker};
pub use error::VectorError;
pub use index::VectorIndex;
pub use router::{HybridResult, SearchRouter, VectorResult};
