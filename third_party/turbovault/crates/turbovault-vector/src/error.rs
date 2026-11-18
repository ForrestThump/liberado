/// Guard a method body behind `#[cfg(feature = "local")]`, returning an error
/// when the feature is disabled.
///
/// # Usage
///
/// ```ignore
/// fn do_thing(&self, path: &str) -> Result<(), VectorError> {
///     require_feature!(Database, path);
///
///     #[cfg(feature = "local")]
///     { /* real implementation */ }
/// }
/// ```
#[macro_export]
macro_rules! require_feature {
    ($variant:ident) => {
        #[cfg(not(feature = "local"))]
        {
            return Err(VectorError::$variant(
                "vector-search feature not compiled in".to_string(),
            ));
        }
    };
    ($variant:ident, $($param:expr),+ $(,)?) => {
        #[cfg(not(feature = "local"))]
        {
            let _ = ($($param),+);
            return Err(VectorError::$variant(
                "vector-search feature not compiled in".to_string(),
            ));
        }
    };
}

use thiserror::Error;

#[derive(Debug, Error)]
pub enum VectorError {
    #[error("vector index error: {0}")]
    Index(String),
    #[error("embedding error: {0}")]
    Embedding(String),
    #[error("database error: {0}")]
    Database(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("config error: {0}")]
    Config(String),
    #[error("rerank error: {0}")]
    Rerank(String),
}
