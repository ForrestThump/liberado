//! Re-export of `turbovault_sql` types for convenience.
//!
//! The SQL engine implementation lives in the `turbovault-sql` crate.
//! This module re-exports the public API when the `sql` feature is enabled.

pub use turbovault_sql::{FrontmatterSqlEngine, SqlSession};
