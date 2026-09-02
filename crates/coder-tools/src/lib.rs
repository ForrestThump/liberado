//! Coding tool runtime for Liberado's Rust-native agent loop.

mod fuzzy_match;
pub mod git;
mod hashline;
mod read_search;
pub mod repo_map;
mod symbols;
mod text_view;

#[cfg(test)]
pub(crate) use read_search::{default_output_mode, glob_match, identifiers};
#[cfg(test)]
pub(crate) use symbols::{extract_impl_symbol, is_comment_line, lang_from_path, ts_class};

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use liberado_coder_core::{
    CommandPolicy, EXPLORE_TOOL_NAMES, HashlineConfig, PathPolicy, SandboxSpec,
};
use liberado_coder_sandbox::{
    CommandOutput, CommandRequest, DockerWorkspace, HostWorkspace, SandboxError, Workspace,
    WorktreeWorkspace,
};
pub use liberado_coder_sandbox::{ensure_session_worktree, session_worktree_path};
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::oneshot;

pub use hashline::{
    compute_file_hash as hashline_compute_file_hash, prompt_guidance as hashline_prompt_guidance,
};

/// Run a blocking git-module call in spawn_blocking so the async executor stays responsive.
async fn git_call<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, git::GitError> + Send + 'static,
) -> Result<T, git::GitError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| git::GitError {
            exit_code: 1,
            message: format!("git task join failed: {e}"),
        })?
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("{0}")]
    BadRequest(String),
    #[error("path denied by policy: {0}")]
    PathDenied(String),
    #[error("filesystem error: {0}")]
    Filesystem(String),
    #[error("sandbox error: {0}")]
    Sandbox(#[from] SandboxError),
}

/// Caps on concurrent `run_command_background` jobs for one coding tool runtime.
///
/// Unattended runs have launched nine concurrent `cargo` builds and filled the disk; a prompt
/// asking the model to be careful is not a resource bound. These limits refuse the start in band
/// (`ToolError::BadRequest`) so the model can read the error and call `check_background` first.
///
/// Pack-local (not kernel): a second pack would not need "how many cargo builds may run at once".
/// Defaults match the F9 acceptance bar; tests may override via [`CodingToolRuntime::with_background_limits`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundLimits {
    /// Programs treated as build-like (matched on executable stem, case-insensitive).
    pub build_like_programs: Vec<String>,
    /// Maximum total background jobs in flight (including completed-but-unpolled).
    pub max_background: usize,
    /// Maximum build-like jobs among those in flight.
    pub max_build_like: usize,
}

impl Default for BackgroundLimits {
    fn default() -> Self {
        Self {
            build_like_programs: vec![
                "cargo".into(),
                "npm".into(),
                "pnpm".into(),
                "yarn".into(),
                "make".into(),
                "go".into(),
                "gradle".into(),
                "mvn".into(),
                "tsc".into(),
            ],
            max_background: 2,
            max_build_like: 1,
        }
    }
}

#[derive(Clone)]
pub struct CodingToolRuntime {
    workspace: Arc<dyn Workspace>,
    path_policy: PathPolicy,
    validation_command: Option<CommandRequest>,
    hashline: HashlineConfig,
    edit: liberado_coder_core::EditConfig,
    background_jobs: Arc<Mutex<HashMap<String, BackgroundJob>>>,
    background_limits: BackgroundLimits,
    offered_tools: Option<Vec<String>>,
    /// Absolute directory where oversized command output is written (backlog 0.9).
    ///
    /// Under the workspace root (`<root>/.liberado/offload`) so the model can `read_file`
    /// the full body. The sandbox writes; this pack chooses the directory.
    offload_dir: PathBuf,
}

struct BackgroundJob {
    receiver: oneshot::Receiver<Result<CommandOutput, SandboxError>>,
    /// Program name as passed to `run_command_background` (for build-like detection of in-flight jobs).
    program_name: String,
}

/// Base directory for coding worktrees (`LIBERADO_DATA_DIR/coding-worktrees`, else
/// `.liberado/coding-worktrees` under the process cwd — the same root the daemon uses for
/// goal-workspaces when `LIBERADO_DATA_DIR` is unset).
pub fn coding_worktrees_base() -> PathBuf {
    let data = std::env::var("LIBERADO_DATA_DIR").unwrap_or_else(|_| ".liberado".into());
    PathBuf::from(data).join("coding-worktrees")
}

/// Durable coding-session workspace path (`coding-worktrees/<session_id>`).
/// Returns `None` when `session_id` is not a safe directory name.
pub fn durable_session_workspace(session_id: &str) -> Option<PathBuf> {
    session_worktree_path(&coding_worktrees_base(), session_id).ok()
}

/// `run_command` is argv, not a shell. Compare 3 burned a repair attempt on
/// `cargo test --workspace 2>&1` — cargo treated `2>&1` as an argument, exited
/// 101, and that looked identical to the ship-bar test failure.
fn preflight_shell_tokens(program: &str, args: &[String]) -> Option<String> {
    const TOKENS: &[&str] = &["2>&1", ">&2", "|", "||", "&&", ">", ">>", "<"];
    let hit = args.iter().find(|a| TOKENS.contains(&a.as_str()))?;
    Some(format!(
        "`run_command` is not a shell — {program:?} received {hit:?} as an argument. \
         Drop the token; stdout and stderr are already captured separately."
    ))
}

/// If this is `gh pr create … --base <name>`, ensure `origin` has that branch. Returns an error
/// message when the base is missing or cannot be checked.
fn preflight_gh_pr_create(program: &str, args: &[String]) -> Option<String> {
    let prog = Path::new(program)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(program);
    if !prog.eq_ignore_ascii_case("gh") {
        return None;
    }
    // Match `gh pr create … --base <branch>` (also `--base=<branch>`).
    let mut saw_pr = false;
    let mut saw_create = false;
    let mut base: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "pr" {
            saw_pr = true;
        } else if a == "create" {
            saw_create = true;
        } else if a == "--base" {
            base = args.get(i + 1).cloned();
            i += 1;
        } else if let Some(rest) = a.strip_prefix("--base=") {
            base = Some(rest.to_string());
        }
        i += 1;
    }
    if !(saw_pr && saw_create) {
        return None;
    }
    let base = base.filter(|b| !b.is_empty())?;
    if base.starts_with('-') {
        return Some(format!(
            "refusing gh pr create: invalid --base '{base}' (looks like a flag)"
        ));
    }
    let refspec = format!("refs/heads/{base}");
    let output = liberado_common::process::std_command("git")
        .args(["ls-remote", "--exit-code", "origin", &refspec])
        .output();
    match output {
        Ok(o) if o.status.success() => None,
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            Some(format!(
                "refusing gh pr create --base {base}: origin has no branch '{base}' \
                 (git ls-remote exit {:?}). Push the integration branch first, or pick a base \
                 that exists on origin. stderr: {stderr}",
                o.status.code()
            ))
        }
        Err(e) => Some(format!(
            "refusing gh pr create --base {base}: could not run git ls-remote: {e}"
        )),
    }
}

impl CodingToolRuntime {
    pub fn new(
        root: impl Into<PathBuf>,
        command_policy: CommandPolicy,
        path_policy: PathPolicy,
    ) -> Result<Self, ToolError> {
        let workspace = HostWorkspace::new(root, command_policy)?;
        Ok(Self::from_workspace(workspace, path_policy))
    }

    pub async fn from_sandbox(
        root: impl Into<PathBuf>,
        sandbox: SandboxSpec,
        command_policy: CommandPolicy,
        path_policy: PathPolicy,
    ) -> Result<Self, ToolError> {
        Self::from_sandbox_with_session(root, sandbox, command_policy, path_policy, None).await
    }

    /// Like `from_sandbox`, but uses `session_id` as the worktree directory name when
    /// `SandboxSpec::Worktree` is active. Prefer a unique goal/task id over the project folder
    /// name so two coding sessions on the same repo do not collide, and so self-host dogfood
    /// (workspace root = `…/life-os`) does not create `…/worktrees/life-os`.
    pub async fn from_sandbox_with_session(
        root: impl Into<PathBuf>,
        sandbox: SandboxSpec,
        command_policy: CommandPolicy,
        path_policy: PathPolicy,
        session_id: Option<&str>,
    ) -> Result<Self, ToolError> {
        match sandbox {
            SandboxSpec::HostLocal => {
                // A git worktree (compare trees, Paseo checkouts) has no gitignored
                // path-deps. Copy them from the main checkout so `cargo check` can resolve.
                let root = root.into();
                let _ = liberado_coder_sandbox::provision_path_deps(&root, &root).await;
                Self::new(root, command_policy, path_policy)
            }
            SandboxSpec::Docker(spec) => {
                let workspace = DockerWorkspace::new(root, spec, command_policy)?;
                Ok(Self::from_workspace(workspace, path_policy))
            }
            SandboxSpec::Worktree => {
                let root = root.into();
                let fallback = root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("session");
                // Sanitize: worktree dir name must not contain path separators.
                let session_id = session_id
                    .filter(|s| {
                        !s.is_empty() && !s.contains("..") && !s.contains('/') && !s.contains('\\')
                    })
                    .unwrap_or(fallback);
                // Prefer LIBERADO_DATA_DIR/coding-worktrees (or .liberado/coding-worktrees) so
                // self-host does not create sibling dirs next to the project checkout (dogfood #1).
                let worktrees_base = coding_worktrees_base();
                let workspace =
                    WorktreeWorkspace::new(&root, session_id, &worktrees_base, command_policy)
                        .await?;
                Ok(Self::from_workspace(workspace, path_policy))
            }
        }
    }

    pub fn command_grants(&self) -> liberado_coder_sandbox::CommandGrantSet {
        self.workspace.command_grants()
    }

    pub fn from_workspace(workspace: impl Workspace + 'static, path_policy: PathPolicy) -> Self {
        let offload_dir = workspace.root().join(".liberado").join("offload");
        Self {
            workspace: Arc::new(workspace),
            path_policy,
            validation_command: None,
            hashline: HashlineConfig::default(),
            edit: liberado_coder_core::EditConfig::default(),
            background_jobs: Arc::new(Mutex::new(HashMap::new())),
            background_limits: BackgroundLimits::default(),
            offered_tools: None,
            offload_dir,
        }
    }

    /// Override background concurrency caps (defaults match F9: two total, one build-like).
    pub fn with_background_limits(mut self, limits: BackgroundLimits) -> Self {
        self.background_limits = limits;
        self
    }

    pub fn with_validation_command(mut self, command: CommandRequest) -> Self {
        self.validation_command = Some(command);
        self
    }

    /// Enable or configure hashline (line-anchored) edit mode.
    pub fn with_hashline(mut self, config: HashlineConfig) -> Self {
        self.hashline = config;
        self
    }

    /// Anchor-matching behaviour for `edit_file`.
    pub fn with_edit(mut self, config: liberado_coder_core::EditConfig) -> Self {
        self.edit = config;
        self
    }

    /// Restrict the model-offered coding catalog to these names. `None` keeps the full set.
    /// Executor-injected finish tools are not this list.
    pub fn with_offered_tools(mut self, names: Option<Vec<String>>) -> Self {
        self.offered_tools = names.filter(|n| !n.is_empty());
        self
    }

    pub fn hashline(&self) -> &HashlineConfig {
        &self.hashline
    }

    pub fn workspace_root(&self) -> &Path {
        self.workspace.root()
    }

    /// Stamp the runtime's offload directory onto a command request.
    ///
    /// Model-visible commands go through this. Gate/backend requests that do not
    /// keep `offload_dir: None` and keep head truncation.
    fn with_offload(&self, mut request: CommandRequest) -> CommandRequest {
        request.offload_dir = Some(self.offload_dir.clone());
        request
    }

    /// Turn an absolute offload path into the workspace-relative spelling the model reads.
    fn offload_rel(&self, path: &Option<PathBuf>) -> Option<String> {
        let abs = path.as_ref()?;
        let rel = abs.strip_prefix(self.workspace.root()).ok()?;
        Some(rel.to_string_lossy().replace('\\', "/"))
    }

    /// When a command result was offloaded, append the reachable path and a dedicated key.
    fn apply_offload(&self, output: &CommandOutput, value: &mut Value) {
        if let Some(rel) = self.offload_rel(&output.stdout_offload) {
            value["stdout"] = json!(format!(
                "{}\n\n[full stdout saved to {rel} — read it with read_file]",
                output.stdout
            ));
            value["full_stdout_path"] = json!(rel);
        }
        if let Some(rel) = self.offload_rel(&output.stderr_offload) {
            value["stderr"] = json!(format!(
                "{}\n\n[full stderr saved to {rel} — read it with read_file]",
                output.stderr
            ));
            value["full_stderr_path"] = json!(rel);
        }
    }

    pub async fn invoke_json_for_backend(
        &self,
        name: &str,
        args: Value,
    ) -> Result<Value, ToolError> {
        self.invoke_json(name, args).await
    }

    fn rel_path(&self, rel_path: &str, write: bool) -> Result<PathBuf, ToolError> {
        if path_denied(rel_path, &self.path_policy) {
            return Err(ToolError::PathDenied(rel_path.to_string()));
        }
        if write && !path_allowed_to_write(rel_path, &self.path_policy) {
            return Err(ToolError::PathDenied(rel_path.to_string()));
        }
        if write && !self.path_policy.write_scope.permits(rel_path) {
            return Err(ToolError::PathDenied(rel_path.to_string()));
        }
        Ok(self.workspace.resolve_path(rel_path)?)
    }

    async fn invoke_json(&self, name: &str, args: Value) -> Result<Value, ToolError> {
        if let Some(git_tool) = name.strip_prefix("git_") {
            return self.invoke_git_json(git_tool, args).await;
        }
        match name {
            "list_files" => self.list_files(args).await,
            // `search_text` is kept as an alias: a run mid-flight against an older
            // catalog should not fail on a rename.
            "grep" | "search_text" => self.grep(args).await,
            "list_symbols" => self.list_symbols(args).await,
            "read_file" => self.read_file(args).await,
            "write_file" => self.write_file(args).await,
            "edit_file" => self.edit_file(args).await,
            "apply_patch" => self.apply_patch(args).await,
            "hashline_edit" => self.hashline_edit(args).await,
            "run_command_background" => self.run_command_background(args).await,
            "check_background" => self.check_background(args).await,
            "run_command" => self.run_command(args).await,
            "validate" => self.validate().await,
            other => Err(ToolError::BadRequest(format!("unknown tool: {other}"))),
        }
    }

    /// The git-family tools, named after the `git_` prefix.
    async fn invoke_git_json(&self, git_tool: &str, args: Value) -> Result<Value, ToolError> {
        match git_tool {
            "status" => self.git_status().await,
            "diff" => self.git_diff(args).await,
            "branch" => self.git_branch(args).await,
            "commit" => self.git_commit(args).await,
            "push" => self.git_push(args).await,
            "log" => self.git_log(args).await,
            "fetch" => self.git_fetch(args).await,
            "merge" => self.git_merge(args).await,
            other => Err(ToolError::BadRequest(format!("unknown tool: git_{other}"))),
        }
    }

