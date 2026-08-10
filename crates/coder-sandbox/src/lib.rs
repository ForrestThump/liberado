//! Workspace and command sandbox abstractions for Liberado's Rust-native coder.
//!
//! This crate is the deterministic boundary layer: it resolves paths under a workspace root,
//! enforces command policy, caps command output, and defines the traits future Docker/remote
//! sandboxes will implement.

mod checkpoint;
mod merge;
mod path_deps;
mod preflight;
mod preflight_baseline;
pub mod warmup;
pub use checkpoint::{Checkpoint, CheckpointError, ShadowGit};
pub use merge::{
    ConflictSides, MergeAttempt, MergeError, add_worktree_on_branch, branch_tip, commit_merge,
    list_unmerged_paths, merge_branch, read_conflict_sides, remove_worktree,
    rev_parse as git_rev_parse, stage_resolution,
};
pub use preflight::{
    DEFAULT_LOG_CAP_BYTES, DEFAULT_STEP_TIMEOUT_SECS, FailureSet, OPAQUE_FAILURE, PreflightError,
    PreflightReport, PreflightSpec, PreflightStep, PreflightStepResult, describe_failures,
    diff_against_baseline, failure_identities, liberado_ship_preflight_spec,
    liberado_ship_preflight_steps, report_failures, resolve_ship_spec, run_preflight,
    run_preflight_with_options,
};
pub use preflight_baseline::{
    BaselineOptions, baseline_cache_path, compute_baseline, load_baseline, store_baseline,
};
// Durable session worktree helpers are defined below next to WorktreeWorkspace.

use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use async_trait::async_trait;
use liberado_coder_core::{CommandPolicy, DockerSandboxSpec, SandboxVolume};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::time::timeout;

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
        // Windows `canonicalize` yields `\\?\C:\...` which breaks `git -C` / `current_dir`
        // (git rewrites it to `//?/C:/...` and fails with "Invalid argument"). Strip for CLI use.
        let root = strip_extended_path_prefix(&root);
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
        let mut command = liberado_common::process::command("docker");
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

