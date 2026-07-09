//! Workspace and command sandbox abstractions for Liberado's Rust-native coder.
//!
//! This crate is the deterministic boundary layer: it resolves paths under a workspace root,
//! enforces command policy, caps command output, and defines the traits future Docker/remote
//! sandboxes will implement.

use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use async_trait::async_trait;
use liberado_coder_core::CommandPolicy;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{process::Command, time::timeout};

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("workspace root does not exist: {0}")]
    MissingRoot(String),
    #[error("absolute paths are not accepted: {0}")]
    AbsolutePath(String),
    #[error("path escapes workspace root: {0}")]
    PathEscape(String),
    #[error("path contains unsupported component: {0}")]
    InvalidPath(String),
    #[error("command denied by policy: {0}")]
    CommandDenied(String),
    #[error("command spawn failed: {0}")]
    Spawn(String),
    #[error("command output read failed: {0}")]
    Output(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRequest {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_max_bytes: Option<usize>,
}

impl CommandRequest {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            timeout_secs: None,
            output_max_bytes: None,
        }
    }

    pub fn command_line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run_command(&self, request: CommandRequest) -> Result<CommandOutput, SandboxError>;
}

pub trait Workspace: CommandRunner {
    fn root(&self) -> &Path;
    fn resolve_path(&self, rel_path: &str) -> Result<PathBuf, SandboxError>;
}

#[derive(Debug, Clone)]
pub struct HostWorkspace {
    root: PathBuf,
    command_policy: CommandPolicy,
}

impl HostWorkspace {
    pub fn new(
        root: impl Into<PathBuf>,
        command_policy: CommandPolicy,
    ) -> Result<Self, SandboxError> {
        let root = root.into();
        let root = root
            .canonicalize()
            .map_err(|_| SandboxError::MissingRoot(root.display().to_string()))?;
        Ok(Self {
            root,
            command_policy,
        })
    }

    pub fn command_policy(&self) -> &CommandPolicy {
        &self.command_policy
    }
}

impl Workspace for HostWorkspace {
    fn root(&self) -> &Path {
        &self.root
    }

    fn resolve_path(&self, rel_path: &str) -> Result<PathBuf, SandboxError> {
        let path = Path::new(rel_path);
        if path.is_absolute() {
            return Err(SandboxError::AbsolutePath(rel_path.to_string()));
        }

        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => {}
                Component::ParentDir => return Err(SandboxError::PathEscape(rel_path.to_string())),
                _ => return Err(SandboxError::InvalidPath(rel_path.to_string())),
            }
        }

        let resolved = self.root.join(normalized);
        if !resolved.starts_with(&self.root) {
            return Err(SandboxError::PathEscape(rel_path.to_string()));
        }
        Ok(resolved)
    }
}

#[async_trait]
impl CommandRunner for HostWorkspace {
    async fn run_command(&self, request: CommandRequest) -> Result<CommandOutput, SandboxError> {
        ensure_command_allowed(&self.command_policy, &request)?;

        let mut command = Command::new(&request.program);
        command.args(&request.args).current_dir(&self.root);
        command.kill_on_drop(true);
        for (key, value) in &request.env {
            command.env(key, value);
        }

        let timeout_secs = request
            .timeout_secs
            .unwrap_or(self.command_policy.timeout_secs);
        let output_result = timeout(Duration::from_secs(timeout_secs), command.output()).await;
        let (exit_code, stdout, stderr, timed_out) = match output_result {
            Ok(Ok(output)) => (output.status.code(), output.stdout, output.stderr, false),
            Ok(Err(e)) => return Err(SandboxError::Spawn(e.to_string())),
            Err(_) => (None, Vec::new(), Vec::new(), true),
        };

        let max = request
            .output_max_bytes
            .unwrap_or(self.command_policy.output_max_bytes);
        let stdout = capped_utf8(stdout, max);
        let stderr = capped_utf8(stderr, max);

        Ok(CommandOutput {
            exit_code,
            stdout,
            stderr,
            timed_out,
        })
    }
}

pub fn ensure_command_allowed(
    policy: &CommandPolicy,
    request: &CommandRequest,
) -> Result<(), SandboxError> {
    let line = request.command_line();
    if policy.deny.iter().any(|rule| command_matches(rule, &line)) {
        return Err(SandboxError::CommandDenied(line));
    }
    if !policy.allow.is_empty() && !policy.allow.iter().any(|rule| command_matches(rule, &line)) {
        return Err(SandboxError::CommandDenied(line));
    }
    Ok(())
}

fn command_matches(rule: &str, command_line: &str) -> bool {
    let rule = rule.trim();
    !rule.is_empty() && (command_line == rule || command_line.starts_with(&format!("{rule} ")))
}

fn capped_utf8(mut buf: Vec<u8>, max_bytes: usize) -> String {
    if buf.len() > max_bytes {
        buf.truncate(max_bytes);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (tempfile::TempDir, HostWorkspace) {
        let dir = tempfile::tempdir().unwrap();
        let ws = HostWorkspace::new(dir.path(), CommandPolicy::default()).unwrap();
        (dir, ws)
    }

    #[test]
    fn resolve_path_rejects_parent_escape() {
        let (_dir, ws) = workspace();
        let err = ws.resolve_path("../Cargo.toml").unwrap_err();
        assert!(matches!(err, SandboxError::PathEscape(_)));
    }

    #[test]
    fn resolve_path_rejects_absolute_path() {
        let (_dir, ws) = workspace();
        let err = ws.resolve_path("C:/Windows").unwrap_err();
        assert!(matches!(err, SandboxError::AbsolutePath(_)));
    }

    #[test]
    fn resolve_path_accepts_nested_relative_path() {
        let (_dir, ws) = workspace();
        let path = ws.resolve_path("src/lib.rs").unwrap();
        assert!(path.ends_with(Path::new("src/lib.rs")));
        assert!(path.starts_with(ws.root()));
    }

    #[test]
    fn command_policy_denies_prefix_match() {
        let policy = CommandPolicy {
            allow: vec!["cargo test".to_string()],
            deny: vec!["cargo publish".to_string()],
            ..CommandPolicy::default()
        };
        let mut request = CommandRequest::new("cargo");
        request.args = vec!["publish".to_string(), "--dry-run".to_string()];
        assert!(ensure_command_allowed(&policy, &request).is_err());
    }

    #[test]
    fn command_policy_allows_configured_prefix() {
        let policy = CommandPolicy {
            allow: vec!["cargo test".to_string()],
            ..CommandPolicy::default()
        };
        let mut request = CommandRequest::new("cargo");
        request.args = vec!["test".to_string(), "-p".to_string(), "x".to_string()];
        assert!(ensure_command_allowed(&policy, &request).is_ok());
    }

    #[test]
    fn capped_utf8_truncates_large_output() {
        let text = capped_utf8(b"abcdef".to_vec(), 3);
        assert_eq!(text, "abc");
    }
}