    async fn hashline_edit(&self, args: Value) -> Result<Value, ToolError> {
        if !self.hashline.enabled {
            return Err(ToolError::BadRequest(
                "hashline_edit is disabled; set [coder.hashline] enabled = true in tuning.toml"
                    .into(),
            ));
        }
        #[derive(Deserialize)]
        struct Args {
            /// Full hashline patch: one or more `[path#TAG]` sections with PUT/CUT/REM ops.
            input: String,
        }
        let args: Args = parse_args(args)?;
        if args.input.trim().is_empty() {
            return Err(ToolError::BadRequest("input must not be empty".to_string()));
        }

        let sections = hashline::parse_patch(&args.input).map_err(ToolError::BadRequest)?;
        let hash_length = self.hashline.hash_length;

        // Resolve policy + read all targets first (all-or-nothing preflight helpers).
        let mut resolved: Vec<(String, PathBuf, bool)> = Vec::with_capacity(sections.len());
        for section in &sections {
            let write = true;
            let path = self.rel_path(&section.path, write)?;
            resolved.push((section.path.clone(), path, write));
        }

        let reports = {
            let resolved = &resolved;
            hashline::apply_patch_sections(
                &sections,
                hash_length,
                true,
                |rel| {
                    let path = resolved
                        .iter()
                        .find(|(p, _, _)| p == rel)
                        .map(|(_, path, _)| path.clone())
                        .ok_or_else(|| format!("unknown path {rel}"))?;
                    std::fs::read_to_string(&path).map_err(|e| format!("read {rel}: {e}"))
                },
                |rel, content| {
                    let path = resolved
                        .iter()
                        .find(|(p, _, _)| p == rel)
                        .map(|(_, path, _)| path.clone())
                        .ok_or_else(|| format!("unknown path {rel}"))?;
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {rel}: {e}"))?;
                    }
                    std::fs::write(&path, content.as_bytes())
                        .map_err(|e| format!("write {rel}: {e}"))
                },
                |rel| {
                    let path = resolved
                        .iter()
                        .find(|(p, _, _)| p == rel)
                        .map(|(_, path, _)| path.clone())
                        .ok_or_else(|| format!("unknown path {rel}"))?;
                    std::fs::remove_file(&path).map_err(|e| format!("remove {rel}: {e}"))
                },
            )
            .map_err(ToolError::BadRequest)?
        };

        let files: Vec<Value> = reports
            .iter()
            .map(|r| {
                json!({
                    "path": r.path,
                    "op": r.op,
                    "tag": r.file_hash,
                    "first_changed_line": r.first_changed_line,
                })
            })
            .collect();
        Ok(json!({
            "files": files,
            "sections": files.len(),
            "hashline": true,
        }))
    }

    /// Write a whole file. Refuses to silently replace one that already has content.
    ///
    /// `std::fs::write` truncates, and nothing here checked whether the target existed. A coding
    /// run used `write_file` on `crates/executor/src/lib.rs` believing it was adding a struct and
    /// replaced 3,921 lines with 40 — `5,825` deletions against `54` insertions across three
    /// files, in one turn, with no error and no warning.
    ///
    /// The coder prompt already says "prefer edit_file/apply_patch for existing files". That is a
    /// preference, and a preference is not a guard; this is the same lesson as every other config
    /// that parsed and reached nobody. `overwrite` makes a deliberate rewrite say so.
    ///
    /// **The verifier is not a substitute for this.** `cargo check` did catch that run, because
    /// what was destroyed happened to be Rust that other crates compile against. Deleting a
    /// fixture, a doc, a prompt file or a `.toml` passes every verifier we have.
    async fn write_file(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            path: String,
            content: String,
            /// Required to replace a file that already has content.
            #[serde(default)]
            overwrite: bool,
            /// Add to the end of the file instead of replacing it.
            #[serde(default)]
            append: bool,
        }
        let args: Args = parse_args(args)?;
        let path = self.rel_path(&args.path, true)?;

        if args.append {
            // The move both failed runs were actually trying to make. "Add a struct to this file"
            // is an append, and with no way to express it the model reached for a whole-file
            // write and destroyed 3,921 lines. An append cannot delete anything.
            if args.overwrite {
                return Err(ToolError::BadRequest(
                    "append and overwrite are opposites; pass only one.".to_string(),
                ));
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(fs_err)?;
            }
            // The file's own shape is preserved: appending LF text to a CRLF file would leave it
            // mixed, and a mixed file is one the edit tools then refuse to normalize.
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let view = text_view::to_model_view(&existing);
            let addition = text_view::normalize_anchor(&args.content, view.ending);
            let combined = format!("{}{addition}", view.text);
            let restored = text_view::materialize(&combined, view.ending, view.bom);
            std::fs::write(&path, restored.as_bytes()).map_err(fs_err)?;
            return Ok(json!({ "path": args.path, "written": true, "appended": true }));
        }

        // An overwrite that throws away most of a substantial file is a mistake every time we
        // have seen it, and the boolean alone did not stop it: the model flipped the flag the
        // moment the refusal named it. So the shape of the change is checked, not just the
        // caller's intent.
        //
        // The numbers are arbitrary and that is the honest objection to them. The alternative was
        // measured: a flag the model can set on retry is not a guard. 50 lines keeps small files
        // freely rewritable; a fifth of the original allows a genuine condensing rewrite while
        // rejecting a 1,726-line file becoming 67.
        if args.overwrite
            && !args.append
            && let Ok(existing) = std::fs::read_to_string(&path)
        {
            let before = existing.lines().count();
            let after = args.content.lines().count();
            if before > 50 && after * 5 < before {
                return Err(ToolError::BadRequest(format!(
                    "overwrite would cut {} from {before} lines to {after}. That is a truncation, not a rewrite. Use edit_file, hashline_edit or apply_patch to change part of the file, or \"append\": true to add to it. If you really do mean to discard the file, delete it first with run_command.",
                    args.path
                )));
            }
        }

        if !args.overwrite {
            // Byte length, not a line count or a percentage: any threshold is a number someone
            // has to defend later, and "the file already has something in it" needs no defending.
            // An empty existing file is not content, so re-writing one is not a clobber.
            if let Ok(meta) = std::fs::metadata(&path)
                && meta.is_file()
                && meta.len() > 0
            {
                let existing_lines = std::fs::read_to_string(&path)
                    .map(|c| c.lines().count())
                    .unwrap_or(0);
                // Deliberately does NOT mention `overwrite`. The first version of this message
                // listed it as the third option, and a dispatched run read the refusal and
                // immediately re-sent the same call with `"overwrite": true`, deleting 1,659
                // lines. An error that ends by naming the flag that bypasses it is not a guard,
                // it is a hint.
                return Err(ToolError::BadRequest(format!(
                    "{} already exists with {existing_lines} lines, and write_file replaces the whole file. To add to the end pass \"append\": true. To change part of it use edit_file, hashline_edit or apply_patch.",
                    args.path
                )));
            }
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(fs_err)?;
        }
        std::fs::write(&path, args.content.as_bytes()).map_err(fs_err)?;
        Ok(json!({ "path": args.path, "written": true }))
    }

    /// Replace an exact span of text in one file.
    ///
    /// Matching happens in the model view ([`text_view`]), not against raw bytes, so an LF
    /// anchor matches a CRLF file and a BOM does not shift line one. Both sides are normalized;
    /// the file's own shape is restored on write.
    ///
    /// Every error names the next action. "old text was not found" is correct and useless: the
    /// model's remaining move is to re-read, and a run that cannot infer that spends its turns
    /// guessing variants of the same anchor. That is measured, not supposed — a dispatched run
    /// burned 15 of 25 edit calls on it.
    async fn edit_file(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            path: String,
            old: String,
            new: String,
            /// Replace every occurrence instead of failing when the anchor is ambiguous.
            #[serde(default)]
            replace_all: bool,
        }
        let args: Args = parse_args(args)?;
        if args.old.is_empty() {
            return Err(ToolError::BadRequest(
                "old must not be empty. To create a file or replace one whole, use write_file."
                    .to_string(),
            ));
        }
        if args.old == args.new {
            // Silently succeeding here reports an edit that changed nothing, which is how a run
            // convinces itself it has made progress.
            return Err(ToolError::BadRequest(
                "old and new are identical, so this edit would change nothing.".to_string(),
            ));
        }
        let path = self.rel_path(&args.path, true)?;
        let raw = std::fs::read_to_string(&path).map_err(fs_err)?;
        let view = text_view::to_model_view(&raw);
        let old = text_view::normalize_anchor(&args.old, view.ending);
        let new = text_view::normalize_anchor(&args.new, view.ending);

        let outcome = fuzzy_match::find_match(
            &view.text,
            &old,
            self.edit.fuzzy_match && !args.replace_all,
            self.edit.fuzzy_threshold,
        );

        let (updated, count) = match outcome {
            fuzzy_match::MatchOutcome::Exact { count, .. } => {
                if count > 1 && !args.replace_all {
                    return Err(ToolError::BadRequest(format!(
                        "old text matched {count} times in {}. Include more surrounding lines to make it unique, or pass \"replace_all\": true to change every occurrence.",
                        args.path
                    )));
                }
                let updated = if args.replace_all {
                    view.text.replace(&old, &new)
                } else {
                    view.text.replacen(&old, &new, 1)
                };
                (updated, count)
            }
            fuzzy_match::MatchOutcome::Fuzzy(m) => {
                // Re-indent the replacement to where the match was actually found. Without this,
                // fixing the anchor only moves the error: the edit lands and the inserted block
                // sits at the indentation the model imagined rather than the file's.
                let adjusted = fuzzy_match::adjust_indentation(&old, &m.actual_text, &new);
                tracing::info!(
                    path = %args.path,
                    line = m.start_line,
                    confidence = m.confidence,
                    "edit_file matched an approximate anchor"
                );
                (view.text.replacen(&m.actual_text, &adjusted, 1), 1)
            }
            fuzzy_match::MatchOutcome::Ambiguous { count, best } => {
                return Err(ToolError::BadRequest(format!(
                    "old text is not exact, and {count} places in {} are close enough that choosing one would be a guess (nearest is line {}). Include more surrounding lines so the anchor is unique.",
                    args.path, best.start_line
                )));
            }
            fuzzy_match::MatchOutcome::NotFound { closest } => {
                // Quote the near miss. "Not found" alone leaves the model to guess again, which
                // is how one run spent 21 of 50 edits on variants of the same wrong anchor.
                let near = match closest {
                    Some(c) if c.confidence > 0.6 => format!(
                        " The closest text is at line {} and is {:.0}% similar:\n{}",
                        c.start_line,
                        c.confidence * 100.0,
                        c.actual_text.lines().take(4).collect::<Vec<_>>().join("\n")
                    ),
                    _ => String::new(),
                };
                return Err(ToolError::BadRequest(format!(
                    "old text was not found in {}. It must match the file, including whitespace and indentation. The file may have changed since you last read it — read it again and take the anchor from that output.{near}",
                    args.path
                )));
            }
        };
        let restored = text_view::materialize(&updated, view.ending, view.bom);
        std::fs::write(&path, restored.as_bytes()).map_err(fs_err)?;
        Ok(json!({ "path": args.path, "replacements": count }))
    }

    async fn apply_patch(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            edits: Vec<PatchEdit>,
        }
        #[derive(Deserialize)]
        struct PatchEdit {
            path: String,
            old: String,
            new: String,
        }

        let args: Args = parse_args(args)?;
        if args.edits.is_empty() {
            return Err(ToolError::BadRequest(
                "edits must contain at least one edit".to_string(),
            ));
        }

        let mut prepared = Vec::with_capacity(args.edits.len());
        for edit in args.edits {
            if edit.old.is_empty() {
                return Err(ToolError::BadRequest(format!(
                    "old must not be empty for {}",
                    edit.path
                )));
            }
            let path = self.rel_path(&edit.path, true)?;
            let content = std::fs::read_to_string(&path).map_err(fs_err)?;
            let count = content.matches(&edit.old).count();
            if count == 0 {
                return Err(ToolError::BadRequest(format!(
                    "old text was not found in {}",
                    edit.path
                )));
            }
            if count > 1 {
                return Err(ToolError::BadRequest(format!(
                    "old text matched {count} times in {}; provide more context",
                    edit.path
                )));
            }
            let updated = content.replacen(&edit.old, &edit.new, 1);
            prepared.push((edit.path, path, updated));
        }

        let changed = prepared
            .iter()
            .map(|(rel, _, _)| rel.clone())
            .collect::<Vec<_>>();
        let edit_count = changed.len();
        for (_, path, updated) in prepared {
            std::fs::write(&path, updated.as_bytes()).map_err(fs_err)?;
        }
        Ok(json!({ "files": changed, "edits": edit_count }))
    }

    async fn git_status(&self) -> Result<Value, ToolError> {
        let root = self.workspace.root().to_path_buf();
        match git_call(move || git::status(&root)).await {
            Ok(stdout) => Ok(json!({
                "exit_code": 0,
                "stdout": stdout,
                "stderr": "",
                "timed_out": false,
            })),
            Err(e) => Ok(json!({
                "exit_code": e.exit_code,
                "stdout": "",
                "stderr": e.message,
                "timed_out": false,
            })),
        }
    }

    async fn git_diff(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            #[serde(default = "default_diff_mode")]
            mode: String,
        }
        let args: Args = parse_args(args)?;
        if !matches!(args.mode.as_str(), "name_only" | "stat" | "patch") {
            return Err(ToolError::BadRequest(format!(
                "unsupported diff mode: {}",
                args.mode
            )));
        }
        let mode = args.mode.clone();
        let root = self.workspace.root().to_path_buf();
        let output = git_call(move || match mode.as_str() {
            "name_only" => git::diff_name_only(&root),
            "stat" => git::diff_stat(&root),
            _ => git::diff_patch(&root),
        })
        .await;
        let (exit_code, diff_stdout, stderr) = match output {
            Ok(stdout) => (0, stdout, String::new()),
            Err(e) => (e.exit_code, String::new(), e.message),
        };
        let untracked = self.untracked_section(&args.mode).await?;
        let stdout = match untracked {
            Some(extra) if diff_stdout.trim().is_empty() => extra,
            Some(extra) => format!("{}\n{extra}", diff_stdout.trim_end()),
            None => diff_stdout,
        };
        Ok(json!({
            "mode": args.mode,
            "exit_code": exit_code,
            "stdout": stdout,
            "stderr": stderr,
            "timed_out": false,
        }))
    }

    /// The untracked half of "what have I changed".
    ///
    /// `git diff` reports tracked files only, so a file the model has just *created* is absent
    /// from every diff mode. That is not a cosmetic gap. In one run the model wrote a 334-line
    /// module, saw four consecutive empty diffs, concluded "the file doesn't exist yet — that's
    /// the root cause of the build failure", and wrote the whole module a second time. It spent
    /// an attempt chasing a file that was on disk the entire time.
    ///
    /// Writing a file is the most common first act of a coding task, so the tool that answers
    /// "what have I changed" has to count it. `--exclude-standard` keeps `.gitignore` honoured,
    /// so `target/` does not drown the answer.
    async fn untracked_section(&self, mode: &str) -> Result<Option<String>, ToolError> {
        let root = self.workspace.root().to_path_buf();
        let listed = git_call(move || git::untracked_files(&root)).await;
        let listed = match listed {
            Ok(paths) => paths,
            Err(_) => return Ok(None), // not a repo: no untracked section
        };
        if listed.is_empty() {
            return Ok(None);
        }

        let mut out = String::from("# untracked (new files, not yet added)\n");
        match mode {
            "name_only" => {
                for path in &listed {
                    out.push_str(path);
                    out.push('\n');
                }
            }
            "stat" => {
                for path in &listed {
                    match self.untracked_body(path) {
                        Some(body) => {
                            let n = body.lines().count();
                            out.push_str(&format!(" {path} | {n} +++\n"));
                        }
                        None => out.push_str(&format!(" {path} | (unreadable)\n")),
                    }
                }
            }
            // patch: the content, because a new file's content *is* its diff. Bounded, so one
            // generated file cannot crowd out the rest of the answer.
            _ => {
                let mut budget = UNTRACKED_PATCH_BUDGET;
                for path in &listed {
                    let Some(body) = self.untracked_body(path) else {
                        out.push_str(&format!("--- new file {path} (unreadable)\n"));
                        continue;
                    };
                    out.push_str(&format!("--- new file {path}\n"));
                    if budget == 0 {
                        out.push_str(
                            "(omitted: earlier files used the patch budget; read it \
                                      directly)\n",
                        );
                        continue;
                    }
                    let shown = truncate_on_char(&body, budget);
                    budget = budget.saturating_sub(shown.len());
                    for line in shown.lines() {
                        out.push('+');
                        out.push_str(line);
                        out.push('\n');
                    }
                    if shown.len() < body.len() {
                        out.push_str(&format!("… truncated; read {path} for the rest\n"));
                    }
                }
            }
        }
        Ok(Some(out))
    }

    /// Read an untracked file for the diff, or `None` if it cannot be shown.
    ///
    /// Goes through the same `rel_path` guard as every other read: `git ls-files` reports what is
    /// in the working tree, and a path policy that hides a directory from `read_file` must not be
    /// circumvented by asking for a diff instead.
    fn untracked_body(&self, rel: &str) -> Option<String> {
        let path = self.rel_path(rel, false).ok()?;
        let bytes = std::fs::read(&path).ok()?;
        // Binary files have no useful "+" rendering, and a stray NUL would corrupt the transcript.
        if bytes.contains(&0) {
            return None;
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn git_branch(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            name: String,
        }
        let args: Args = parse_args(args)?;
        if args.name.is_empty() {
            return Err(ToolError::BadRequest(
                "branch name must not be empty".to_string(),
            ));
        }
        if args.name.starts_with('-') {
            return Err(ToolError::BadRequest(
                "branch name must not start with '-'".to_string(),
            ));
        }
        let branch_name = args.name.clone();
        let root = self.workspace.root().to_path_buf();
        let name_for_closure = branch_name.clone();
        match git_call(move || git::branch_create(&root, &name_for_closure)).await {
            Ok(()) => Ok(json!({
                "branch": branch_name,
                "exit_code": 0,
                "stdout": "",
                "stderr": "",
                "timed_out": false,
            })),
            Err(e) => Ok(json!({
                "branch": branch_name,
                "exit_code": e.exit_code,
                "stdout": "",
                "stderr": e.message,
                "timed_out": false,
            })),
        }
    }

    async fn git_commit(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            message: String,
            #[serde(default)]
            files: Vec<String>,
        }
        let args: Args = parse_args(args)?;
        if args.message.is_empty() {
            return Err(ToolError::BadRequest(
                "commit message must not be empty".to_string(),
            ));
        }

        let files = args.files.clone();
        let message = args.message.clone();
        let root = self.workspace.root().to_path_buf();
        match git_call(move || git::commit(&root, &message, Some(&files))).await {
            Ok(stdout) => Ok(json!({
                "committed": true,
                "exit_code": 0,
                "stdout": stdout,
                "stderr": "",
                "timed_out": false,
            })),
            Err(e) => Ok(json!({
                "committed": false,
                "stage_exit_code": null,
                "stage_stderr": "",
                "exit_code": e.exit_code,
                "stdout": "",
                "stderr": e.message,
                "timed_out": false,
            })),
        }
    }

    async fn git_push(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            #[serde(default = "default_remote")]
            remote: String,
            #[serde(default)]
            branch: Option<String>,
            #[serde(default)]
            set_upstream: bool,
        }
        let args: Args = parse_args(args)?;
        if args.remote.is_empty() {
            return Err(ToolError::BadRequest(
                "remote must not be empty".to_string(),
            ));
        }
        if args.remote.starts_with('-') {
            return Err(ToolError::BadRequest(
                "remote must not start with '-'".to_string(),
            ));
        }
        if let Some(ref branch) = args.branch {
            if branch.is_empty() {
                return Err(ToolError::BadRequest(
                    "branch must not be empty".to_string(),
                ));
            }
            if branch.starts_with('-') {
                return Err(ToolError::BadRequest(
                    "branch must not start with '-'".to_string(),
                ));
            }
        }
        let remote = args.remote.clone();
        let branch = args.branch.clone();
        let set_upstream = args.set_upstream;
        let root = self.workspace.root().to_path_buf();
        match git_call(move || git::push(&root, &remote, branch.as_deref(), set_upstream)).await {
            Ok(stdout) => Ok(json!({
                "exit_code": 0,
                "stdout": stdout,
                "stderr": "",
                "timed_out": false,
            })),
            Err(e) => Ok(json!({
                "exit_code": e.exit_code,
                "stdout": "",
                "stderr": e.message,
                "timed_out": false,
            })),
        }
    }

    async fn git_log(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            #[serde(default = "default_log_limit")]
            limit: u32,
            #[serde(default)]
            branch: Option<String>,
            #[serde(default)]
            format: Option<String>,
        }
        fn default_log_limit() -> u32 {
            20
        }
        let args: Args = parse_args(args)?;
        let limit = args.limit.min(100);
        let fmt = args.format.unwrap_or_else(|| "%h %s".to_string());
        let branch = args.branch.filter(|b| !b.is_empty());
        let root = self.workspace.root().to_path_buf();
        match git_call(move || git::log(&root, limit, Some(&fmt), branch.as_deref())).await {
            Ok(stdout) => Ok(json!({
                "exit_code": 0,
                "stdout": stdout,
                "stderr": "",
                "timed_out": false,
            })),
            Err(e) => Ok(json!({
                "exit_code": e.exit_code,
                "stdout": "",
                "stderr": e.message,
                "timed_out": false,
            })),
        }
    }

    async fn git_fetch(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            #[serde(default = "default_remote")]
            remote: String,
            #[serde(default)]
            branch: Option<String>,
        }
        let args: Args = parse_args(args)?;
        if args.remote.starts_with('-') {
            return Err(ToolError::BadRequest(
                "remote must not start with '-'".to_string(),
            ));
        }
        if let Some(ref branch) = args.branch {
            if branch.is_empty() {
                return Err(ToolError::BadRequest(
                    "branch must not be empty".to_string(),
                ));
            }
            if branch.starts_with('-') {
                return Err(ToolError::BadRequest(
                    "branch must not start with '-'".to_string(),
                ));
            }
        }
        let remote = args.remote.clone();
        let branch = args.branch.clone();
        let root = self.workspace.root().to_path_buf();
        match git_call(move || git::fetch(&root, &remote, branch.as_deref())).await {
            Ok(stdout) => Ok(json!({
                "exit_code": 0,
                "stdout": stdout,
                "stderr": "",
                "timed_out": false,
            })),
            Err(e) => Ok(json!({
                "exit_code": e.exit_code,
                "stdout": "",
                "stderr": e.message,
                "timed_out": false,
            })),
        }
    }

    async fn git_merge(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            branch: String,
            #[serde(default)]
            fast_forward_only: bool,
        }
        let args: Args = parse_args(args)?;
        if args.branch.is_empty() {
            return Err(ToolError::BadRequest(
                "branch must not be empty".to_string(),
            ));
        }
        if args.branch.starts_with('-') {
            return Err(ToolError::BadRequest(
                "branch must not start with '-'".to_string(),
            ));
        }
        let branch = args.branch.clone();
        let ff_only = args.fast_forward_only;
        let root = self.workspace.root().to_path_buf();
        match git_call(move || git::merge(&root, &branch, ff_only)).await {
            Ok(stdout) => Ok(json!({
                "exit_code": 0,
                "stdout": stdout,
                "stderr": "",
                "timed_out": false,
            })),
            Err(e) => Ok(json!({
                "exit_code": e.exit_code,
                "stdout": "",
                "stderr": e.message,
                "timed_out": false,
            })),
        }
    }

    async fn run_command_background(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            program: String,
            #[serde(default)]
            args: Vec<String>,
        }
        let args: Args = parse_args(args)?;
        if let Some(err) = preflight_shell_tokens(&args.program, &args.args) {
            return Err(ToolError::BadRequest(err));
        }
        let limits = &self.background_limits;
        let build_like = is_build_like(&args.program, &limits.build_like_programs);

        // Lock early and check limits *before* spawning. The lock is held only long enough to
        // count and maybe insert — command execution holds no lock.
        //
        // A completed-but-unpolled job still occupies a slot: the model must call
        // `check_background` to reap it. That is intentional — a finished cargo build still
        // consumed disk, and silently stacking completed builds re-opens the exhaustion path.
        let mut jobs = self
            .background_jobs
            .lock()
            .map_err(|e| ToolError::BadRequest(format!("background job lock: {e}")))?;

        let total_count = jobs.len();
        let build_like_count = jobs
            .values()
            .filter(|j| is_build_like(&j.program_name, &limits.build_like_programs))
            .count();

        if build_like && build_like_count >= limits.max_build_like {
            let existing = jobs
                .iter()
                .find(|(_, j)| is_build_like(&j.program_name, &limits.build_like_programs))
                .map(|(id, _)| id.as_str())
                .unwrap_or("(unknown)");
            return Err(ToolError::BadRequest(format!(
                "at most {} build-like background job(s) may run at once (got {build_like_count}); \
                 job {existing} is still running — call check_background on it first",
                limits.max_build_like,
            )));
        }

        if total_count >= limits.max_background {
            let existing = jobs
                .keys()
                .next()
                .map(|s| s.as_str())
                .unwrap_or("(unknown)");
            return Err(ToolError::BadRequest(format!(
                "at most {} background job(s) may run at once (got {total_count}); \
                 job {existing} is still running — call check_background on it first",
                limits.max_background,
            )));
        }

        let mut request = CommandRequest::new(args.program.clone());
        request.args = args.args;
        let request = self.with_offload(request);
        let program_name = args.program.clone();

        let (tx, rx) = oneshot::channel();
        let workspace = Arc::clone(&self.workspace);

        tokio::spawn(async move {
            let output = workspace.run_command(request).await;
            let _ = tx.send(output);
        });

        let job_id = format!(
            "job-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );

        jobs.insert(
            job_id.clone(),
            BackgroundJob {
                receiver: rx,
                program_name,
            },
        );

        Ok(json!({
            "job_id": job_id,
            "status": "running",
            "program": args.program,
        }))
    }

    async fn check_background(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            job_id: String,
        }
        let args: Args = parse_args(args)?;

        let mut jobs = self
            .background_jobs
            .lock()
            .map_err(|e| ToolError::BadRequest(format!("background job lock: {e}")))?;

        let Some(mut job) = jobs.remove(&args.job_id) else {
            return Ok(json!({
                "job_id": args.job_id,
                "status": "unknown",
                "error": "no such background job",
            }));
        };

        let jid = args.job_id.clone();
        match job.receiver.try_recv() {
            Ok(Ok(output)) => {
                let mut value = json!({
                    "job_id": jid,
                    "status": "completed",
                    "exit_code": output.exit_code,
                    "stdout": output.stdout.clone(),
                    "stderr": output.stderr.clone(),
                    "timed_out": output.timed_out,
                });
                self.apply_offload(&output, &mut value);
                Ok(value)
            }
            Ok(Err(e)) => Ok(json!({
                "job_id": jid,
                "status": "failed",
                "error": e.to_string(),
            })),
            Err(oneshot::error::TryRecvError::Empty) => {
                jobs.insert(jid.clone(), job);
                Ok(json!({
                    "job_id": jid,
                    "status": "running",
                }))
            }
            Err(oneshot::error::TryRecvError::Closed) => Ok(json!({
                "job_id": jid,
                "status": "failed",
                "error": "background task panicked or was dropped",
            })),
        }
    }

    async fn run_command(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            program: String,
            #[serde(default)]
            args: Vec<String>,
        }
        let args: Args = parse_args(args)?;
        if let Some(err) = preflight_shell_tokens(&args.program, &args.args) {
            return Err(ToolError::BadRequest(err));
        }
        // Dogfood finding #5: refuse `gh pr create --base X` when origin has no X, so we never
        // silently land a PR on main with a multi-PR stack.
        if let Some(err) = preflight_gh_pr_create(&args.program, &args.args) {
            return Err(ToolError::BadRequest(err));
        }
        let mut request = CommandRequest::new(args.program);
        request.args = args.args;
        let output = self
            .workspace
            .run_command(self.with_offload(request))
            .await?;
        let mut value = json!({
            "exit_code": output.exit_code,
            "stdout": output.stdout.clone(),
            "stderr": output.stderr.clone(),
            "timed_out": output.timed_out,
        });
        self.apply_offload(&output, &mut value);
        Ok(value)
    }

    /// Check the workspace, and answer even when nothing was configured.
    ///
    /// This used to return `{"configured": false, "passed": null}` — a shrug — whenever
    /// `[coder] validation_command` was unset, which is the default on the ACP path. The cost of
    /// that shrug is measurable: a run called `validate` at turn 8, doing exactly what its prompt
    /// asked, got the empty answer, worked nine more turns on code that did not compile, and only
    /// found out at turn 17 when it reached for `cargo test --workspace` itself. The instruction
    /// was followed and the tool had nothing to say.
    ///
    /// With no command configured, a cargo workspace gets `cargo check --workspace --all-targets`
    /// — the same check the acceptance gate runs, so an early `validate` and the final verdict
    /// cannot disagree about what "compiles" means. A non-cargo workspace still reports
    /// unconfigured, because inventing a check for a tree we do not understand is worse than
    /// admitting there is none.
    async fn validate(&self) -> Result<Value, ToolError> {
        let command = match self.validation_command.clone() {
            Some(command) => command,
            None => match self.default_check() {
                Some(command) => command,
                None => return Ok(json!({ "configured": false, "passed": null })),
            },
        };
        let output = self
            .workspace
            .run_command(self.with_offload(command))
            .await?;
        let mut value = json!({
            "configured": true,
            "passed": output.exit_code == Some(0) && !output.timed_out,
            "exit_code": output.exit_code,
            "stdout": output.stdout.clone(),
            "stderr": output.stderr.clone(),
            "timed_out": output.timed_out,
        });
        self.apply_offload(&output, &mut value);
        Ok(value)
    }
}

