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
pub use path_deps::{declared_path_dep_roots, main_worktree_root, provision_path_deps};
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
    collections::{BTreeMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
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
    /// When the output exceeds `output_max_bytes`, write the full decoded body here
    /// and return a head+tail preview. The directory must be writable and reachable
    /// via `read_file` (under the workspace root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offload_dir: Option<PathBuf>,
}

impl CommandRequest {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            timeout_secs: None,
            output_max_bytes: None,
            offload_dir: None,
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
    /// Absolute path of the file holding the full decoded stdout (when it was offloaded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_offload: Option<PathBuf>,
    /// Absolute path of the file holding the full decoded stderr (when it was offloaded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_offload: Option<PathBuf>,
}

#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run_command(&self, request: CommandRequest) -> Result<CommandOutput, SandboxError>;
}

pub trait Workspace: CommandRunner {
    fn root(&self) -> &Path;
    fn resolve_path(&self, rel_path: &str) -> Result<PathBuf, SandboxError>;
    /// Extra program stems allowed despite [`CommandPolicy::deny`]. Shared with ACP permission.
    fn command_grants(&self) -> CommandGrantSet {
        CommandGrantSet::default()
    }
}

/// Program stems the operator has allowed after a permission prompt (session, workspace, or global).
#[derive(Clone, Default)]
pub struct CommandGrantSet {
    stems: Arc<Mutex<HashSet<String>>>,
}

impl CommandGrantSet {
    pub fn allow(&self, program: &str) {
        let stem = program_file_stem(program).to_ascii_lowercase();
        if stem.is_empty() {
            return;
        }
        if let Ok(mut g) = self.stems.lock() {
            g.insert(stem);
        }
    }

    pub fn revoke(&self, program: &str) {
        let stem = program_file_stem(program).to_ascii_lowercase();
        if let Ok(mut g) = self.stems.lock() {
            g.remove(&stem);
        }
    }

    pub fn contains(&self, program: &str) -> bool {
        let stem = program_file_stem(program).to_ascii_lowercase();
        self.stems
            .lock()
            .map(|g| g.contains(&stem))
            .unwrap_or(false)
    }
}

impl std::fmt::Debug for CommandGrantSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CommandGrantSet")
    }
}

#[derive(Clone, Debug)]
pub struct HostWorkspace {
    root: PathBuf,
    command_policy: CommandPolicy,
    grants: CommandGrantSet,
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
            grants: CommandGrantSet::default(),
        })
    }

    pub fn command_policy(&self) -> &CommandPolicy {
        &self.command_policy
    }

    pub fn with_command_grants(mut self, grants: CommandGrantSet) -> Self {
        self.grants = grants;
        self
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
        ensure_granted_or_allowed(
            &self.host.command_grants(),
            self.host.command_policy(),
            request,
        )?;
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

    fn command_grants(&self) -> CommandGrantSet {
        self.host.command_grants()
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
        let id = offload_id();
        let (stdout, stdout_offload) = preview_or_offload(
            stdout,
            max,
            request.offload_dir.as_deref(),
            &format!("cmd-{id}-stdout.txt"),
        );
        let (stderr, stderr_offload) = preview_or_offload(
            stderr,
            max,
            request.offload_dir.as_deref(),
            &format!("cmd-{id}-stderr.txt"),
        );

        Ok(CommandOutput {
            exit_code,
            stdout,
            stderr,
            timed_out,
            stdout_offload,
            stderr_offload,
        })
    }
}

impl Workspace for HostWorkspace {
    fn root(&self) -> &Path {
        &self.root
    }