/// Strip Windows extended-length prefixes so paths can be passed to git and other CLIs.
///
/// `std::fs::canonicalize` on Windows returns `\\?\C:\...` (or `\\?\UNC\...`). Git for Windows
/// turns that into `//?/C:/...` and fails with "could not create leading directories … Invalid
/// argument". Host `current_dir` is similarly happier with a plain drive path.
///
/// Idempotent on non-Windows / already-stripped paths.
pub fn strip_extended_path_prefix(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if let Some(unc) = rest.strip_prefix(r"UNC\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        return PathBuf::from(rest);
    }
    // Defensive: if a caller already stringified the extended form with forward slashes.
    if let Some(rest) = s.strip_prefix("//?/") {
        let rest = rest.replace('/', "\\");
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

/// Path string safe for `git -C` and similar CLIs (extended prefix stripped).
pub fn path_for_cli(path: &Path) -> String {
    strip_extended_path_prefix(path)
        .to_string_lossy()
        .into_owned()
}

/// Canonical git binary name (shared constant so crates don't each spell "git").
pub const GIT: &str = "git";

/// Run git under `current_dir`, return trimmed stdout.
///
/// Errors map to [`SandboxError::Spawn`] so callers can keep their own error wrapping.
pub async fn run_git(current_dir: &Path, args: &[&str]) -> Result<String, SandboxError> {
    let dir = path_for_cli(current_dir);
    let output = liberado_common::process::command(GIT)
        .args(args)
        .current_dir(&dir)
        .output()
        .await
        .map_err(|e| SandboxError::Spawn(format!("git {args:?}: {e}")))?;
    if !output.status.success() {
        return Err(SandboxError::Spawn(format!(
            "git {args:?} exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Like [`run_git`] but failures are logged and swallowed (best-effort operations).
pub async fn run_git_best_effort(current_dir: &Path, args: &[&str]) {
    let _ = run_git(current_dir, args).await;
}

#[async_trait]
impl CommandRunner for HostWorkspace {
    async fn run_command(&self, request: CommandRequest) -> Result<CommandOutput, SandboxError> {
        ensure_command_allowed(&self.command_policy, &request)?;

        let mut command = liberado_common::process::command(&request.program);
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

// ── WorktreeWorkspace — git-worktree isolation ──────────────────────────────
//
// See `docs/spec/architecture/agentic-loops.md` §Concurrency, rule 11: isolation
// must land before any concurrent workers. A worktree gives each child its own
// filesystem view so sibling edits do not collide.

/// Validate a session id for use as a worktree directory name under `worktrees_base`.
fn validate_session_worktree_id(session_id: &str) -> Result<(), SandboxError> {
    // `Drop` / remove_dir_all on worktree paths must not be steerable outside the base.
    if session_id.is_empty()
        || session_id.contains("..")
        || session_id.contains('/')
        || session_id.contains('\\')
    {
        return Err(SandboxError::MissingRoot(format!(
            "session id '{session_id}' is not a safe worktree directory name"
        )));
    }
    Ok(())
}

/// Make `path` absolute (via cwd if relative) and strip Windows extended prefixes.
/// Git worktree paths must be absolute: a relative dest is resolved from the process cwd
/// for `worktree add` but `-C dest` checkout can fail if the relative spelling is ambiguous.
fn absolute_path(path: &Path) -> PathBuf {
    let p = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    strip_extended_path_prefix(&p)
}

/// Path of the durable session worktree (`worktrees_base/session_id`) — may not exist yet.
pub fn session_worktree_path(
    worktrees_base: &Path,
    session_id: &str,
) -> Result<PathBuf, SandboxError> {
    validate_session_worktree_id(session_id)?;
    Ok(absolute_path(worktrees_base).join(session_id))
}

/// Ensure a **durable** linked git worktree at `worktrees_base/session_id`.
///
/// Unlike [`WorktreeWorkspace`], this path is **not** deleted when the agent attempt ends.
/// Park / mid-build resume / shadow-git checkpoints (S4) all need the same filesystem root to
/// survive attempt teardown. Reuses an existing worktree directory when present.
///
/// `parent_root` must be a git repository. The parent working tree is left untouched.
pub async fn ensure_session_worktree(
    parent_root: &Path,
    session_id: &str,
    worktrees_base: &Path,
) -> Result<PathBuf, SandboxError> {
    let dest = session_worktree_path(worktrees_base, session_id)?;
    let parent_root = parent_root
        .canonicalize()
        .map_err(|_| SandboxError::MissingRoot(parent_root.display().to_string()))?;
    let parent_root = strip_extended_path_prefix(&parent_root);
    let worktrees_base = absolute_path(worktrees_base);
    std::fs::create_dir_all(&worktrees_base)
        .map_err(|e| SandboxError::MissingRoot(format!("worktree base dir: {e}")))?;

    // Reuse: mid-build park/resume must land on the same files as the last attempt.
    if dest.exists() && dest.join(".git").exists() {
        return Ok(strip_extended_path_prefix(
            &dest.canonicalize().unwrap_or_else(|_| dest.clone()),
        ));
    }
    // Broken leftover (dir without git) — remove and recreate.
    if dest.exists() {
        let _ = std::fs::remove_dir_all(&dest);
    }

    match create_linked_worktree(&parent_root, &dest).await {
        Ok(()) => {}
        Err(e) => {
            // Concurrent ensure (tests / double start): path appeared between check and add.
            if !(dest.exists() && dest.join(".git").exists()) {
                return Err(e);
            }
        }
    }
    Ok(strip_extended_path_prefix(&dest.canonicalize().map_err(
        |e| SandboxError::MissingRoot(format!("worktree canonicalize: {e}")),
    )?))
}

/// Ceiling for the git plumbing that sets up a worktree.
///
/// These are local, near-instant operations — a healthy `worktree prune` is ~15ms. The bound
/// exists to convert a wedged subprocess into a reported error, not to police slow disks, so it
/// is set far above any legitimate duration.
const GIT_TIMEOUT: Duration = liberado_common::process::DEFAULT_COMMAND_TIMEOUT;

/// Create a linked worktree at `dest` from `parent_root` (must not already exist).
async fn create_linked_worktree(parent_root: &Path, dest: &Path) -> Result<(), SandboxError> {
    let parent_cli = path_for_cli(parent_root);
    let dest_cli = path_for_cli(dest);

    // Bounded, because this is the path that hung. `process::command` nulls the child's stdin
    // so it can no longer inherit the ACP bridge's JSON-RPC wire — the actual bug, which cost a
    // Paseo prompt 19 silent minutes — and `output_within` makes sure that if some *other*
    // external call ever wedges here, it surfaces as an error in 30s rather than as a spinner
    // with no end. The two are separate properties: one prevents the hang, the other keeps the
    // next unknown hang diagnosable.
    let mut prune = liberado_common::process::command("git");
    prune.args(["-C", &parent_cli]).args(["worktree", "prune"]);
    // Stale registrations are advisory; a prune that fails or times out must not block the add.
    if let Err(e) =
        liberado_common::process::output_within(&mut prune, "git worktree prune", GIT_TIMEOUT).await
    {
        tracing::warn!(%e, "git worktree prune did not complete; continuing to worktree add");
    }

    let mut add = liberado_common::process::command("git");
    add.args(["-C", &parent_cli])
        .args(["worktree", "add", "--no-checkout", &dest_cli]);
    let output = liberado_common::process::output_within(&mut add, "git worktree add", GIT_TIMEOUT)
        .await
        .map_err(|e| SandboxError::Spawn(format!("git worktree add: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SandboxError::Spawn(format!(
            "git worktree add failed: {stderr}"
        )));
    }

    let mut checkout = liberado_common::process::command("git");
    checkout
        .args(["-C", &dest_cli])
        .args(["checkout", "HEAD", "--"]);
    let output =
        liberado_common::process::output_within(&mut checkout, "git checkout", GIT_TIMEOUT)
            .await
            .map_err(|e| SandboxError::Spawn(format!("git checkout in worktree: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_dir_all(dest);
        return Err(SandboxError::Spawn(format!(
            "git checkout in worktree failed: {stderr}"
        )));
    }

    // The workspace's path dependencies are gitignored, so `git worktree add` leaves them out
    // and cargo cannot resolve the manifest — meaning a coding run in here could not compile or
    // test a single line of its own work. Provision them before handing the worktree over.
    crate::path_deps::provision_path_deps(parent_root, dest).await;

    Ok(())
}

/// A git-worktree-isolated workspace. On construction, `git worktree add --no-checkout`
/// creates a linked worktree under `<data>/worktrees/<session-id>/`, then the working tree
/// is synchronised from the parent via `git checkout`. The parent repo is unaffected by
/// any modification inside the worktree.
///
/// **Ephemeral:** the worktree is removed on [`Drop`]. Prefer [`ensure_session_worktree`] for
/// coding goal sessions that must park/resume mid-build (S4).
#[derive(Debug)]
pub struct WorktreeWorkspace {
    inner: HostWorkspace,
    /// The git worktree path — removed on `Drop`.
    worktree_path: Option<PathBuf>,
    /// The parent git repository root, used for `git worktree prune` on teardown.
    parent_repo: Option<PathBuf>,
}

impl WorktreeWorkspace {
    /// Create a linked git worktree for `session_id` rooted at `parent_root`, storing
    /// worktrees under `worktrees_base` (typically `<data>/worktrees/`).
    ///
    /// `parent_root` must be a git repository root. The parent working tree is left
    /// untouched; the worktree starts as a clean checkout of HEAD.
    ///
    /// Always creates a **fresh** worktree (removes any prior directory at the dest). For
    /// durable park/resume, use [`ensure_session_worktree`] + [`HostWorkspace`] instead.
    pub async fn new(
        parent_root: &Path,
        session_id: &str,
        worktrees_base: &Path,
        command_policy: CommandPolicy,
    ) -> Result<Self, SandboxError> {
        let parent_root = parent_root
            .canonicalize()
            .map_err(|_| SandboxError::MissingRoot(parent_root.display().to_string()))?;
        let parent_root = strip_extended_path_prefix(&parent_root);
        let worktrees_base = strip_extended_path_prefix(worktrees_base);
        validate_session_worktree_id(session_id)?;
        let worktrees_base = absolute_path(&worktrees_base);
        let dest = worktrees_base.join(session_id);
        std::fs::create_dir_all(&worktrees_base)
            .map_err(|e| SandboxError::MissingRoot(format!("worktree base dir: {e}")))?;

        // Ephemeral: wipe leftover from a prior crashed run.
        if dest.exists() {
            let _ = std::fs::remove_dir_all(&dest);
        }

        create_linked_worktree(&parent_root, &dest).await?;

        let inner = HostWorkspace::new(&dest, command_policy)?;

        Ok(Self {
            inner,
            worktree_path: Some(dest),
            parent_repo: Some(parent_root),
        })
    }

    /// Remove the worktree from disk and prune its `.git/worktrees/<name>` registration.
    /// Idempotent — safe to call multiple times.
    pub async fn cleanup(&mut self) {
        let _path = self.worktree_path.take();
        let _repo = self.parent_repo.take();
        // Prune the registration before removing the directory — git needs the
        // worktree metadata to know which registration to clean up.
        if let Some(repo) = _repo {
            let _ = liberado_common::process::command("git")
                .args(["-C", &repo.to_string_lossy()])
                .args(["worktree", "prune"])
                .output()
                .await;
        }
        if let Some(path) = _path {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

impl Drop for WorktreeWorkspace {
    fn drop(&mut self) {
        if let Some(path) = self.worktree_path.take() {
            let _ = std::fs::remove_dir_all(&path);
        }
        // Drop is synchronous so we cannot run `git worktree prune` here.
        // The stale registration is harmless: it occupies a few bytes in
        // .git/worktrees/ and will be pruned by the next worktree creation.
    }
}

impl Workspace for WorktreeWorkspace {
    fn root(&self) -> &Path {
        self.inner.root()
    }

    fn resolve_path(&self, rel_path: &str) -> Result<PathBuf, SandboxError> {
        self.inner.resolve_path(rel_path)
    }
}

#[async_trait]
impl CommandRunner for WorktreeWorkspace {
    async fn run_command(&self, request: CommandRequest) -> Result<CommandOutput, SandboxError> {
        self.inner.run_command(request).await
    }
}

#[cfg(test)]
mod tests {
    /// `Drop` does `remove_dir_all(worktree_path)`, so a traversing session id would delete outside
    /// the worktree base. Ids are internally minted ULIDs today — this is what keeps that a fact
    /// rather than an assumption.
    #[tokio::test]
    async fn a_traversing_session_id_is_refused_before_any_directory_is_made() {
        let base = std::env::temp_dir().join(format!("wt-guard-{}", std::process::id()));
        for bad in ["../escape", "a/b", "..", ""] {
            let err = WorktreeWorkspace::new(
                std::path::Path::new("."),
                bad,
                &base,
                CommandPolicy::default(),
            )
            .await;
            assert!(
                err.is_err(),
                "session id {bad:?} must be refused, not joined into a path Drop will delete"
            );
        }
        assert!(!base.exists(), "a refused id must not create the base dir");
    }

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
        assert!(
            args[4].contains(":/workspace"),
            "volume mount should reference /workspace, got: {0}",
            args[4]
        );
        assert!(
            !args[4].contains(":ro"),
            "volume mount should not be read-only, got: {0}",
            args[4]
        );
        let normalized_root = ws.root().to_string_lossy().replace('\\', "/");
        assert!(
            args[4].starts_with(&normalized_root),
            "volume mount should start with host root, got: {0}",
            args[4]
        );
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

    // ── WorktreeWorkspace tests ─────────────────────────────────────────

    #[test]
    fn strip_extended_path_prefix_removes_verbatim_drive_and_unc() {
        #[cfg(windows)]
        {
            assert_eq!(
                strip_extended_path_prefix(Path::new(r"\\?\C:\Users\me\repo")),
                PathBuf::from(r"C:\Users\me\repo")
            );
            assert_eq!(
                strip_extended_path_prefix(Path::new(r"\\?\UNC\server\share\repo")),
                PathBuf::from(r"\\server\share\repo")
            );
            assert_eq!(
                strip_extended_path_prefix(Path::new(r"//?/C:/Users/me/repo")),
                PathBuf::from(r"C:\Users\me\repo")
            );
        }
        // Already-plain paths are unchanged on every platform.
        assert_eq!(
            strip_extended_path_prefix(Path::new("/home/me/repo")),
            PathBuf::from("/home/me/repo")
        );
    }

    async fn worktree_setup() -> (tempfile::TempDir, tempfile::TempDir, WorktreeWorkspace) {
        let parent = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();

        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(parent.path())
            .status()
            .unwrap();
        assert!(status.success());

        // A commit is needed for worktree to have a HEAD to check out.
        std::fs::write(parent.path().join("README.md"), "# test repo\n").unwrap();
        let status = std::process::Command::new("git")
            .args(["-C", &parent.path().to_string_lossy()])
            .args(["add", "README.md"])
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args(["-C", &parent.path().to_string_lossy()])
            .args(["commit", "--quiet", "-m", "init"])
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test")
            .status()
            .unwrap();
        assert!(status.success());

        let ws = WorktreeWorkspace::new(
            parent.path(),
            "test-session",
            base.path(),
            CommandPolicy::default(),
        )
        .await
        .unwrap();
        (parent, base, ws)
    }

    #[tokio::test]
    async fn worktree_root_is_a_subdirectory_of_the_base() {
        let (_parent, _base, ws) = worktree_setup().await;
        assert!(ws.root().exists(), "worktree root must exist");
        assert!(
            ws.root().join("README.md").exists(),
            "worktree must have parent's committed files"
        );
        // The root is at <base>/test-session. On some platforms canonicalize
        // resolves tempdir symlinks differently, so check by relative path.
        assert_eq!(ws.root().file_name().unwrap(), "test-session");
    }

    #[tokio::test]
    async fn worktree_writes_are_isolated_from_parent() {
        let (parent, _base, ws) = worktree_setup().await;

        let parent_readme = std::fs::read_to_string(parent.path().join("README.md")).unwrap();
        assert_eq!(parent_readme, "# test repo\n");

        std::fs::write(ws.root().join("new-file.txt"), "worktree content").unwrap();
        assert!(ws.root().join("new-file.txt").exists());
        assert!(
            !parent.path().join("new-file.txt").exists(),
            "worktree write must not appear in parent"
        );

        let parent_readme2 = std::fs::read_to_string(parent.path().join("README.md")).unwrap();
        assert_eq!(parent_readme2, "# test repo\n", "parent README untouched");
    }

    #[tokio::test]
    async fn worktree_cleanup_removes_the_directory() {
        let (_parent, _base, mut ws) = worktree_setup().await;
        let root = ws.root().to_path_buf();
        assert!(root.exists());
        ws.cleanup().await;
        assert!(!root.exists());
    }

    #[tokio::test]
    async fn worktree_drop_removes_the_directory() {
        let (_parent, _base, ws) = worktree_setup().await;
        let root = ws.root().to_path_buf();
        assert!(root.exists());
        drop(ws);
        assert!(!root.exists());
    }

    #[tokio::test]
    async fn durable_session_worktree_reuses_path_and_survives_drop() {
        let parent = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(parent.path())
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::write(parent.path().join("seed.txt"), "v1\n").unwrap();
        let _ = std::process::Command::new("git")
            .args(["-C", &parent.path().to_string_lossy()])
            .args(["add", "seed.txt"])
            .status();
        let _ = std::process::Command::new("git")
            .args(["-C", &parent.path().to_string_lossy()])
            .args(["commit", "--quiet", "-m", "init"])
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test")
            .status();

        let p1 = ensure_session_worktree(parent.path(), "sess-durable", base.path())
            .await
            .unwrap();
        std::fs::write(p1.join("marker.txt"), "kept\n").unwrap();
        let p2 = ensure_session_worktree(parent.path(), "sess-durable", base.path())
            .await
            .unwrap();
        assert_eq!(p1, p2, "second ensure must reuse the same path");
        assert_eq!(
            std::fs::read_to_string(p2.join("marker.txt")).unwrap(),
            "kept\n",
            "durable worktree must not wipe in-progress edits on re-ensure"
        );
    }

    #[tokio::test]
    async fn worktree_resolve_path_is_contained() {
        let (_parent, _base, ws) = worktree_setup().await;
        let path = ws.resolve_path("src/lib.rs").unwrap();
        assert!(path.starts_with(ws.root()));
    }

    #[tokio::test]
    async fn worktree_resolve_path_rejects_escape() {
        let (_parent, _base, ws) = worktree_setup().await;
        let err = ws.resolve_path("../secret.txt").unwrap_err();
        assert!(matches!(err, SandboxError::PathEscape(_)));
    }

    /// After explicit cleanup with prune, a second worktree for the same session id
    /// can be created without git complaining about an existing registration.
    #[tokio::test]
    async fn worktree_recreation_after_cleanup_succeeds() {
        let parent = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();

        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(parent.path())
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::write(parent.path().join("f"), "v").unwrap();
        let _ = std::process::Command::new("git")
            .args(["-C", &parent.path().to_string_lossy()])
            .args(["add", "."])
            .status()
            .unwrap();
        let _ = std::process::Command::new("git")
            .args(["-C", &parent.path().to_string_lossy()])
            .args(["commit", "--quiet", "-m", "x"])
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();

        let session = "recreate-test";
        let mut ws1 = WorktreeWorkspace::new(
            parent.path(),
            session,
            base.path(),
            CommandPolicy::default(),
        )
        .await
        .unwrap();
        assert!(ws1.root().exists());
        ws1.cleanup().await;
        assert!(!ws1.root().exists());

        // Second creation must succeed — prune cleared the stale registration.
        let ws2 = WorktreeWorkspace::new(
            parent.path(),
            session,
            base.path(),
            CommandPolicy::default(),
        )
        .await
        .unwrap();
        assert!(ws2.root().exists());
        drop(ws2);
    }
}

#[cfg(test)]
mod git_helper_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        for args in [
            ["init", "--quiet"].as_slice(),
            ["config", "user.email", "test@liberado.local"].as_slice(),
            ["config", "user.name", "liberado-test"].as_slice(),
        ] {
            assert!(
                std::process::Command::new(GIT)
                    .args(args)
                    .current_dir(dir)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    #[tokio::test]
    async fn run_git_returns_stdout() {
        let dir = std::env::temp_dir().join(format!("lib-git-helper-{}", unique()));
        init_repo(&dir);
        std::fs::write(dir.join("test.txt"), "hello").unwrap();
        let status = run_git(&dir, &["status", "--porcelain"]).await.unwrap();
        assert!(
            status.contains("test.txt"),
            "expected test.txt in status: {status}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_git_errors_on_bad_command() {
        let dir = std::env::temp_dir().join(format!("lib-git-err-{}", unique()));
        init_repo(&dir);
        let err = run_git(&dir, &["this-is-not-a-git-subcommand"]).await;
        assert!(err.is_err(), "bad git command should fail");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_git_best_effort_does_not_panic_on_failure() {
        let dir = std::env::temp_dir().join(format!("lib-git-be-{}", unique()));
        init_repo(&dir);
        run_git_best_effort(&dir, &["this-is-not-a-git-subcommand"]).await;
        // Should not panic.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_session_worktree_id_rejects_bad_ids() {
        assert!(validate_session_worktree_id("").is_err());
        assert!(validate_session_worktree_id("a/b").is_err());
        assert!(validate_session_worktree_id("a\\b").is_err());
        assert!(validate_session_worktree_id("..").is_err());
        assert!(validate_session_worktree_id("a../b").is_err());
    }

    #[test]
    fn validate_session_worktree_id_accepts_good_ids() {
        assert!(validate_session_worktree_id("session-1").is_ok());
        assert!(validate_session_worktree_id("abc_def").is_ok());
        assert!(validate_session_worktree_id("task42").is_ok());
    }
}
