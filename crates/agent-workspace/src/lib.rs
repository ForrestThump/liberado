//! Private file workspace for one agent.
//!
//! Each agent id resolves to its own directory. The directory name is a hash of the id's exact
//! bytes, so two agents cannot land in the same directory through case folding, slashes, or
//! Windows path rules. A marker file records the id and refuses a directory that was bound to a
//! different id.
//!
//! The default cap is 1 GiB ([`DEFAULT_CAP_BYTES`]). Each file, directory, and symlink costs 4096
//! bytes plus the file's length. Writes and downloads that would pass the cap are refused, including
//! an empty file when the cap is 0, and a refused download leaves no file behind. A download
//! redirect must stay on http or https. Every tool path is resolved inside that agent's `files/`
//! directory.
//!
//! This is not a shared scratch directory. There is no path that every agent can write. Agents
//! share through a channel or a local git repository.

mod entries;
mod error;
mod id;
mod quota;
mod sandbox;
mod tools;
mod workspace;

pub use error::WorkspaceError;
pub use tools::{
    TOOL_DELETE, TOOL_DOWNLOAD, TOOL_LIST, TOOL_READ, TOOL_WRITE, WORKSPACE_TOOL_NAMES,
    WorkspaceRuntime, apply, is_workspace_tool, tool_defs,
};
pub use workspace::{
    AgentWorkspace, DEFAULT_CAP_BYTES, DEFAULT_DIR_NAME, DirEntry, EntryKind, MAX_READ_BYTES,
    WorkspaceSettings,
};

#[cfg(test)]
#[path = "sandbox_tests.rs"]
mod sandbox_tests;

#[cfg(test)]
#[path = "symlink_tests.rs"]
mod symlink_tests;

#[cfg(test)]
#[path = "quota_tests.rs"]
mod quota_tests;

#[cfg(test)]
mod tests;
