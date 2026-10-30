use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("outbox database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("outbox time: {0}")]
    Time(String),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;