impl CodingToolRuntime {
    /// The check to run when a deployment configured none.
    ///
    /// `None` for a workspace with no `Cargo.toml`: a guessed check for an unknown stack would
    /// report failures the task is not about, which is worse than reporting nothing.
    fn default_check(&self) -> Option<CommandRequest> {
        if !self.workspace.root().join("Cargo.toml").is_file() {
            return None;
        }
        Some(CommandRequest {
            program: "cargo".to_string(),
            args: vec![
                "check".to_string(),
                "--workspace".to_string(),
                "--all-targets".to_string(),
            ],
            env: Default::default(),
            timeout_secs: None,
            output_max_bytes: None,
            offload_dir: None,
        })
    }
}

#[async_trait]
impl ToolRuntime for CodingToolRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        // Full tool surface; explore mode filters to EXPLORE_TOOL_NAMES (read-only) so the model
        // is not offered write tools. PathPolicy/CommandPolicy still enforce on invoke.
        let full = vec![
            tool(
                "list_files",
                "List workspace files with policy filtering.",
                json!({
                    "type": "object",
                    "properties": { "limit": { "type": "integer", "minimum": 1 } }
                }),
            ),
            tool(
                "grep",
                "Search file contents with a regular expression. Reach for this whenever you do not already know exactly where something lives — finding every caller of a function, every place a field is read, or whether a symbol exists at all. It is far cheaper than reading files and guessing, and an anchor built from a grep hit is one that actually exists. If you already know the file and want to see it, use read_file instead.",
                json!({
                    "type": "object",
                    "required": ["pattern"],
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Rust regex syntax. Braces, parentheses and brackets are special — escape them, or set fixed_strings to search for the text literally."
                        },
                        "path": {
                            "type": "string",
                            "description": "Subdirectory to search, relative to the workspace root. Use this to scope a search rather than a path-anchored glob."
                        },
                        "glob": {
                            "type": "string",
                            "description": "Filename filter such as *.rs. Matched against the file name only, so src/**/*.rs matches nothing — scope with path instead."
                        },
                        "output_mode": {
                            "type": "string",
                            "enum": ["files_with_matches", "content", "count"],
                            "description": "files_with_matches (default) lists the paths that match. content returns the matching lines. count returns per-file totals."
                        },
                        "-i": { "type": "boolean", "description": "Case-insensitive." },
                        "-C": { "type": "integer", "minimum": 0, "description": "Lines of context either side of a match, in content mode." },
                        "fixed_strings": { "type": "boolean", "description": "Treat pattern as literal text rather than a regex." },
                        "head_limit": { "type": "integer", "minimum": 0, "description": "Cap on results. Defaults to 250." }
                    }
                }),
            ),
            tool(
                "list_symbols",
                "Walk workspace files and extract top-level symbols (functions, structs, classes, etc.) across supported languages. Returns a structured map of file→symbols for quick codebase orientation.",
                json!({
                    "type": "object",
                    "properties": { "limit": { "type": "integer", "minimum": 1 } }
                }),
            ),
            tool(
                "read_file",
                "Read a file, optionally by line range.",
                json!({
                    "type": "object",
                    "required": ["path"],
                    "properties": {
                        "path": { "type": "string" },
                        "start_line": { "type": ["integer", "null"], "minimum": 1 },
                        "line_count": { "type": ["integer", "null"], "minimum": 1 }
                    }
                }),
            ),
            tool(
                "write_file",
                "Create a new file, or replace one whole file. To change part of an existing file use edit_file, hashline_edit or apply_patch instead — write_file replaces the entire contents.",
                json!({
                    "type": "object",
                    "required": ["path", "content"],
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" },
                        "overwrite": {
                            "type": "boolean",
                            "description": "Required to replace a file that already has content. Omit it when creating a new file."
                        },
                        "append": {
                            "type": "boolean",
                            "description": "Add content to the end of the file instead of replacing it. Use this to add a function, struct or test to an existing file."
                        }
                    }
                }),
            ),
            tool(
                "edit_file",
                "Replace an exact text span in a file. This is the required tool for any change to part of an existing file, however small.",
                json!({
                    "type": "object",
                    "required": ["path", "old", "new"],
                    "properties": {
                        "path": { "type": "string" },
                        "old": { "type": "string" },
                        "new": { "type": "string" },
                        "replace_all": {
                            "type": "boolean",
                            "description": "Replace every occurrence. Without it an anchor that matches more than once is rejected, so an edit cannot silently hit the wrong place."
                        }
                    }
                }),
            ),
            tool(
                "apply_patch",
                "Apply multiple exact replacements atomically after validating every edit.",
                json!({
                    "type": "object",
                    "required": ["edits"],
                    "properties": {
                        "edits": {
                            "type": "array",
                            "minItems": 1,
                            "items": {
                                "type": "object",
                                "required": ["path", "old", "new"],
                                "properties": {
                                    "path": { "type": "string" },
                                    "old": { "type": "string" },
                                    "new": { "type": "string" }
                                }
                            }
                        }
                    }
                }),
            ),
            tool(
                "hashline_edit",
                "Apply a line-anchored hashline patch. Each section starts with [path#TAG] from read_file; use PUT N.=M: / PUT <N: / PUT >N: / CUT N.=M / REM with + body rows.",
                json!({
                    "type": "object",
                    "required": ["input"],
                    "properties": {
                        "input": {
                            "type": "string",
                            "description": "Hashline patch text with [path#TAG] sections and PUT/CUT/REM operations."
                        }
                    }
                }),
            ),
            tool(
                "git_status",
                "Return git status --porcelain.",
                json!({ "type": "object" }),
            ),
            tool(
                "git_diff",
                "Return git diff in name_only, stat, or patch mode.",
                json!({
                    "type": "object",
                    "properties": {
                        "mode": { "type": "string", "enum": ["name_only", "stat", "patch"] }
                    }
                }),
            ),
            tool(
                "git_branch",
                "Create and switch to a new git branch.",
                json!({
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": { "type": "string" }
                    }
                }),
            ),
            tool(
                "git_commit",
                "Stage files and create a git commit with the given message. Stages all changes when no files are listed.",
                json!({
                    "type": "object",
                    "required": ["message"],
                    "properties": {
                        "message": { "type": "string" },
                        "files": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    }
                }),
            ),
            tool(
                "git_push",
                "Push the current branch to a remote.",
                json!({
                    "type": "object",
                    "properties": {
                        "remote": { "type": "string" },
                        "branch": { "type": "string" },
                        "set_upstream": { "type": "boolean" }
                    }
                }),
            ),
            tool(
                "git_log",
                "Show structured commit history (--format, --max-count, optional branch).",
                json!({
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "default": 20 },
                        "branch": { "type": "string" },
                        "format": { "type": "string", "default": "%h %s" }
                    }
                }),
            ),
            tool(
                "git_fetch",
                "Fetch refs from a remote. Use before reviewing remote branches.",
                json!({
                    "type": "object",
                    "properties": {
                        "remote": { "type": "string", "default": "origin" },
                        "branch": { "type": "string" }
                    }
                }),
            ),
            tool(
                "git_merge",
                "Merge a branch into the current branch. Use after review.",
                json!({
                    "type": "object",
                    "required": ["branch"],
                    "properties": {
                        "branch": { "type": "string" },
                        "fast_forward_only": { "type": "boolean" }
                    }
                }),
            ),
            tool(
                "run_command_background",
                "Start a long-running command (build, test suite) in the background. Returns a job_id immediately; use check_background to poll for completion.",
                json!({
                    "type": "object",
                    "required": ["program"],
                    "properties": {
                        "program": { "type": "string" },
                        "args": { "type": "array", "items": { "type": "string" } }
                    }
                }),
            ),
            tool(
                "check_background",
                "Check the status of a background job started with run_command_background. Returns 'running', 'completed' (with output), or 'failed'.",
                json!({
                    "type": "object",
                    "required": ["job_id"],
                    "properties": {
                        "job_id": { "type": "string" }
                    }
                }),
            ),
            tool(
                "run_command",
                "Run a policy-checked command in the workspace.",
                json!({
                    "type": "object",
                    "required": ["program"],
                    "properties": {
                        "program": { "type": "string" },
                        "args": { "type": "array", "items": { "type": "string" } }
                    }
                }),
            ),
            tool(
                "validate",
                "Run the configured validation command.",
                json!({ "type": "object" }),
            ),
        ];
        // Exactly one edit tool family is offered, never two. This used to *add* hashline_edit
        // while leaving edit_file and apply_patch in place, so the model had a line-numbered read
        // view and two tools that match raw text — it pasted the numbered view into the text
        // tools in 14 of 41 calls in one run. oh-my-pi avoids this by construction: its
        // `edit.mode` is an enum. `hashline.enabled` is that enum, spelled as a boolean.
        let mut full: Vec<ToolDef> = if self.hashline.enabled {
            full.into_iter()
                .filter(|t| t.name != "edit_file" && t.name != "apply_patch")
                .collect()
        } else {
            full.into_iter()
                .filter(|t| t.name != "hashline_edit")
                .collect()
        };
        if self.path_policy.writes_disabled() {
            full.retain(|t| EXPLORE_TOOL_NAMES.contains(&t.name.as_str()));
        }
        if let Some(names) = &self.offered_tools {
            full.retain(|t| names.iter().any(|n| n == &t.name));
        }
        full
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        // Defense in depth: even if a write tool is named, read-only policy refuses it.
        if self.path_policy.writes_disabled() && !EXPLORE_TOOL_NAMES.contains(&call.name.as_str()) {
            return Err(format!(
                "tool '{}' is not available in explore (read-only) mode",
                call.name
            ));
        }
        if let Some(names) = &self.offered_tools
            && !names.iter().any(|n| n == &call.name)
        {
            return Err(format!(
                "tool '{}' is not offered; search with run_command if you need grep",
                call.name
            ));
        }
        self.invoke_json(&call.name, call.arguments.clone())
            .await
            .and_then(|value| {
                serde_json::to_string(&value).map_err(|e| ToolError::BadRequest(e.to_string()))
            })
            .map_err(|e| e.to_string())
    }

    /// Tools that only observe the workspace, so the executor may run them concurrently.
    ///
    /// `validate` is deliberately **not** here: it shells out to the operator's configured
    /// validation command (`cargo test`, `pytest`, …), which writes build artefacts into the
    /// workspace and is the most expensive call in the set. Running it as if it were a reader
    /// would let a build mutate the tree while sibling reads are observing it.
    fn is_read_only(&self, tool_name: &str) -> bool {
        matches!(
            tool_name,
            "read_file"
                | "search_text"
                | "list_files"
                | "list_symbols"
                | "git_status"
                | "git_diff"
                | "git_log"
        )
    }
}

