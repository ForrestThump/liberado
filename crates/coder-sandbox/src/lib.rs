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
use liberado_coder_core::{CommandPolicy, DockerSandboxSpec, SandboxVolume};
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
    #[error("invalid docker sandbox config: {0}")]
    InvalidDockerConfig(String),
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

#[derive(Debug, Clone)]
pub struct DockerWorkspace {
    host: HostWorkspace,
    spec: DockerSandboxSpec,
}

impl DockerWorkspace {
    pub fn new(
        root: impl Into<PathBuf>,
        spec: DockerSandboxSpec,
        command_policy: CommandPolicy,
    ) -> Result<Self, SandboxError> {
        Ok(Self {
            host: HostWorkspace::new(root, command_policy)?,
            spec,
        })
    }

    pub fn docker_run_args(&self, request: &CommandRequest) -> Result<Vec<String>, SandboxError> {
        ensure_command_allowed(self.host.command_policy(), request)?;
        if self.spec.image.trim().is_empty() {
            return Err(SandboxError::InvalidDockerConfig(
                "docker image must not be empty".to_string(),
            ));
        }

        let mut args = vec![
            "run".to_string(),
            "--rm".to_string(),
            "-i".to_string(),
            "-v".to_string(),
            docker_volume_arg(self.host.root(), "/workspace", false),
            "-w".to_string(),
            "/workspace".to_string(),
        ];
        if let Some(network) = &self.spec.network {
            args.push("--network".to_string());
            args.push(network.clone());
        }
        if let Some(user) = &self.spec.user {
            args.push("--user".to_string());
            args.push(user.clone());
        }
        for key in &self.spec.env_allowlist {
            args.push("-e".to_string());
            args.push(key.clone());
        }
        for (key, value) in &request.env {
            args.push("-e".to_string());
            args.push(format!("{key}={value}"));
        }
        for volume in &self.spec.volumes {
            args.push("-v".to_string());
            args.push(sandbox_volume_arg(volume));
        }
        args.push(self.spec.image.clone());
        args.push(request.program.clone());
        args.extend(request.args.clone());
        Ok(args)
    }
}

impl Workspace for DockerWorkspace {
    fn root(&self) -> &Path {
        self.host.root()
    }

    fn resolve_path(&self, rel_path: &str) -> Result<PathBuf, SandboxError> {
        self.host.resolve_path(rel_path)
    }
}

