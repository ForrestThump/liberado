//! Failures from a private agent workspace.
//!
//! Tool dispatch turns these into the text the model sees. None of them fall back to a shared
//! directory.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("agent id is empty")]
    EmptyAgentId,
    #[error("agent id contains a NUL byte")]
    NulInAgentId,
    #[error("directory {} belongs to a different agent", .dir.display())]
    IdentityMismatch { dir: PathBuf },
    #[error("that path leaves this agent's workspace")]
    PathEscape,
    #[error("invalid workspace path: {0}")]
    BadPath(String),
    #[error(
        "refused: {adding} bytes would make this workspace {would_use} bytes, and the cap is {cap} bytes"
    )]
    OverCap {
        adding: u64,
        would_use: u64,
        cap: u64,
    },
    #[error("file is {len} bytes; a read returns at most {max} bytes")]
    ReadTooLarge { len: u64, max: u64 },
    #[error("that path is not a file")]
    NotAFile,
    #[error("that path is not a directory")]
    NotADirectory,
    #[error("refusing to delete the workspace root")]
    DeleteRoot,
    #[error("no such path")]
    NotFound,
    #[error("only http and https URLs can be downloaded")]
    UnsupportedUrl,
    #[error("{0}")]
    Io(String),
}

impl WorkspaceError {
    pub(crate) fn io(err: impl std::fmt::Display) -> Self {
        Self::Io(err.to_string())
    }
}