fn tool(name: &str, description: &str, parameters: Value) -> ToolDef {
    ToolDef::new(name, description, parameters)
}

/// Whether `program` is build-like for background concurrency limits.
///
/// Matches case-insensitively on the executable stem only, so `/usr/bin/cargo`, `cargo`, and
/// `cargo.exe` all match `cargo`.
fn is_build_like(program: &str, build_like_programs: &[String]) -> bool {
    let stem = Path::new(program)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(program);
    build_like_programs
        .iter()
        .any(|p| p.eq_ignore_ascii_case(stem))
}

fn parse_args<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|e| ToolError::BadRequest(e.to_string()))
}

fn default_diff_mode() -> String {
    "patch".to_string()
}

fn default_remote() -> String {
    "origin".to_string()
}

fn fs_err(error: std::io::Error) -> ToolError {
    ToolError::Filesystem(error.to_string())
}

fn path_denied(rel: &str, policy: &PathPolicy) -> bool {
    policy
        .deny_globs
        .iter()
        .any(|pattern| path_matches(pattern, rel))
}

fn path_allowed_to_write(rel: &str, policy: &PathPolicy) -> bool {
    policy
        .allow_write_globs
        .iter()
        .any(|pattern| path_matches(pattern, rel))
}

fn path_matches(pattern: &str, rel: &str) -> bool {
    let pattern = pattern.replace('\\', "/");
    let rel = rel.replace('\\', "/");
    pattern == "**"
        || pattern == rel
        || pattern
            .strip_suffix("/**")
            .is_some_and(|prefix| rel == prefix || rel.starts_with(&format!("{prefix}/")))
}

/// How much untracked file content one `git_diff --mode patch` may inline, across all files.
///
/// Generous enough that a new module arrives whole — the case this exists for — and bounded so a
/// checked-in fixture or a generated file cannot swallow the turn.
const UNTRACKED_PATCH_BUDGET: usize = 24_000;