#[async_trait]
impl CommandRunner for DockerWorkspace {
    async fn run_command(&self, request: CommandRequest) -> Result<CommandOutput, SandboxError> {
        let docker_args = self.docker_run_args(&request)?;
        let mut command = Command::new("docker");
        command.args(&docker_args);
        command.kill_on_drop(true);

        let timeout_secs = request
            .timeout_secs
            .unwrap_or(self.host.command_policy().timeout_secs);
        let output_result = timeout(Duration::from_secs(timeout_secs), command.output()).await;
        let (exit_code, stdout, stderr, timed_out) = match output_result {
            Ok(Ok(output)) => (output.status.code(), output.stdout, output.stderr, false),
            Ok(Err(e)) => return Err(SandboxError::Spawn(e.to_string())),
            Err(_) => (None, Vec::new(), Vec::new(), true),
        };

        let max = request
            .output_max_bytes
            .unwrap_or(self.host.command_policy().output_max_bytes);
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

fn docker_volume_arg(host: &Path, container: &str, read_only: bool) -> String {
    let mut arg = format!("{}:{container}", docker_path(host));
    if read_only {
        arg.push_str(":ro");
    }
    arg
}

fn sandbox_volume_arg(volume: &SandboxVolume) -> String {
    let mut arg = format!(
        "{}:{}",
        normalize_docker_path(&volume.host),
        normalize_docker_path(&volume.container)
    );
    if volume.read_only {
        arg.push_str(":ro");
    }
    arg
}

fn docker_path(path: &Path) -> String {
    normalize_docker_path(&path.to_string_lossy())
}

fn normalize_docker_path(path: &str) -> String {
    path.replace('\\', "/")
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

    /// What counts as absolute is platform-specific, and the guard is `Path::is_absolute`, so the
    /// test has to speak the host's dialect. `C:/Windows` is absolute on Windows but an ordinary
    /// relative name on Unix — hardcoding it passed here and failed on Linux, where the path was
    /// simply joined onto the root instead of refused.
    #[test]
    fn resolve_path_rejects_absolute_path() {
        let (_dir, ws) = workspace();
        let absolute = if cfg!(windows) { "C:/Windows" } else { "/etc" };
        let err = ws.resolve_path(absolute).unwrap_err();
        assert!(matches!(err, SandboxError::AbsolutePath(_)));
    }

    /// The other half of that asymmetry: on Unix a drive-letter path is not absolute, so it is
    /// treated as an ordinary relative name. That is safe — it still lands inside the root — but
    /// pinning it keeps the behaviour deliberate rather than incidental.
    #[cfg(unix)]
    #[test]
    fn drive_letter_path_is_contained_on_unix() {
        let (_dir, ws) = workspace();
        let path = ws.resolve_path("C:/Windows").unwrap();
        assert!(path.starts_with(ws.root()));
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

    #[test]
    fn capped_utf8_passes_through_at_exact_boundary() {
        let text = capped_utf8(b"abc".to_vec(), 3);
        assert_eq!(text, "abc");
    }

    #[test]
    fn capped_utf8_passes_through_below_boundary() {
        let text = capped_utf8(b"ab".to_vec(), 3);
        assert_eq!(text, "ab");
    }

    #[test]
    fn resolve_path_accepts_curdir_prefix() {
        let (_dir, ws) = workspace();
        let path = ws.resolve_path("./src/lib.rs").unwrap();
        assert!(path.ends_with(Path::new("src/lib.rs")));
    }

    #[test]
    fn resolve_path_accepts_intermediate_curdir() {
        let (_dir, ws) = workspace();
        let path = ws.resolve_path("src/./lib.rs").unwrap();
        assert!(path.ends_with(Path::new("lib.rs")));
    }

    #[test]
    fn docker_workspace_builds_docker_run_args() {
        let dir = tempfile::tempdir().unwrap();
        let ws = DockerWorkspace::new(
            dir.path(),
            DockerSandboxSpec {
                image: "liberado-coder:latest".to_string(),
                network: Some("none".to_string()),
                env_allowlist: vec!["OPENROUTER_API_KEY".to_string()],
                volumes: vec![SandboxVolume {
                    host: "C:\\cache".to_string(),
                    container: "/cache".to_string(),
                    read_only: true,
                }],
                user: Some("1000:1000".to_string()),
            },
            CommandPolicy::default(),
        )
        .unwrap();
        let mut request = CommandRequest::new("cargo");
        request.args = vec!["test".to_string()];
        request
            .env
            .insert("RUST_LOG".to_string(), "info".to_string());

        let args = ws.docker_run_args(&request).unwrap();

        assert_eq!(args[0], "run");
        assert!(args.contains(&"--rm".to_string()));
        assert!(args.contains(&"--network".to_string()));
        assert!(args.contains(&"none".to_string()));
        assert!(args.contains(&"--user".to_string()));
        assert!(args.contains(&"1000:1000".to_string()));
        assert!(args.contains(&"OPENROUTER_API_KEY".to_string()));
        assert!(args.contains(&"RUST_LOG=info".to_string()));
        assert!(args.contains(&"C:/cache:/cache:ro".to_string()));
        assert_eq!(
            args.iter().rev().take(3).cloned().collect::<Vec<_>>(),
            vec![
                "test".to_string(),
                "cargo".to_string(),
                "liberado-coder:latest".to_string()
            ]
        );

        // Workspace volume mount at args[4] must reference /workspace.
        assert!(args[4].contains(":/workspace"), "volume mount should reference /workspace, got: {0}", args[4]);
        assert!(!args[4].contains(":ro"), "volume mount should not be read-only, got: {0}", args[4]);
        let normalized_root = ws.root().to_string_lossy().replace('\\', "/");
        assert!(args[4].starts_with(&normalized_root), "volume mount should start with host root, got: {0}", args[4]);
    }

    #[test]
    fn docker_workspace_resolve_path_delegates_to_host() {
        let dir = tempfile::tempdir().unwrap();
        let ws = DockerWorkspace::new(
            dir.path(),
            DockerSandboxSpec {
                image: "liberado-coder:latest".to_string(),
                network: None,
                env_allowlist: Vec::new(),
                volumes: Vec::new(),
                user: None,
            },
            CommandPolicy::default(),
        )
        .unwrap();
        let path = ws.resolve_path("src/lib.rs").unwrap();
        assert!(path.ends_with(Path::new("src/lib.rs")));
        assert!(path.starts_with(ws.root()));
    }

    #[test]
    fn docker_workspace_obeys_command_policy() {
        let dir = tempfile::tempdir().unwrap();
        let ws = DockerWorkspace::new(
            dir.path(),
            DockerSandboxSpec {
                image: "liberado-coder:latest".to_string(),
                network: None,
                env_allowlist: Vec::new(),
                volumes: Vec::new(),
                user: None,
            },
            CommandPolicy {
                allow: vec!["cargo test".to_string()],
                ..CommandPolicy::default()
            },
        )
        .unwrap();
        let mut request = CommandRequest::new("cargo");
        request.args = vec!["publish".to_string()];

        let err = ws.docker_run_args(&request).unwrap_err();

        assert!(matches!(err, SandboxError::CommandDenied(_)));
    }
}
