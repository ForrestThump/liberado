//! Agent identity → directory name.
//!
//! The directory name is the hex SHA-256 of the agent id's exact bytes. The raw id is never a
//! path component, so case folding, slashes, and Windows reserved names cannot put two agents in
//! one directory.

use sha2::{Digest, Sha256};

use crate::error::WorkspaceError;

pub(crate) fn directory_name(agent_id: &str) -> Result<String, WorkspaceError> {
    if agent_id.is_empty() {
        return Err(WorkspaceError::EmptyAgentId);
    }
    if agent_id.as_bytes().contains(&0) {
        return Err(WorkspaceError::NulInAgentId);
    }
    let digest = Sha256::digest(agent_id.as_bytes());
    Ok(hex_encode(digest.as_ref()))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