/// Truncate to at most `max` bytes without splitting a UTF-8 character.
///
/// Public because the critic's diff builder in `coder-agent` bounds untracked file content the
/// same way, and two copies of a byte-slicing helper is one copy too many.
pub fn truncate_on_char(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_core::DockerSandboxSpec;

    /// A runtime with hashline **explicitly off**.
    ///
    /// These tests predate hashline mode and assert `read_file`'s plain output. They used to
    /// inherit that from `HashlineConfig::default()`, which then flipped to enabled — so five of
    /// them broke at once for a reason none of them mentioned. Pinning it here makes each test
    /// state the mode it is testing instead of borrowing a global decision that can move.
    fn runtime() -> (tempfile::TempDir, CodingToolRuntime) {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap()
                .with_hashline(HashlineConfig {
                    enabled: false,
                    hash_length: HashlineConfig::HASH_LENGTH_MIN,
                });
        (dir, runtime)
    }

    // ── edit reliability: the failure modes two dispatched runs actually hit ──────────────

    /// The Windows case. `core.autocrlf` is on by default there, so a checkout holds `\r\n`
    /// while every model emits `\n`. Matching raw bytes fails with "not found" and no error
    /// message can explain why. The file must still be CRLF afterwards.
    #[tokio::test]
    async fn an_lf_anchor_edits_a_crlf_file_and_leaves_it_crlf() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("crlf.rs"), "fn a() {}\r\nfn b() {}\r\n").unwrap();

        runtime
            .invoke_json(
                "edit_file",
                json!({"path": "crlf.rs", "old": "fn b() {}\n", "new": "fn c() {}\n"}),
            )
            .await
            .expect("an LF anchor must match a CRLF file");

        let after = std::fs::read_to_string(dir.path().join("crlf.rs")).unwrap();
        assert_eq!(
            after, "fn a() {}\r\nfn c() {}\r\n",
            "the file's own line endings must survive the edit"
        );
    }

    /// A BOM shifts every line-one anchor, and dropping it rewrites a file the edit never
    /// intended to touch.
    #[tokio::test]
    async fn a_bom_neither_blocks_the_anchor_nor_disappears() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("bom.rs"), "\u{feff}fn a() {}\n").unwrap();

        runtime
            .invoke_json(
                "edit_file",
                json!({"path": "bom.rs", "old": "fn a() {}", "new": "fn z() {}"}),
            )
            .await
            .expect("a leading BOM must not hide line one from the model");

        let after = std::fs::read_to_string(dir.path().join("bom.rs")).unwrap();
        assert_eq!(after, "\u{feff}fn z() {}\n");
    }

    /// An ambiguous anchor is still rejected by default — that is the guard against editing the
    /// wrong occurrence — but `replace_all` gives the model a way forward other than guessing.
    #[tokio::test]
    async fn replace_all_resolves_an_ambiguous_anchor() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("d.rs"), "let x = old;\nlet y = old;\n").unwrap();

        let err = runtime
            .invoke_json(
                "edit_file",
                json!({"path": "d.rs", "old": "old", "new": "new"}),
            )
            .await
            .expect_err("two matches must not be edited silently");
        assert!(
            err.to_string().contains("replace_all"),
            "the error must name the way out, not just the problem: {err}"
        );

        let out = runtime
            .invoke_json(
                "edit_file",
                json!({"path": "d.rs", "old": "old", "new": "new", "replace_all": true}),
            )
            .await
            .expect("replace_all must be honoured");
        assert_eq!(out["replacements"], 2);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("d.rs")).unwrap(),
            "let x = new;\nlet y = new;\n"
        );
    }

    /// "old text was not found" is correct and useless. The model's only remaining move is to
    /// re-read the file, and a run that cannot infer that spends its turns on variants of the
    /// same wrong anchor — 15 of 25 edit calls in one dispatched run.
    #[tokio::test]
    async fn a_missing_anchor_tells_the_model_to_re_read() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("e.rs"), "fn a() {}\n").unwrap();

        let err = runtime
            .invoke_json(
                "edit_file",
                json!({"path": "e.rs", "old": "fn nowhere() {}", "new": "x"}),
            )
            .await
            .expect_err("a missing anchor is an error");
        let message = err.to_string();
        assert!(
            message.contains("read it again"),
            "the error must name the recovery action: {message}"
        );
        assert!(
            message.contains("e.rs"),
            "the error must name the file: {message}"
        );
    }

    /// An edit that changes nothing must not report success. A run that believes it has made
    /// progress stops looking for the reason it has not.
    #[tokio::test]
    async fn an_edit_that_changes_nothing_is_rejected() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("f.rs"), "same\n").unwrap();
        runtime
            .invoke_json(
                "edit_file",
                json!({"path": "f.rs", "old": "same", "new": "same"}),
            )
            .await
            .expect_err("a no-op edit must not read as an edit");
    }

    /// The move both failed runs were trying to make. "Add a struct to this file" is an append;
    /// with no way to say so, the model reached for `write_file` and destroyed 3,921 lines.
    #[tokio::test]
    async fn append_adds_to_a_file_without_destroying_it() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("g.rs"), "fn existing() {}\n").unwrap();

        runtime
            .invoke_json(
                "write_file",
                json!({"path": "g.rs", "content": "fn added() {}\n", "append": true}),
            )
            .await
            .expect("append must not need the overwrite flag");

        assert_eq!(
            std::fs::read_to_string(dir.path().join("g.rs")).unwrap(),
            "fn existing() {}\nfn added() {}\n",
            "append must keep every existing line"
        );
    }

    /// Appending LF text to a CRLF file would leave it mixed — and a mixed file is one the edit
    /// tools then refuse to normalize, so the damage compounds silently.
    #[tokio::test]
    async fn append_keeps_the_files_own_line_endings() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("h.rs"), "fn a() {}\r\n").unwrap();

        runtime
            .invoke_json(
                "write_file",
                json!({"path": "h.rs", "content": "fn b() {}\n", "append": true}),
            )
            .await
            .expect("append");

        let after = std::fs::read_to_string(dir.path().join("h.rs")).unwrap();
        assert_eq!(after, "fn a() {}\r\nfn b() {}\r\n", "got {after:?}");
    }

    #[tokio::test]
    async fn append_creates_a_file_that_does_not_exist_yet() {
        let (dir, runtime) = runtime();
        runtime
            .invoke_json(
                "write_file",
                json!({"path": "new/deep.rs", "content": "first\n", "append": true}),
            )
            .await
            .expect("appending to nothing is a create");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new/deep.rs")).unwrap(),
            "first\n"
        );
    }

    #[tokio::test]
    async fn append_and_overwrite_together_are_rejected() {
        let (_dir, runtime) = runtime();
        runtime
            .invoke_json(
                "write_file",
                json!({"path": "i.txt", "content": "x", "append": true, "overwrite": true}),
            )
            .await
            .expect_err("opposite intents must not be guessed at");
    }

    /// The refusal has to point at the alternative that would have worked. Both failed runs
    /// wanted to add to a file, and neither was told there was a way to.
    #[tokio::test]
    async fn the_clobber_refusal_offers_append() {
        let (_dir, runtime) = runtime();
        runtime
            .invoke_json(
                "write_file",
                json!({"path": "j.txt", "content": "one\ntwo\n"}),
            )
            .await
            .unwrap();
        let err = runtime
            .invoke_json("write_file", json!({"path": "j.txt", "content": "stub"}))
            .await
            .expect_err("clobber must be refused");
        let message = err.to_string();
        assert!(message.contains("append"), "must offer append: {message}");
        assert!(
            message.contains("edit_file"),
            "must offer edit_file: {message}"
        );
    }

    // ── one edit tool family at a time ───────────────────────────────────────────────────

    /// With hashline on, the raw-text edit tools must be gone.
    ///
    /// Offering both gave the model a line-numbered read view alongside tools that match raw
    /// text, and it pasted the numbered view into the text tools in 14 of 41 calls — the worst
    /// anchor failure rate of four dispatched runs. `oh-my-pi` makes `edit.mode` an enum for
    /// exactly this reason.
    #[test]
    fn hashline_mode_offers_no_raw_text_edit_tool() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap()
                .with_hashline(HashlineConfig {
                    enabled: true,
                    hash_length: 7,
                });
        let names: Vec<String> = runtime.catalog().into_iter().map(|t| t.name).collect();
        assert!(names.contains(&"hashline_edit".to_string()), "{names:?}");
        assert!(
            !names.contains(&"edit_file".to_string()),
            "edit_file must not be offered beside hashline_edit: {names:?}"
        );
        assert!(
            !names.contains(&"apply_patch".to_string()),
            "apply_patch matches raw text and must not be offered beside hashline_edit: {names:?}"
        );
    }

    /// And with hashline off, the text tools are present and hashline_edit is not.
    #[test]
    fn text_mode_offers_no_hashline_tool() {
        let (_dir, runtime) = runtime();
        let names: Vec<String> = runtime.catalog().into_iter().map(|t| t.name).collect();
        assert!(names.contains(&"edit_file".to_string()), "{names:?}");
        assert!(names.contains(&"apply_patch".to_string()), "{names:?}");
        assert!(!names.contains(&"hashline_edit".to_string()), "{names:?}");
    }

    // ── fuzzy anchors ────────────────────────────────────────────────────────────────────

    /// The exact failure from a dispatched run: the anchor was right in every character except
    /// four leading spaces the file does not have.
    #[tokio::test]
    async fn an_anchor_with_the_wrong_indentation_still_edits() {
        let (dir, runtime) = runtime();
        std::fs::write(
            dir.path().join("t.rs"),
            "mod a {}\n/// Receives a TurnRecord per completed turn.\npub trait T {}\n",
        )
        .unwrap();

        runtime
            .invoke_json(
                "edit_file",
                json!({
                    "path": "t.rs",
                    "old": "    /// Receives a TurnRecord per completed turn.",
                    "new": "    /// Receives a TurnRecord and a RequestRecord."
                }),
            )
            .await
            .expect("a near-exact anchor must not cost the run a turn");

        let after = std::fs::read_to_string(dir.path().join("t.rs")).unwrap();
        assert!(after.contains("and a RequestRecord"), "{after}");
        assert!(
            after.starts_with("mod a {}\n/// Receives"),
            "the replacement must be re-indented to the file, not the model's guess:\n{after}"
        );
    }

    /// Fuzzy must never override an exact match. An edit that worked before must behave
    /// identically, or this is a regression wearing a feature's clothes.
    #[tokio::test]
    async fn an_exact_anchor_is_unaffected_by_fuzzy_matching() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("u.rs"), "let a = 1;\nlet b = 2;\n").unwrap();
        runtime
            .invoke_json(
                "edit_file",
                json!({"path": "u.rs", "old": "let b = 2;", "new": "let b = 3;"}),
            )
            .await
            .expect("exact");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("u.rs")).unwrap(),
            "let a = 1;\nlet b = 3;\n"
        );
    }

    /// Two plausible sites must be refused, not chosen between. An edit in the wrong place is
    /// reported as success and ships; a refused edit costs one turn.
    #[tokio::test]
    async fn an_ambiguous_near_match_is_refused() {
        let (dir, runtime) = runtime();
        std::fs::write(
            dir.path().join("v.rs"),
            "fn a() {\n\tdo_work();\n}\nfn b() {\n\tdo_work();\n}\n",
        )
        .unwrap();
        let err = runtime
            .invoke_json(
                "edit_file",
                json!({"path": "v.rs", "old": "  do_work();", "new": "  do_more();"}),
            )
            .await
            .expect_err("two candidates must not be silently resolved");
        let m = err.to_string();
        assert!(
            m.contains("close enough") || m.contains("matched 2 times"),
            "the error must say why it refused: {m}"
        );
    }

    /// A miss should quote the near miss. "Not found" alone left one run spending 21 of 50 edits
    /// on variants of the same wrong anchor.
    #[tokio::test]
    async fn a_miss_quotes_the_closest_text() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("w.rs"), "pub fn handle_request(&self) {}\n").unwrap();
        let err = runtime
            .invoke_json(
                "edit_file",
                json!({"path": "w.rs", "old": "pub fn handle_response(&self) {}", "new": "x"}),
            )
            .await
            .expect_err("text this different is a miss");
        let m = err.to_string();
        assert!(
            m.contains("closest text is at line"),
            "the model needs to see what was nearly it: {m}"
        );
    }

    /// The cost of matching at 0.95, recorded rather than discovered later.
    ///
    /// A one-character difference in a short line clears the threshold, so `handle_requests`
    /// edits `handle_request`. That is the trade `oh-my-pi` ships and it is the right one here:
    /// the alternative is rejecting the indentation near-misses that made up a large share of
    /// our anchor failures. It is tolerable only because ambiguity is still refused — a second
    /// plausible site turns this into an error rather than a coin flip.
    #[tokio::test]
    async fn a_single_character_difference_is_accepted_when_unambiguous() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("y.rs"), "pub fn handle_request() {}\n").unwrap();
        runtime
            .invoke_json(
                "edit_file",
                json!({"path": "y.rs", "old": "pub fn handle_requests() {}", "new": "pub fn go() {}"}),
            )
            .await
            .expect("a 96%-similar unique anchor is accepted");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("y.rs")).unwrap(),
            "pub fn go() {}\n"
        );
    }

    /// Turning fuzzy off must restore exact-only behaviour, so a deployment that wants strictness
    /// can have it.
    #[tokio::test]
    async fn fuzzy_can_be_switched_off() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap()
                .with_hashline(HashlineConfig {
                    enabled: false,
                    hash_length: 7,
                })
                .with_edit(liberado_coder_core::EditConfig {
                    fuzzy_match: false,
                    fuzzy_threshold: 0.95,
                });
        std::fs::write(dir.path().join("x.rs"), "let a = 1;\n").unwrap();
        runtime
            .invoke_json(
                "edit_file",
                json!({"path": "x.rs", "old": "    let a = 1;", "new": "let a = 2;"}),
            )
            .await
            .expect_err("with fuzzy off, a near-exact anchor must still be rejected");
    }

    /// The failure this guard exists for.
    ///
    /// A run called `write_file` on a 3,921-line source file believing it was adding a struct.
    /// `std::fs::write` truncated it to 40 lines, no error, and the same turn did it to two more
    /// files: 5,825 deletions against 54 insertions. Nothing in the tool looked at what was
    /// already there.
    #[tokio::test]
    async fn write_file_refuses_to_silently_replace_an_existing_file() {
        let (_dir, runtime) = runtime();
        runtime
            .invoke_json(
                "write_file",
                json!({"path": "src/big.rs", "content": "line one
line two
line three
"}),
            )
            .await
            .expect("creating a new file is allowed");

        let err = runtime
            .invoke_json(
                "write_file",
                json!({"path": "src/big.rs", "content": "stub
"}),
            )
            .await
            .expect_err("replacing existing content must not be silent");

        let message = err.to_string();
        assert!(
            message.contains("3 lines"),
            "the error must say how much would be lost: {message}"
        );
        assert!(
            message.contains("edit_file"),
            "the error must name the tool the model should have used: {message}"
        );

        let survived = std::fs::read_to_string(_dir.path().join("src/big.rs")).expect("read");
        assert!(
            survived.contains("line three"),
            "the refusal must leave the file untouched, got: {survived:?}"
        );
    }

    /// The bypass a dispatched run actually used.
    ///
    /// It called write_file on a 1,726-line file, was refused, read the refusal — which ended
    /// "To replace it deliberately, pass overwrite: true" — and re-sent the identical call with
    /// that flag. 1,659 lines went. The message taught the model how to defeat the guard.
    #[tokio::test]
    async fn the_refusal_does_not_name_the_flag_that_bypasses_it() {
        let (_dir, runtime) = runtime();
        runtime
            .invoke_json(
                "write_file",
                json!({"path": "big.rs", "content": "a
b
c
d
e
"}),
            )
            .await
            .unwrap();
        let err = runtime
            .invoke_json(
                "write_file",
                json!({"path": "big.rs", "content": "stub
"}),
            )
            .await
            .expect_err("clobber is refused");
        let m = err.to_string();
        assert!(
            !m.contains("overwrite"),
            "an error that names its own bypass is a hint, not a guard: {m}"
        );
        assert!(
            m.contains("append"),
            "it must still offer the safe route: {m}"
        );
        assert!(m.contains("edit_file"), "and the correct one: {m}");
    }

    /// And the flag itself no longer suffices for a truncation.
    #[tokio::test]
    async fn overwrite_cannot_truncate_a_substantial_file() {
        let (dir, runtime) = runtime();
        let body: String = (0..200)
            .map(|i| {
                format!(
                    "line {i}
"
                )
            })
            .collect();
        std::fs::write(dir.path().join("huge.rs"), &body).unwrap();

        let err = runtime
            .invoke_json(
                "write_file",
                json!({"path": "huge.rs", "content": "stub
", "overwrite": true}),
            )
            .await
            .expect_err("cutting 200 lines to 1 is a truncation, not a rewrite");
        assert!(
            err.to_string().contains("truncation"),
            "the error must name what it is refusing: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("huge.rs")).unwrap(),
            body,
            "the file must be untouched"
        );
    }

    /// A genuine condensing rewrite is still allowed — the guard is about scale, not intent.
    #[tokio::test]
    async fn overwrite_still_allows_a_real_rewrite() {
        let (dir, runtime) = runtime();
        let body: String = (0..100)
            .map(|i| {
                format!(
                    "line {i}
"
                )
            })
            .collect();
        std::fs::write(dir.path().join("mid.rs"), &body).unwrap();
        let replacement: String = (0..40)
            .map(|i| {
                format!(
                    "new {i}
"
                )
            })
            .collect();
        runtime
            .invoke_json(
                "write_file",
                json!({"path": "mid.rs", "content": replacement, "overwrite": true}),
            )
            .await
            .expect("100 lines to 40 is a rewrite and must be allowed");
    }

    /// Small files stay freely replaceable; the guard must not turn every rewrite into a fight.
    #[tokio::test]
    async fn a_small_file_can_still_be_replaced_wholesale() {
        let (dir, runtime) = runtime();
        std::fs::write(
            dir.path().join("small.txt"),
            "one
two
three
",
        )
        .unwrap();
        runtime
            .invoke_json(
                "write_file",
                json!({"path": "small.txt", "content": "x
", "overwrite": true}),
            )
            .await
            .expect("a three-line file is not a truncation risk");
    }

    /// A deliberate rewrite is still possible — the flag makes it deliberate.
    #[tokio::test]
    async fn write_file_replaces_when_overwrite_is_asked_for() {
        let (_dir, runtime) = runtime();
        runtime
            .invoke_json(
                "write_file",
                json!({"path": "a.txt", "content": "old
"}),
            )
            .await
            .unwrap();
        runtime
            .invoke_json(
                "write_file",
                json!({"path": "a.txt", "content": "new
", "overwrite": true}),
            )
            .await
            .expect("an explicit overwrite must be honoured");
        assert_eq!(
            std::fs::read_to_string(_dir.path().join("a.txt")).unwrap(),
            "new
"
        );
    }

    /// An empty file is not content. Refusing here would block the ordinary
    /// create-then-fill sequence for no gain.
    #[tokio::test]
    async fn an_empty_existing_file_is_not_a_clobber() {
        let (_dir, runtime) = runtime();
        std::fs::write(_dir.path().join("empty.txt"), "").unwrap();
        runtime
            .invoke_json(
                "write_file",
                json!({"path": "empty.txt", "content": "now filled
"}),
            )
            .await
            .expect("writing over an empty file must be allowed");
    }

    #[tokio::test]
    async fn write_then_read_file() {
        let (_dir, runtime) = runtime();
        runtime
            .invoke_json(
                "write_file",
                json!({"path": "src/lib.rs", "content": "pub fn answer() -> u8 { 42 }"}),
            )
            .await
            .unwrap();
        let read = runtime
            .invoke_json("read_file", json!({"path": "src/lib.rs"}))
            .await
            .unwrap();
        assert_eq!(read["content"], "pub fn answer() -> u8 { 42 }");
    }

    #[tokio::test]
    async fn hashline_read_and_edit_when_enabled() {
        let (dir, runtime) = runtime();
        let runtime = runtime.with_hashline(HashlineConfig {
            enabled: true,
            hash_length: 6,
        });
        std::fs::write(
            dir.path().join("greet.py"),
            "def greet(name):\n    print(name)\n",
        )
        .unwrap();

        let read = runtime
            .invoke_json("read_file", json!({"path": "greet.py"}))
            .await
            .unwrap();
        assert_eq!(read["hashline"], true);
        let tag = read["tag"].as_str().unwrap();
        assert_eq!(tag.len(), 6);
        let content = read["content"].as_str().unwrap();
        assert!(content.starts_with(&format!("[greet.py#{tag}]")));
        assert!(content.contains("1:def greet(name):"));

        // hashline_edit is in the catalog only when enabled
        assert!(runtime.catalog().iter().any(|t| t.name == "hashline_edit"));

        let patch =
            format!("[greet.py#{tag}]\nPUT 1.=2:\n+def greet(name):\n+    print(f'Hi {{name}}')\n");
        let result = runtime
            .invoke_json("hashline_edit", json!({ "input": patch }))
            .await
            .unwrap();
        assert_eq!(result["sections"], 1);
        let updated = std::fs::read_to_string(dir.path().join("greet.py")).unwrap();
        assert!(updated.contains("Hi {name}") || updated.contains("f'Hi"));
    }

    #[tokio::test]
    async fn hashline_edit_absent_when_disabled() {
        let (_dir, runtime) = runtime();
        assert!(!runtime.catalog().iter().any(|t| t.name == "hashline_edit"));
        let err = runtime
            .invoke_json("hashline_edit", json!({ "input": "[a#AAAA]\nREM" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("disabled"));
    }

    #[tokio::test]
    async fn hashline_partial_read_uses_full_file_tag() {
        let (dir, runtime) = runtime();
        let runtime = runtime.with_hashline(HashlineConfig {
            enabled: true,
            hash_length: 5,
        });
        let full = "line1\nline2\nline3\nline4\n";
        std::fs::write(dir.path().join("n.txt"), full).unwrap();
        let expected_tag = hashline_compute_file_hash(full, 5);

        let read = runtime
            .invoke_json(
                "read_file",
                json!({"path": "n.txt", "start_line": 2, "line_count": 2}),
            )
            .await
            .unwrap();
        assert_eq!(read["tag"], expected_tag);
        let content = read["content"].as_str().unwrap();
        assert!(content.starts_with(&format!("[n.txt#{expected_tag}]")));
        assert!(content.contains("2:line2"));
        assert!(content.contains("3:line3"));
        assert!(!content.contains("1:line1"));
    }

    #[tokio::test]
    async fn hashline_edit_rejects_stale_tag_without_write() {
        let (dir, runtime) = runtime();
        let runtime = runtime.with_hashline(HashlineConfig {
            enabled: true,
            hash_length: 4,
        });
        std::fs::write(dir.path().join("a.txt"), "original\n").unwrap();
        let err = runtime
            .invoke_json(
                "hashline_edit",
                json!({ "input": "[a.txt#ZZZZ]\nPUT 1.=1:\n+mutated\n" }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("stale"), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "original\n"
        );
    }

    #[tokio::test]
    async fn hashline_edit_rejects_empty_input() {
        let (_dir, runtime) = runtime();
        let runtime = runtime.with_hashline(HashlineConfig {
            enabled: true,
            hash_length: 4,
        });
        let err = runtime
            .invoke_json("hashline_edit", json!({ "input": "  \n" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty") || err.to_string().contains("hashline"));
    }

    #[tokio::test]
    async fn hashline_edit_multi_file_atomic() {
        let (dir, runtime) = runtime();
        let runtime = runtime.with_hashline(HashlineConfig {
            enabled: true,
            hash_length: 4,
        });
        std::fs::write(dir.path().join("a.txt"), "aaa\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "bbb\n").unwrap();
        let a_tag = hashline_compute_file_hash("aaa\n", 4);
        let b_tag = hashline_compute_file_hash("bbb\n", 4);
        let patch = format!("[a.txt#{a_tag}]\nPUT 1.=1:\n+AAA\n[b.txt#{b_tag}]\nPUT 1.=1:\n+BBB");
        let result = runtime
            .invoke_json("hashline_edit", json!({ "input": patch }))
            .await
            .unwrap();
        assert_eq!(result["sections"], 2);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "AAA\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "BBB\n"
        );
    }

    #[tokio::test]
    async fn hashline_edit_rem_deletes_file() {
        let (dir, runtime) = runtime();
        let runtime = runtime.with_hashline(HashlineConfig {
            enabled: true,
            hash_length: 4,
        });
        let content = "delete me\n";
        std::fs::write(dir.path().join("trash.txt"), content).unwrap();
        let tag = hashline_compute_file_hash(content, 4);
        let result = runtime
            .invoke_json(
                "hashline_edit",
                json!({ "input": format!("[trash.txt#{tag}]\nREM") }),
            )
            .await
            .unwrap();
        assert_eq!(result["files"][0]["op"], "delete");
        assert!(!dir.path().join("trash.txt").exists());
    }

    #[tokio::test]
    async fn hashline_edit_respects_path_policy() {
        let (dir, _base) = runtime();
        let policy = PathPolicy {
            allow_write_globs: vec!["ok/**".into()],
            ..PathPolicy::default()
        };
        let runtime = CodingToolRuntime::new(dir.path(), CommandPolicy::default(), policy)
            .unwrap()
            .with_hashline(HashlineConfig {
                enabled: true,
                hash_length: 4,
            });
        std::fs::create_dir_all(dir.path().join("ok")).unwrap();
        std::fs::write(dir.path().join("ok/a.txt"), "x\n").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "y\n").unwrap();
        let secret_tag = hashline_compute_file_hash("y\n", 4);
        let err = runtime
            .invoke_json(
                "hashline_edit",
                json!({
                    "input": format!("[secret.txt#{secret_tag}]\nPUT 1.=1:\n+z\n")
                }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("denied") || err.to_string().contains("path"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("secret.txt")).unwrap(),
            "y\n"
        );
    }

    #[tokio::test]
    async fn hashline_edit_cut_and_insert() {
        let (dir, runtime) = runtime();
        let runtime = runtime.with_hashline(HashlineConfig {
            enabled: true,
            hash_length: 6,
        });
        let content = "one\ntwo\nthree\n";
        std::fs::write(dir.path().join("c.txt"), content).unwrap();
        let tag = hashline_compute_file_hash(content, 6);
        let patch = format!("[c.txt#{tag}]\nCUT 2.=2\nPUT >1:\n+inserted\n");
        runtime
            .invoke_json("hashline_edit", json!({ "input": patch }))
            .await
            .unwrap();
        let after = std::fs::read_to_string(dir.path().join("c.txt")).unwrap();
        assert_eq!(after, "one\ninserted\nthree\n");
    }

    #[tokio::test]
    async fn hashline_catalog_present_only_when_enabled() {
        let (_dir, off) = runtime();
        assert!(!off.catalog().iter().any(|t| t.name == "hashline_edit"));
        let on = off.with_hashline(HashlineConfig {
            enabled: true,
            hash_length: 4,
        });
        assert!(on.catalog().iter().any(|t| t.name == "hashline_edit"));
        // Explore (read-only) still omits write tools including hashline_edit.
        let dir = tempfile::tempdir().unwrap();
        let ro = CodingToolRuntime::new(
            dir.path(),
            CommandPolicy::default(),
            PathPolicy::read_only(),
        )
        .unwrap()
        .with_hashline(HashlineConfig {
            enabled: true,
            hash_length: 4,
        });
        assert!(!ro.catalog().iter().any(|t| t.name == "hashline_edit"));
        assert!(ro.catalog().iter().any(|t| t.name == "read_file"));
    }

    #[test]
    fn hashline_prompt_guidance_export_is_nonempty() {
        let g = hashline_prompt_guidance(4);
        assert!(g.contains("hashline_edit"));
        assert!(g.contains('4'));
    }

    #[tokio::test]
    async fn edit_file_rejects_ambiguous_old_text() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("a.txt"), "one\none\n").unwrap();
        let err = runtime
            .invoke_json(
                "edit_file",
                json!({"path": "a.txt", "old": "one", "new": "two"}),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("matched 2 times"));
    }

    #[tokio::test]
    async fn apply_patch_updates_multiple_files() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "beta\n").unwrap();

        let result = runtime
            .invoke_json(
                "apply_patch",
                json!({
                    "edits": [
                        {"path": "a.txt", "old": "alpha", "new": "ALPHA"},
                        {"path": "b.txt", "old": "beta", "new": "BETA"}
                    ]
                }),
            )
            .await
            .unwrap();

        assert_eq!(result["edits"], 2);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "ALPHA\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "BETA\n"
        );
    }

    #[tokio::test]
    async fn apply_patch_rejects_without_partial_write() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "beta\n").unwrap();

        let err = runtime
            .invoke_json(
                "apply_patch",
                json!({
                    "edits": [
                        {"path": "a.txt", "old": "alpha", "new": "ALPHA"},
                        {"path": "b.txt", "old": "missing", "new": "BETA"}
                    ]
                }),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("old text was not found"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "alpha\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "beta\n"
        );
    }

    /// The tool is named `grep` because that is the word the model already knows. The rename is
    /// the point of the change, so a catalog without it is a regression.
    #[tokio::test]
    async fn the_tool_is_offered_as_grep() {
        let (_dir, runtime) = runtime();
        let names: Vec<String> = runtime.catalog().into_iter().map(|t| t.name).collect();
        assert!(names.contains(&"grep".to_string()), "{names:?}");
    }

    #[tokio::test]
    async fn run_command_refuses_shell_redirect_tokens() {
        let (_dir, runtime) = runtime();
        let err = runtime
            .invoke_json(
                "run_command",
                json!({"program": "cargo", "args": ["test", "--workspace", "2>&1"]}),
            )
            .await
            .expect_err("2>&1 must not be passed through to cargo");
        let msg = err.to_string();
        assert!(
            msg.contains("not a shell"),
            "refuse must name the argv/shell distinction: {msg}"
        );
        assert!(msg.contains("2>&1"), "{msg}");
    }

    #[tokio::test]
    async fn run_command_obeys_command_policy() {
        let dir = tempfile::tempdir().unwrap();
        let current_exe = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let runtime = CodingToolRuntime::new(
            dir.path(),
            CommandPolicy {
                allow: vec![current_exe.clone()],
                ..CommandPolicy::default()
            },
            PathPolicy::default(),
        )
        .unwrap();

        let result = runtime
            .invoke_json(
                "run_command",
                json!({"program": current_exe, "args": ["--help"]}),
            )
            .await
            .unwrap();
        assert_eq!(result["timed_out"], false);
    }

    /// Backlog item C1: `run_command` must refuse git even though the default `allow` list is
    /// empty (which means "allow all"). The default `deny` entry carries that weight.
    #[tokio::test]
    async fn run_command_refuses_git() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let err = runtime
            .invoke_json(
                "run_command",
                json!({"program": "git", "args": ["status", "--porcelain"]}),
            )
            .await
            .expect_err("run_command must refuse git with the default policy");
        assert!(
            err.to_string().contains("denied"),
            "the refusal must name the policy deny, got: {err}"
        );
    }

    /// A cross-platform "print this text" command pair.
    fn echo_args(text: &str) -> (String, Vec<String>) {
        #[cfg(windows)]
        {
            (
                "cmd.exe".to_string(),
                vec!["/C".to_string(), "echo".to_string(), text.to_string()],
            )
        }
        #[cfg(not(windows))]
        {
            ("/bin/echo".to_string(), vec![text.to_string()])
        }
    }

    /// Backlog 0.9: an oversized `run_command` result is offloaded, not truncated.
    /// The model sees a head+tail preview plus a path `read_file` can open.
    #[tokio::test]
    async fn run_command_offloads_oversized_output_and_the_full_body_is_reachable() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = CodingToolRuntime::new(
            dir.path(),
            CommandPolicy {
                output_max_bytes: 16,
                ..CommandPolicy::default()
            },
            PathPolicy::default(),
        )
        .unwrap();

        let body = "M".repeat(1000);
        let (program, args) = echo_args(&body);
        let result = runtime
            .invoke_json("run_command", json!({"program": program, "args": args}))
            .await
            .unwrap();

        let rel = result["full_stdout_path"]
            .as_str()
            .expect("oversized stdout must carry a full_stdout_path");
        assert!(
            !rel.contains('\\'),
            "the path the model reads must use forward slashes: {rel}"
        );
        assert!(
            rel.starts_with(".liberado/offload/"),
            "offload path must sit under the workspace offload dir: {rel}"
        );
        let full = std::fs::read_to_string(dir.path().join(rel)).unwrap();
        assert_eq!(
            full.trim(),
            body,
            "offload file must hold the full body, not a preview"
        );
        assert_ne!(
            result["stdout"].as_str().unwrap(),
            full,
            "the model must see a preview, not the full body"
        );
        assert!(
            result["stdout"]
                .as_str()
                .unwrap()
                .contains("full stdout saved to"),
            "the preview must name the offload file: {}",
            result["stdout"]
        );
        assert!(
            result["stdout"].as_str().unwrap().contains("truncated"),
            "the preview must keep the truncation marker"
        );

        let reread = runtime
            .invoke_json("read_file", json!({"path": rel}))
            .await
            .unwrap();
        assert_eq!(
            reread["content"].as_str().unwrap().trim(),
            body,
            "read_file must return the offloaded full body: {}",
            reread["content"]
        );
    }

    /// Backlog 0.9: a result under the threshold is unchanged.
    #[tokio::test]
    async fn run_command_result_under_threshold_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = CodingToolRuntime::new(
            dir.path(),
            CommandPolicy {
                output_max_bytes: 64 * 1024,
                ..CommandPolicy::default()
            },
            PathPolicy::default(),
        )
        .unwrap();

        let (program, args) = echo_args("hi");
        let result = runtime
            .invoke_json("run_command", json!({"program": program, "args": args}))
            .await
            .unwrap();

        assert!(
            result.get("full_stdout_path").is_none(),
            "under threshold -> no offload key: {result}"
        );
        assert!(
            result.get("full_stderr_path").is_none(),
            "under threshold -> no stderr offload key: {result}"
        );
        assert_eq!(
            result["stdout"].as_str().unwrap().trim(),
            "hi",
            "stdout must be the full, untruncated output: {result}"
        );
        assert_eq!(result["exit_code"], 0, "{result}");
    }

    /// C1's other half: the dedicated git tools still work with the default policy — they go
    /// through the gix-backed path, not `run_command`.
    #[tokio::test]
    async fn dedicated_git_tool_still_works_with_default_policy() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let result = runtime
            .invoke_json("git_branch", json!({"name": "c1-test-branch"}))
            .await
            .unwrap();
        assert_eq!(result["branch"], "c1-test-branch");
        assert_eq!(result["exit_code"], 0);

        let current = std::process::Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let branch = String::from_utf8_lossy(&current.stdout).trim().to_string();
        assert_eq!(branch, "c1-test-branch");
    }

    /// A cargo workspace with no configured command still gets a real answer.
    ///
    /// The empty answer cost a run nine turns: it called `validate` at turn 8, was told
    /// `{"configured": false}`, and only discovered its code did not compile at turn 17.
    #[tokio::test]
    async fn validate_falls_back_to_a_compile_check_in_a_cargo_workspace() {
        let (dir, runtime) = runtime();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]
name = \"vtest\"
version = \"0.1.0\"
edition = \"2021\"
",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn f( -> u8 { 1 }
",
        )
        .unwrap();

        let out = runtime
            .invoke_json("validate", json!({}))
            .await
            .expect("validate");
        assert_eq!(
            out["configured"], true,
            "an unconfigured cargo workspace must still be checkable: {out}"
        );
        assert_eq!(
            out["passed"], false,
            "a workspace that does not compile must not pass: {out}"
        );
    }

    /// And a workspace that compiles passes, so the model can trust a green answer.
    #[tokio::test]
    async fn validate_passes_on_a_compiling_workspace() {
        let (dir, runtime) = runtime();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]
name = \"vok\"
version = \"0.1.0\"
edition = \"2021\"
",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn f() -> u8 { 1 }
",
        )
        .unwrap();

        let out = runtime
            .invoke_json("validate", json!({}))
            .await
            .expect("validate");
        assert_eq!(out["passed"], true, "{out}");
    }

    /// A tree we do not understand still reports unconfigured. Guessing a check for an unknown
    /// stack would fail the run on something the task was never about.
    #[tokio::test]
    async fn validate_stays_unconfigured_outside_a_cargo_workspace() {
        let (_dir, runtime) = runtime();
        let out = runtime
            .invoke_json("validate", json!({}))
            .await
            .expect("validate");
        assert_eq!(out["configured"], false, "{out}");
    }

    #[tokio::test]
    async fn validate_reports_unconfigured_state() {
        let (_dir, runtime) = runtime();
        let result = runtime.invoke_json("validate", json!({})).await.unwrap();
        assert_eq!(result["configured"], false);
        assert!(result["passed"].is_null());
    }

    #[tokio::test]
    async fn can_construct_runtime_from_docker_sandbox_spec() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = CodingToolRuntime::from_sandbox(
            dir.path(),
            SandboxSpec::Docker(DockerSandboxSpec {
                image: "liberado-coder:latest".to_string(),
                network: Some("none".to_string()),
                env_allowlist: Vec::new(),
                volumes: Vec::new(),
                user: None,
            }),
            CommandPolicy::default(),
            PathPolicy::default(),
        )
        .await
        .unwrap()
        // Plain `read_file` output is what this test checks, so the mode is stated rather than
        // inherited from a default that has already moved once.
        .with_hashline(HashlineConfig {
            enabled: false,
            hash_length: HashlineConfig::HASH_LENGTH_MIN,
        });

        runtime
            .invoke_json(
                "write_file",
                json!({"path": "hello.txt", "content": "hello\n"}),
            )
            .await
            .unwrap();
        let read = runtime
            .invoke_json("read_file", json!({"path": "hello.txt"}))
            .await
            .unwrap();
        assert_eq!(read["content"], "hello");
    }

    #[tokio::test]
    async fn edit_file_writes_unique_old_text() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("doc.txt"), "original content\n").unwrap();

        let result = runtime
            .invoke_json(
                "edit_file",
                json!({"path": "doc.txt", "old": "original", "new": "REVISED"}),
            )
            .await
            .unwrap();

        assert_eq!(result["replacements"], 1);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("doc.txt")).unwrap(),
            "REVISED content\n"
        );
    }

    #[tokio::test]
    async fn apply_patch_rejects_ambiguous_edit() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("dup.txt"), "duplicate\nduplicate\n").unwrap();

        let err = runtime
            .invoke_json(
                "apply_patch",
                json!({
                    "edits": [
                        {"path": "dup.txt", "old": "duplicate", "new": "replaced"}
                    ]
                }),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("matched 2 times"));
    }

    #[tokio::test]
    async fn git_status_returns_result() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("new.txt"), "new file\n").unwrap();

        let result = runtime.invoke_json("git_status", json!({})).await.unwrap();

        // Not in a git repo, so exit_code will be Some(128) but the tool should not crash.
        assert!(
            result["exit_code"].is_number() || result["timed_out"] == false,
            "git_status should return exit_code and timed_out"
        );
    }

    #[tokio::test]
    async fn git_diff_returns_result() {
        let _dir = tempfile::tempdir().unwrap();
        let runtime =
            CodingToolRuntime::new(_dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let result = runtime
            .invoke_json("git_diff", json!({"mode": "name_only"}))
            .await
            .unwrap();

        assert!(
            result["mode"] == "name_only",
            "git_diff should return the requested mode"
        );
    }

    /// A git repo with one committed file, for the untracked-diff tests.
    ///
    /// `user.email` / `user.name` are set explicitly: they exist on every dev machine and on no
    /// CI runner, so a `git commit` that relies on ambient identity passes locally and fails in
    /// CI. `commit.gpgsign=false` for the same reason in reverse — a developer who signs by
    /// default would otherwise block on a key the runner does not have.
    fn git_repo_with_one_committed_file() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("git available");
            assert!(
                status.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&status.stderr)
            );
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.email", "test@liberado.local"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.path().join("tracked.txt"), "one\n").unwrap();
        run(&["add", "tracked.txt"]);
        run(&["commit", "--quiet", "-m", "base"]);
        dir
    }

    /// The bug this closes, in the form it actually took.
    ///
    /// A run wrote a new module, then called `git_diff` four times and was shown nothing each
    /// time, because `git diff` reports tracked files only. The model concluded the file had
    /// never been written and wrote all 334 lines again.
    #[tokio::test]
    async fn a_new_file_appears_in_the_diff() {
        let dir = git_repo_with_one_committed_file();
        std::fs::write(dir.path().join("brand_new.rs"), "fn hello() {}\n").unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        for mode in ["name_only", "stat", "patch"] {
            let result = runtime
                .invoke_json("git_diff", json!({ "mode": mode }))
                .await
                .unwrap();
            let stdout = result["stdout"].as_str().unwrap_or_default();
            assert!(
                stdout.contains("brand_new.rs"),
                "mode {mode} must show a newly created file, got: {stdout:?}"
            );
        }
    }

    /// Names were what the critic already had, and they were not enough to review a change.
    #[tokio::test]
    async fn patch_mode_carries_the_new_file_content() {
        let dir = git_repo_with_one_committed_file();
        std::fs::write(dir.path().join("brand_new.rs"), "fn hello() {}\n").unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let result = runtime
            .invoke_json("git_diff", json!({"mode": "patch"}))
            .await
            .unwrap();
        let stdout = result["stdout"].as_str().unwrap_or_default();
        assert!(
            stdout.contains("+fn hello() {}"),
            "patch mode must carry the content, not only the name: {stdout:?}"
        );
    }

    /// Tracked edits must survive the addition. Appending the untracked section is worthless if
    /// it displaces the answer the tool already gave.
    #[tokio::test]
    async fn tracked_changes_still_appear_alongside_untracked_ones() {
        let dir = git_repo_with_one_committed_file();
        std::fs::write(dir.path().join("tracked.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.path().join("brand_new.rs"), "fn hello() {}\n").unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let result = runtime
            .invoke_json("git_diff", json!({"mode": "name_only"}))
            .await
            .unwrap();
        let stdout = result["stdout"].as_str().unwrap_or_default();
        assert!(stdout.contains("tracked.txt"), "{stdout:?}");
        assert!(stdout.contains("brand_new.rs"), "{stdout:?}");
    }

    /// `.gitignore` must be honoured, or `target/` alone makes the diff useless.
    #[tokio::test]
    async fn ignored_files_stay_out_of_the_diff() {
        let dir = git_repo_with_one_committed_file();
        std::fs::write(dir.path().join(".gitignore"), "noise/\n").unwrap();
        std::fs::create_dir(dir.path().join("noise")).unwrap();
        std::fs::write(dir.path().join("noise/huge.txt"), "x\n").unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let result = runtime
            .invoke_json("git_diff", json!({"mode": "name_only"}))
            .await
            .unwrap();
        let stdout = result["stdout"].as_str().unwrap_or_default();
        assert!(
            !stdout.contains("huge.txt"),
            "ignored files must not be reported: {stdout:?}"
        );
    }

    /// A clean tree must still read as clean. Reporting the untracked header with nothing under
    /// it would tell a model it has unsaved work when it has none.
    #[tokio::test]
    async fn a_clean_tree_gains_no_untracked_section() {
        let dir = git_repo_with_one_committed_file();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let result = runtime
            .invoke_json("git_diff", json!({"mode": "name_only"}))
            .await
            .unwrap();
        let stdout = result["stdout"].as_str().unwrap_or_default();
        assert!(
            !stdout.contains("untracked"),
            "a clean tree must not grow an untracked section: {stdout:?}"
        );
    }

    #[test]
    fn truncation_never_splits_a_character() {
        // "é" is two bytes; a cut at 1 would produce invalid UTF-8 if done by byte slicing.
        let s = "aé";
        assert_eq!(truncate_on_char(s, 2), "a");
        assert_eq!(truncate_on_char(s, 3), "aé");
        assert_eq!(truncate_on_char(s, 99), "aé");
    }

    #[tokio::test]
    async fn catalog_contains_expected_tools() {
        let (_dir, runtime) = runtime();
        let catalog = runtime.catalog();
        let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();
        for tool in &[
            "list_files",
            "grep",
            "list_symbols",
            "read_file",
            "write_file",
            "edit_file",
            "apply_patch",
            "git_status",
            "git_diff",
            "git_branch",
            "git_commit",
            "git_push",
            "run_command",
            "validate",
        ] {
            assert!(
                names.contains(tool),
                "catalog should contain {tool}, got: {names:?}"
            );
        }
    }

    #[test]
    fn a_configured_offered_set_is_what_the_model_sees() {
        let (_dir, runtime) = runtime();
        let runtime = runtime.with_offered_tools(Some(vec![
            "read_file".into(),
            "write_file".into(),
            "edit_file".into(),
            "run_command".into(),
        ]));
        let names: Vec<String> = runtime.catalog().into_iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            vec!["read_file", "write_file", "edit_file", "run_command"]
        );
    }

    #[tokio::test]
    async fn a_tool_not_in_the_offered_set_is_refused() {
        let (_dir, runtime) = runtime();
        let runtime = runtime.with_offered_tools(Some(vec![
            "read_file".into(),
            "write_file".into(),
            "edit_file".into(),
            "run_command".into(),
        ]));
        let err = runtime
            .invoke(&liberado_provider::ToolInvocation {
                id: "g1".into(),
                name: "grep".into(),
                arguments: json!({"pattern": "x", "path": "."}),
            })
            .await
            .expect_err("grep must not run when it is not offered");
        assert!(
            err.contains("not offered"),
            "refusal must name the gap, got: {err}"
        );
    }

    #[tokio::test]
    async fn invoke_round_trips_through_invoke_json() {
        let (_dir, runtime) = runtime();
        let call = liberado_provider::ToolInvocation {
            id: "t1".to_string(),
            name: "validate".to_string(),
            arguments: json!({}),
        };
        let result = runtime.invoke(&call).await.unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["configured"], false);
    }

    #[tokio::test]
    async fn write_blocked_by_path_policy() {
        let dir = tempfile::tempdir().unwrap();
        // Restrict writes to "src/**" only — anything else should be denied.
        let policy = PathPolicy {
            allow_write_globs: vec!["src/**".to_string()],
            ..PathPolicy::default()
        };
        let runtime = CodingToolRuntime::new(dir.path(), CommandPolicy::default(), policy).unwrap();

        let err = runtime
            .invoke_json("write_file", json!({"path": "README.md", "content": "x"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PathDenied(_)));

        // Writing to src/ should be allowed.
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let ok = runtime
            .invoke_json(
                "write_file",
                json!({"path": "src/main.rs", "content": "fn main() {}"}),
            )
            .await
            .unwrap();
        assert_eq!(ok["written"], true);
    }

    #[tokio::test]
    async fn dispatch_write_scope_allows_a_whitelist_over_its_blacklist() {
        use liberado_coder_core::DispatchWriteScope;

        let dir = tempfile::tempdir().unwrap();
        let policy = PathPolicy {
            write_scope: DispatchWriteScope {
                allow_globs: vec!["docs/**".to_string()],
                deny_globs: vec!["docs/**".to_string(), "src/**".to_string()],
            },
            ..PathPolicy::default()
        };
        let runtime = CodingToolRuntime::new(dir.path(), CommandPolicy::default(), policy).unwrap();

        let allowed = runtime
            .invoke_json(
                "write_file",
                json!({"path": "docs/guide.md", "content": "ok"}), // docs-check: ignore
            )
            .await
            .unwrap();
        assert_eq!(allowed["written"], true);

        let denied = runtime
            .invoke_json(
                "write_file",
                json!({"path": "src/main.rs", "content": "no"}),
            )
            .await
            .unwrap_err();
        assert!(matches!(denied, ToolError::PathDenied(_)));
    }

    #[tokio::test]
    async fn dispatch_write_scope_does_not_bypass_base_denials() {
        use liberado_coder_core::DispatchWriteScope;

        let dir = tempfile::tempdir().unwrap();
        let policy = PathPolicy {
            write_scope: DispatchWriteScope {
                allow_globs: vec![".git/**".to_string()],
                deny_globs: Vec::new(),
            },
            ..PathPolicy::default()
        };
        let runtime = CodingToolRuntime::new(dir.path(), CommandPolicy::default(), policy).unwrap();
        let denied = runtime
            .invoke_json(
                "write_file",
                json!({"path": ".git/config", "content": "no"}),
            )
            .await
            .unwrap_err();
        assert!(matches!(denied, ToolError::PathDenied(_)));
    }

    /// Plan mode is PathPolicy::plan_mode() only — no parallel write gate in the tools crate.
    #[tokio::test]
    async fn plan_mode_path_policy_allows_only_plan_artifact() {
        use liberado_coder_core::PLAN_ARTIFACT_REL;

        let dir = tempfile::tempdir().unwrap();
        let runtime = CodingToolRuntime::new(
            dir.path(),
            CommandPolicy::none_allowed(),
            PathPolicy::plan_mode(),
        )
        .unwrap();

        let err = runtime
            .invoke_json(
                "write_file",
                json!({"path": "src/main.rs", "content": "fn main() {}"}),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PathDenied(_)));

        let ok = runtime
            .invoke_json(
                "write_file",
                json!({"path": PLAN_ARTIFACT_REL, "content": "# Plan\n"}),
            )
            .await
            .unwrap();
        assert_eq!(ok["written"], true);

        // Shell is denied via CommandPolicy::none_allowed (empty-matching allow list).
        let shell_err = runtime
            .invoke_json("run_command", json!({"program": "echo", "args": ["hi"]}))
            .await
            .unwrap_err();
        assert!(
            matches!(shell_err, ToolError::Sandbox(_) | ToolError::BadRequest(_))
                || format!("{shell_err:?}").to_lowercase().contains("denied")
                || format!("{shell_err:?}").to_lowercase().contains("command"),
            "expected command denied, got {shell_err:?}"
        );
    }

    /// Explore mode reuses PathPolicy::read_only + catalog filter — no parallel tool stack.
    #[tokio::test]
    async fn explore_mode_catalog_is_read_only_and_writes_fail() {
        use liberado_coder_core::EXPLORE_TOOL_NAMES;
        use liberado_executor::ToolRuntime;

        let dir = tempfile::tempdir().unwrap();
        let runtime = CodingToolRuntime::new(
            dir.path(),
            CommandPolicy::none_allowed(),
            PathPolicy::read_only(),
        )
        .unwrap();

        let names: Vec<_> = runtime.catalog().into_iter().map(|t| t.name).collect();
        for n in EXPLORE_TOOL_NAMES {
            assert!(names.contains(&n.to_string()), "missing explore tool {n}");
        }
        assert!(!names.iter().any(|n| n == "write_file"));
        assert!(!names.iter().any(|n| n == "run_command"));

        let err = runtime
            .invoke(&liberado_provider::ToolInvocation {
                id: "1".into(),
                name: "write_file".into(),
                arguments: json!({"path": "x.rs", "content": "x"}),
            })
            .await
            .unwrap_err();
        assert!(
            err.contains("explore") || err.contains("read-only") || err.contains("denied"),
            "got {err}"
        );
    }

    pub(crate) fn init_temp_git_repo(dir: &std::path::Path) {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@test")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@test")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "--quiet"]);
        // Repo-local identity: exists on every dev machine and on no CI runner
        // unless we write it. `commit.gpgsign=false` for the reverse case.
        run(&["config", "user.email", "test@liberado.local"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("seed.txt"), "initial\n").unwrap();
        run(&["add", "seed.txt"]);
        run(&["commit", "-m", "initial commit"]);
    }

    #[tokio::test]
    async fn git_branch_creates_and_switches() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let result = runtime
            .invoke_json("git_branch", json!({"name": "feature-x"}))
            .await
            .unwrap();
        assert_eq!(result["branch"], "feature-x");
        assert_eq!(result["exit_code"], 0);

        let current = std::process::Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let branch = String::from_utf8_lossy(&current.stdout).trim().to_string();
        assert_eq!(branch, "feature-x");
    }

    /// Option injection: these values become argv entries, so a name like `-D` or `--force` would
    /// change what git does rather than name a branch. The guards existed but nothing held them —
    /// disabling all three `starts_with('-')` checks left the whole crate green.
    ///
    /// The three sites are separate arguments to separate commands, so this covers each rather than
    /// trusting one to stand for the others.
    #[tokio::test]
    async fn git_tools_reject_arguments_that_would_parse_as_options() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        for (tool, args) in [
            ("git_branch", json!({"name": "-D"})),
            ("git_push", json!({"remote": "--mirror"})),
            ("git_push", json!({"remote": "origin", "branch": "--force"})),
        ] {
            let Err(err) = runtime.invoke_json(tool, args.clone()).await else {
                panic!("{tool} accepted {args}, which git would read as an option");
            };
            assert!(
                err.to_string().contains("must not start with"),
                "{tool} must refuse a leading dash; got: {err}"
            );
        }
    }

    #[tokio::test]
    async fn git_branch_rejects_empty_name() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let err = runtime
            .invoke_json("git_branch", json!({"name": ""}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[tokio::test]
    async fn git_commit_stages_and_commits() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        std::fs::write(dir.path().join("new_file.txt"), "content\n").unwrap();

        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let result = runtime
            .invoke_json(
                "git_commit",
                json!({"message": "add new file", "files": ["new_file.txt"]}),
            )
            .await
            .unwrap();
        assert_eq!(result["committed"], true);
        assert_eq!(result["exit_code"], 0);

        let log = std::process::Command::new("git")
            .args(["log", "--oneline"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let log_text = String::from_utf8_lossy(&log.stdout);
        assert!(log_text.contains("add new file"));
    }

    #[tokio::test]
    async fn git_commit_stages_all_when_no_files_given() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();

        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let result = runtime
            .invoke_json("git_commit", json!({"message": "commit all"}))
            .await
            .unwrap();
        assert_eq!(result["committed"], true);
        assert_eq!(result["exit_code"], 0);

        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&status.stdout).trim().is_empty());
    }

    #[tokio::test]
    async fn git_commit_reports_stage_failure() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());

        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let result = runtime
            .invoke_json("git_commit", json!({"message": "empty commit"}))
            .await
            .unwrap();
        assert_eq!(result["committed"], false);
    }

    #[tokio::test]
    async fn git_commit_rejects_empty_message() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let err = runtime
            .invoke_json("git_commit", json!({"message": ""}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[tokio::test]
    async fn git_push_runs_push_command() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let result = runtime
            .invoke_json("git_push", json!({"remote": "origin", "branch": "main"}))
            .await
            .unwrap();

        assert!(
            result["exit_code"].is_number() || result["timed_out"] == false,
            "git_push should return exit_code and timed_out"
        );
    }

    #[tokio::test]
    async fn git_push_with_set_upstream() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let result = runtime
            .invoke_json(
                "git_push",
                json!({"remote": "origin", "branch": "main", "set_upstream": true}),
            )
            .await
            .unwrap();

        assert!(
            result["exit_code"].is_number() || result["timed_out"] == false,
            "git_push with set_upstream should return exit_code and timed_out"
        );
    }

    #[test]
    fn preflight_gh_pr_create_ignores_non_gh() {
        assert!(preflight_gh_pr_create("cargo", &["test".into()]).is_none());
    }

    #[test]
    fn preflight_gh_pr_create_ignores_without_base() {
        assert!(
            preflight_gh_pr_create(
                "gh",
                &["pr".into(), "create".into(), "--title".into(), "t".into()]
            )
            .is_none()
        );
    }

    #[test]
    fn preflight_gh_pr_create_flags_missing_remote_base() {
        // A branch name that almost certainly does not exist on origin.
        let err = preflight_gh_pr_create(
            "gh",
            &[
                "pr".into(),
                "create".into(),
                "--base".into(),
                "this-branch-does-not-exist-on-origin-zzzz".into(),
            ],
        );
        assert!(
            err.as_ref().is_some_and(
                |e| e.contains("refusing gh pr create") && e.contains("origin has no branch")
            ),
            "expected refusal for missing base, got {err:?}"
        );
    }

    #[tokio::test]
    async fn git_push_defaults_to_origin() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let result = runtime.invoke_json("git_push", json!({})).await.unwrap();

        assert!(
            result["exit_code"].is_number() || result["timed_out"] == false,
            "git_push with no args should default to origin"
        );
    }

    // ── git_log ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn git_log_returns_recent_commits() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        git_add_commit(dir.path(), "second commit");

        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();
        let result = runtime.invoke_json("git_log", json!({})).await.unwrap();
        assert_eq!(result["exit_code"], 0);
        let stdout = result["stdout"].as_str().unwrap();
        assert!(stdout.contains("second commit"));
        assert!(stdout.contains("initial commit"));
    }

    #[tokio::test]
    async fn git_log_respects_limit_and_branch() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), format!("{i}\n")).unwrap();
            git_add_commit(dir.path(), &format!("commit {i}"));
        }

        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();
        let result = runtime
            .invoke_json("git_log", json!({"limit": 2}))
            .await
            .unwrap();
        let stdout = result["stdout"].as_str().unwrap();
        let count = stdout.lines().filter(|l| !l.is_empty()).count();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn git_diff_stat_and_patch_modes() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        // seed.txt already exists from init, modify it so git diff has something to show
        std::fs::write(dir.path().join("seed.txt"), "modified content\n").unwrap();

        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let stat = runtime
            .invoke_json("git_diff", json!({"mode": "stat"}))
            .await
            .unwrap();
        assert!(stat["stdout"].as_str().unwrap().contains("seed.txt"));

        let patch = runtime
            .invoke_json("git_diff", json!({"mode": "patch"}))
            .await
            .unwrap();
        assert!(patch["stdout"].as_str().unwrap().contains("@@"));
    }

    #[tokio::test]
    async fn git_diff_rejects_unsupported_mode() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();
        let err = runtime
            .invoke_json("git_diff", json!({"mode": "invalid"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unsupported diff mode"));
    }

    #[tokio::test]
    async fn git_push_rejects_empty_branch() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();
        let err = runtime
            .invoke_json("git_push", json!({"remote": "origin", "branch": ""}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[tokio::test]
    async fn git_fetch_rejects_dash_prefixed_remote() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();
        let err = runtime
            .invoke_json("git_fetch", json!({"remote": "--depth"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must not start with '-'"));
    }

    #[tokio::test]
    async fn git_merge_rejects_dash_prefixed_branch() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();
        let err = runtime
            .invoke_json("git_merge", json!({"branch": "--no-ff"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must not start with '-'"));
    }

    #[tokio::test]
    async fn git_merge_rejects_empty_branch() {
        let dir = tempfile::tempdir().unwrap();
        init_temp_git_repo(dir.path());
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();
        let err = runtime
            .invoke_json("git_merge", json!({"branch": ""}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    // ── background jobs ───────────────────────────────────────────────

    /// A shell echo that exists on every runner.
    ///
    /// These tests hardcoded `cmd /c echo`, which is Windows-only. They were written on Windows
    /// while CI could not run, so nothing noticed until Linux executed them for the first time.
    fn echo_command(text: &str) -> (String, Vec<String>) {
        if cfg!(windows) {
            ("cmd".into(), vec!["/c".into(), "echo".into(), text.into()])
        } else {
            ("sh".into(), vec!["-c".into(), format!("echo {text}")])
        }
    }

    #[tokio::test]
    async fn background_job_roundtrip_running_then_completed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let (program, args) = echo_command("hello-from-background");
        let started = runtime
            .invoke_json(
                "run_command_background",
                json!({"program": program, "args": args}),
            )
            .await
            .unwrap();
        assert_eq!(started["status"], "running");
        let job_id = started["job_id"].as_str().unwrap().to_string();
        assert!(!job_id.is_empty());

        // Poll until completed — echo finishes fast, handle race
        let mut completed = false;
        for _ in 0..50 {
            let poll = runtime
                .invoke_json("check_background", json!({"job_id": job_id}))
                .await
                .unwrap();
            let status = poll["status"].as_str().unwrap();
            if status == "completed" {
                assert!(
                    poll["stdout"]
                        .as_str()
                        .unwrap_or("")
                        .contains("hello-from-background")
                );
                assert_eq!(poll["exit_code"], 0);
                completed = true;
                break;
            }
            assert!(
                status == "running" || status == "unknown",
                "unexpected status: {status}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(completed, "background job did not complete in time");
    }

    #[tokio::test]
    async fn check_background_unknown_job_id() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();
        let result = runtime
            .invoke_json("check_background", json!({"job_id": "nonexistent"}))
            .await
            .unwrap();
        assert_eq!(result["status"], "unknown");
    }

    // ── background job concurrency limits (F9) ────────────────────────

    #[tokio::test]
    async fn third_background_job_is_refused_with_running_job_id() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let (program, args) = echo_command("job1");
        let first = runtime
            .invoke_json(
                "run_command_background",
                json!({"program": program, "args": args}),
            )
            .await
            .unwrap();
        let first_id = first["job_id"].as_str().unwrap().to_string();

        let (program, args) = echo_command("job2");
        runtime
            .invoke_json(
                "run_command_background",
                json!({"program": program, "args": args}),
            )
            .await
            .unwrap();

        // Completed-but-unpolled jobs still occupy slots — third start must refuse.
        let (program, args) = echo_command("job3");
        let err = runtime
            .invoke_json(
                "run_command_background",
                json!({"program": program, "args": args}),
            )
            .await
            .unwrap_err();

        assert!(
            matches!(err, ToolError::BadRequest(_)),
            "refusal must be BadRequest-class: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("at most 2 background job"),
            "error should name the total cap: {msg}"
        );
        assert!(
            msg.contains("call check_background"),
            "error should tell the model how to react: {msg}"
        );
        assert!(
            msg.contains("job-"),
            "error should name a running job id: {msg}"
        );
        // Either of the two in-flight ids is fine; first is enough to prove identity is present.
        let _ = first_id;
    }

    #[tokio::test]
    async fn two_non_build_background_jobs_are_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let (program, args) = echo_command("job1");
        let r1 = runtime
            .invoke_json(
                "run_command_background",
                json!({"program": program, "args": args}),
            )
            .await
            .unwrap();
        assert_eq!(r1["status"], "running");

        let (program, args) = echo_command("job2");
        let r2 = runtime
            .invoke_json(
                "run_command_background",
                json!({"program": program, "args": args}),
            )
            .await
            .unwrap();
        assert_eq!(r2["status"], "running");
    }

    #[tokio::test]
    async fn second_build_like_while_first_unreaped_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        // Start a build-like job. `cargo` may finish or fail quickly in an empty tree; the slot
        // stays occupied until check_background reaps it.
        let first = runtime
            .invoke_json(
                "run_command_background",
                json!({"program": "cargo", "args": ["--version"]}),
            )
            .await
            .unwrap();
        let first_id = first["job_id"].as_str().unwrap().to_string();
        assert_eq!(first["status"], "running");

        let err = runtime
            .invoke_json(
                "run_command_background",
                json!({"program": "cargo", "args": ["build"]}),
            )
            .await
            .unwrap_err();

        assert!(
            matches!(err, ToolError::BadRequest(_)),
            "refusal must be BadRequest-class: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("build-like"),
            "error should mention the build-like cap: {msg}"
        );
        assert!(
            msg.contains(&first_id),
            "error should name the running build job id {first_id}: {msg}"
        );
        assert!(
            msg.contains("call check_background"),
            "error should tell the model how to react: {msg}"
        );
    }

    #[tokio::test]
    async fn non_build_job_does_not_count_as_build_like() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        // One non-build-like job in flight must not serialize a first build-like start.
        let (program, args) = echo_command("non-build");
        runtime
            .invoke_json(
                "run_command_background",
                json!({"program": program, "args": args}),
            )
            .await
            .unwrap();

        let result = runtime
            .invoke_json(
                "run_command_background",
                json!({"program": "cargo", "args": ["--version"]}),
            )
            .await;
        assert!(
            result.is_ok(),
            "a non-build job must not block the first build-like call: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn build_like_allowed_again_after_check_background_reaps() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();

        let started = runtime
            .invoke_json(
                "run_command_background",
                json!({"program": "cargo", "args": ["--version"]}),
            )
            .await
            .unwrap();
        let job_id = started["job_id"].as_str().unwrap().to_string();

        for _ in 0..50 {
            let poll = runtime
                .invoke_json("check_background", json!({"job_id": job_id}))
                .await
                .unwrap();
            if poll["status"] != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let started2 = runtime
            .invoke_json(
                "run_command_background",
                json!({"program": "cargo", "args": ["--version"]}),
            )
            .await;
        assert!(
            started2.is_ok(),
            "after reaping, a new build-like job should be allowed: {:?}",
            started2.err()
        );
    }

    #[tokio::test]
    async fn configured_background_limits_are_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let custom = BackgroundLimits {
            max_background: 1,
            max_build_like: 1,
            // Empty list → nothing is build-like; only the total cap matters here.
            build_like_programs: vec![],
        };
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap()
                .with_background_limits(custom);

        let (program, args) = echo_command("only-one");
        let started = runtime
            .invoke_json(
                "run_command_background",
                json!({"program": program, "args": args}),
            )
            .await
            .unwrap();
        assert_eq!(started["status"], "running");

        let (program, args) = echo_command("second");
        let err = runtime
            .invoke_json(
                "run_command_background",
                json!({"program": program, "args": args}),
            )
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("at most 1 background job"),
            "custom max_background=1 should be enforced: {msg}"
        );
    }

    #[test]
    fn is_build_like_matches_by_stem_case_insensitive() {
        let progs: Vec<String> = vec![
            "cargo".into(),
            "npm".into(),
            "pnpm".into(),
            "yarn".into(),
            "make".into(),
            "go".into(),
            "gradle".into(),
            "mvn".into(),
            "tsc".into(),
        ];

        assert!(is_build_like("cargo", &progs));
        assert!(is_build_like("/usr/bin/cargo", &progs));
        assert!(is_build_like("cargo.exe", &progs));
        assert!(is_build_like("Cargo", &progs));
        assert!(is_build_like("NPM", &progs));
        assert!(!is_build_like("sh", &progs));
        assert!(!is_build_like("echo", &progs));
        assert!(!is_build_like("git", &progs));
        assert!(!is_build_like("cmd", &progs));
    }

    // ── validate with configured command ──────────────────────────────

    #[tokio::test]
    async fn validate_with_configured_command_reports_configured_true() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap()
                .with_validation_command({
                    let (program, args) = echo_command("ok");
                    let mut req = CommandRequest::new(program);
                    req.args = args;
                    req
                });

        let result = runtime.invoke_json("validate", json!({})).await.unwrap();
        assert_eq!(result["configured"], true);
        assert_eq!(result["passed"], true);
        assert_eq!(result["exit_code"], 0);
    }

    // ── file-tool error paths ─────────────────────────────────────────

    #[tokio::test]
    async fn edit_file_rejects_empty_old_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello\n").unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();
        let err = runtime
            .invoke_json("edit_file", json!({"path": "f.txt", "old": "", "new": "x"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("old must not be empty"));
    }

    #[tokio::test]
    async fn edit_file_rejects_text_not_found() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello\n").unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();
        let err = runtime
            .invoke_json(
                "edit_file",
                json!({"path": "f.txt", "old": "zzz", "new": "x"}),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("old text was not found"));
    }

    #[tokio::test]
    async fn apply_patch_rejects_empty_edits() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();
        let err = runtime
            .invoke_json("apply_patch", json!({"edits": []}))
            .await
            .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("at least one edit")
                || err.to_string().to_lowercase().contains("empty")
        );
    }

    fn git_add_commit(dir: &std::path::Path, message: &str) {
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test")
            .output()
            .unwrap();
    }
}