    fn command_grants(&self) -> CommandGrantSet {
        self.grants.clone()
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

/// Backward-compatible name for the shared child-process path normalization rule.
pub use liberado_common::path::child_process_path as strip_extended_path_prefix;

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

/// Like [`run_git`] but for cleanup paths where failure is not actionable: the caller
/// discards the result, but the outcome is still returned so the call is observable.
pub async fn run_git_best_effort(
    current_dir: &Path,
    args: &[&str],
) -> Result<String, SandboxError> {
    run_git(current_dir, args).await
}

#[async_trait]
impl CommandRunner for HostWorkspace {
    async fn run_command(&self, request: CommandRequest) -> Result<CommandOutput, SandboxError> {
        ensure_granted_or_allowed(&self.grants, &self.command_policy, &request)?;

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
        let id = offload_id();
        let (stdout, stdout_offload) = preview_or_offload(
            stdout,
            max,
            request.offload_dir.as_deref(),
            &format!("cmd-{id}-stdout.txt"),
        );
        let (stderr, stderr_offload) = preview_or_offload(
            stderr,
            max,
            request.offload_dir.as_deref(),
            &format!("cmd-{id}-stderr.txt"),
        );

        Ok(CommandOutput {
            exit_code,
            stdout,
            stderr,
            timed_out,
            stdout_offload,
            stderr_offload,
        })
    }
}

fn ensure_granted_or_allowed(
    grants: &CommandGrantSet,
    policy: &CommandPolicy,
    request: &CommandRequest,
) -> Result<(), SandboxError> {
    if grants.contains(&request.program) {
        return Ok(());
    }
    ensure_command_allowed(policy, request)
}

pub fn ensure_command_allowed(
    policy: &CommandPolicy,
    request: &CommandRequest,
) -> Result<(), SandboxError> {
    let line = request.command_line();
    if policy.deny.iter().any(|rule| command_matches(rule, &line)) {
        return Err(SandboxError::CommandDenied(line));
    }
    // A single-token deny rule also matches the program's file-stem, so the default deny `git`
    // refuses `git.exe` and `C:\path\to\git.exe` on Windows, not only the bare `git` argv.
    // `program` is argv, never a shell word, so there is no quoting to worry about.
    if policy
        .deny
        .iter()
        .any(|rule| deny_matches_program_stem(rule, &request.program))
    {
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

/// Last path component of an argv program, treating both `/` and `\` as separators
/// and dropping a trailing `.exe` of any case.
///
/// [`Path::file_stem`] is host-OS-dependent: on Unix a Windows path is one
/// filename, so `C:\Program Files\Git\bin\git.exe` stems to
/// `C:\Program Files\Git\bin\git`, not `git`. The deny rule has to refuse
/// that spelling on every runner.
fn program_file_stem(program: &str) -> &str {
    let name = program
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(program);
    if name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(".exe") {
        &name[..name.len() - 4]
    } else {
        name
    }
}

/// Does a deny `rule` (a bare program name, no arguments) name this program by file-stem?
///
/// Case-insensitive everywhere: on Windows `GIT.EXE` and `git` are the same program, and being
/// permissive in the *denying* direction is always safe.
fn deny_matches_program_stem(rule: &str, program: &str) -> bool {
    let rule = rule.trim();
    if rule.is_empty() || rule.contains(' ') {
        return false;
    }
    program_file_stem(program).eq_ignore_ascii_case(rule)
}

fn decode_command_bytes(buf: &[u8]) -> String {
    if buf.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16_units(&buf[2..], u16::from_le_bytes);
    }
    if buf.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16_units(&buf[2..], u16::from_be_bytes);
    }
    if looks_like_utf16_le(buf) {
        return decode_utf16_units(buf, u16::from_le_bytes);
    }
    String::from_utf8_lossy(buf).into_owned()
}

fn looks_like_utf16_le(buf: &[u8]) -> bool {
    if buf.len() < 4 || !buf.len().is_multiple_of(2) {
        return false;
    }
    let pairs = buf.len() / 2;
    let high_nul = buf.chunks_exact(2).filter(|c| c[1] == 0).count();
    high_nul * 2 >= pairs
}

fn decode_utf16_units(buf: &[u8], from_bytes: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| from_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Unique per-command suffix for offload file names.
///
/// Nanosecond resolution makes two concurrent commands colliding effectively impossible.
fn offload_id() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_default()
}

/// First `max` bytes of `text`, never splitting a UTF-8 character.
fn truncate_head(text: &str, max: usize) -> String {
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn char_boundary_at_or_before(text: &str, mut idx: usize) -> usize {
    idx = idx.min(text.len());
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Head + tail preview totalling about `max` bytes with a marker between.
fn head_tail_preview(text: &str, max: usize) -> String {
    let head = max / 2;
    let tail = max - head;
    let head_end = char_boundary_at_or_before(text, head);
    let tail_start = char_boundary_at_or_before(text, text.len().saturating_sub(tail));
    if head_end >= tail_start {
        return truncate_head(text, max);
    }
    format!(
        "{}\n\n… [output truncated to {max} bytes of {}; middle omitted] …\n\n{}",
        &text[..head_end],
        text.len(),
        &text[tail_start..],
    )
}

/// When the decoded text fits in `max_bytes`, return it unchanged. When it exceeds
/// the threshold and `offload_dir` is set, write the full body and return a
/// head+tail preview. No directory, or a failed write, degrades to head truncation.
fn preview_or_offload(
    buf: Vec<u8>,
    max_bytes: usize,
    offload_dir: Option<&Path>,
    file_name: &str,
) -> (String, Option<PathBuf>) {
    let text = decode_command_bytes(&buf);
    if text.len() <= max_bytes {
        return (text, None);
    }
    let Some(dir) = offload_dir else {
        return (truncate_head(&text, max_bytes), None);
    };
    let path = dir.join(file_name);
    if std::fs::create_dir_all(dir).is_err() || std::fs::write(&path, &text).is_err() {
        return (truncate_head(&text, max_bytes), None);
    }
    (head_tail_preview(&text, max_bytes), Some(path))
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

    // Leftover gitignored path-deps are copied when the parent manifest still declares them.
    // The current root pin is git+tag, so this is a no-op unless a path dep remains.
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

    fn command_grants(&self) -> CommandGrantSet {
        self.inner.command_grants()
    }
}

#[async_trait]
impl CommandRunner for WorktreeWorkspace {
    async fn run_command(&self, request: CommandRequest) -> Result<CommandOutput, SandboxError> {
        self.inner.run_command(request).await
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lib_git_helper_tests.rs"]
mod git_helper_tests;

#[cfg(test)]
#[path = "lib_survivor_tests.rs"]
mod survivor_tests;
