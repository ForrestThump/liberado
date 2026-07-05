use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("vault error: {0}")]
    Vault(#[from] liberado_vault::VaultError),
    #[error("vector index error: {0}")]
    Vector(#[from] turbovault_vector::VectorError),
    #[error("note has no YAML frontmatter")]
    MissingFrontmatter,
    #[error("malformed memory note frontmatter: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("memory not found: {0}")]
    NotFound(String),
    #[error("invalid memory id: {0}")]
    InvalidId(String),
}
