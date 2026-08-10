//! Coding tool runtime for Liberado's Rust-native agent loop.

mod hashline;
pub mod repo_map;

use std::{
    collections::{HashMap, VecDeque},
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

#[derive(Clone)]
pub struct CodingToolRuntime {
    workspace: Arc<dyn Workspace>,
    path_policy: PathPolicy,
    validation_command: Option<CommandRequest>,
    hashline: HashlineConfig,
    background_jobs: Arc<Mutex<HashMap<String, BackgroundJob>>>,
}

struct BackgroundJob {
    receiver: oneshot::Receiver<Result<CommandOutput, SandboxError>>,
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

    /// Like [`from_sandbox`], but uses `session_id` as the worktree directory name when
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
            SandboxSpec::HostLocal => Self::new(root, command_policy, path_policy),
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

    pub fn from_workspace(workspace: impl Workspace + 'static, path_policy: PathPolicy) -> Self {
        Self {
            workspace: Arc::new(workspace),
            path_policy,
            validation_command: None,
            hashline: HashlineConfig::default(),
            background_jobs: Arc::new(Mutex::new(HashMap::new())),
        }
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

    pub fn hashline(&self) -> &HashlineConfig {
        &self.hashline
    }

    pub fn workspace_root(&self) -> &Path {
        self.workspace.root()
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
        Ok(self.workspace.resolve_path(rel_path)?)
    }

    async fn invoke_json(&self, name: &str, args: Value) -> Result<Value, ToolError> {
        match name {
            "list_files" => self.list_files(args).await,
            "search_text" => self.search_text(args).await,
            "list_symbols" => self.list_symbols(args).await,
            "read_file" => self.read_file(args).await,
            "write_file" => self.write_file(args).await,
            "edit_file" => self.edit_file(args).await,
            "apply_patch" => self.apply_patch(args).await,
            "hashline_edit" => self.hashline_edit(args).await,
            "git_status" => self.git_status().await,
            "git_diff" => self.git_diff(args).await,
            "git_branch" => self.git_branch(args).await,
            "git_commit" => self.git_commit(args).await,
            "git_push" => self.git_push(args).await,
            "git_log" => self.git_log(args).await,
            "git_fetch" => self.git_fetch(args).await,
            "git_merge" => self.git_merge(args).await,
            "run_command_background" => self.run_command_background(args).await,
            "check_background" => self.check_background(args).await,
            "run_command" => self.run_command(args).await,
            "validate" => self.validate().await,
            other => Err(ToolError::BadRequest(format!("unknown tool: {other}"))),
        }
    }

    async fn list_files(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            #[serde(default = "default_limit")]
            limit: usize,
        }
        let args: Args = parse_args(args)?;
        let mut files = Vec::new();
        walk_files(
            self.workspace.root(),
            args.limit,
            &self.path_policy,
            |_, rel| {
                files.push(rel.to_string());
                true
            },
        )?;
        Ok(json!({ "files": files, "limit": args.limit }))
    }

    async fn search_text(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            query: String,
            #[serde(default = "default_limit")]
            limit: usize,
        }
        let args: Args = parse_args(args)?;
        if args.query.is_empty() {
            return Err(ToolError::BadRequest("query must not be empty".to_string()));
        }

        let mut matches = Vec::new();
        walk_files(
            self.workspace.root(),
            self.path_policy.search_max_results,
            &self.path_policy,
            |path, rel| {
                if matches.len() >= args.limit {
                    return false;
                }
                let Ok(content) = std::fs::read_to_string(path) else {
                    return true;
                };
                for (idx, line) in content.lines().enumerate() {
                    if line.contains(&args.query) {
                        matches.push(json!({
                            "path": rel,
                            "line": idx + 1,
                            "text": line,
                        }));
                        if matches.len() >= args.limit {
                            break;
                        }
                    }
                }
                true
            },
        )?;
        Ok(json!({ "matches": matches }))
    }

    async fn list_symbols(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            #[serde(default = "default_limit")]
            limit: usize,
        }
        let args: Args = parse_args(args)?;
        let mut files = Vec::new();
        // `limit` bounds files *returned*; `SYMBOL_SCAN_MAX_FILES` bounds files looked at. A tree
        // is mostly not source, so spending the same budget on both is what made this return one
        // file — a stray `validate.py` — against this repo: the 200 slots went to markdown and
        // manifests before the walk reached `crates/`.
        walk_files(
            self.workspace.root(),
            SYMBOL_SCAN_MAX_FILES,
            &self.path_policy,
            |path, rel| {
                if files.len() >= args.limit {
                    return false;
                }
                // Decide from the extension before touching the file. Reading first loads every
                // asset, lockfile and binary in the tree whole and then discards it — and
                // `read_to_string` only fails on a binary *after* it has read all of it.
                if lang_from_path(rel).is_empty() {
                    return true;
                }
                let Ok(bytes) = std::fs::read(path) else {
                    return true;
                };
                let capped = cap_bytes(bytes, self.path_policy.read_max_bytes);
                let content = String::from_utf8_lossy(&capped);
                let symbols = extract_symbols(rel, &content);
                if !symbols.is_empty() {
                    files.push(json!({
                        "path": rel,
                        "symbols": symbols,
                    }));
                }
                true
            },
        )?;
        Ok(json!({ "files": files, "limit": args.limit }))
    }

    async fn read_file(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            path: String,
            #[serde(default)]
            start_line: Option<usize>,
            #[serde(default)]
            line_count: Option<usize>,
        }
        let args: Args = parse_args(args)?;
        let path = self.rel_path(&args.path, false)?;
        let bytes = std::fs::read(&path).map_err(fs_err)?;
        let capped = cap_bytes(bytes, self.path_policy.read_max_bytes);
        let full = String::from_utf8_lossy(&capped).into_owned();
        let start = args.start_line.unwrap_or(1).max(1);
        let content = slice_lines(&full, args.start_line, args.line_count);

        if self.hashline.enabled {
            // Hash is always over the full file so tags stay stable across partial reads.
            let tag = hashline::compute_file_hash(&full, self.hashline.hash_length);
            let mut formatted = hashline::format_header(&args.path, &tag);
            formatted.push('\n');
            for (i, line) in content.split('\n').enumerate() {
                let line = line.strip_suffix('\r').unwrap_or(line);
                if i > 0 {
                    formatted.push('\n');
                }
                formatted.push_str(&format!("{}:{line}", start + i));
            }
            if content.ends_with('\n') {
                formatted.push('\n');
            }
            return Ok(json!({
                "path": args.path,
                "content": formatted,
                "hashline": true,
                "tag": tag,
                "start_line": start,
            }));
        }

        Ok(json!({ "path": args.path, "content": content }))
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
        }
        let args: Args = parse_args(args)?;
        let path = self.rel_path(&args.path, true)?;

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
                return Err(ToolError::BadRequest(format!(
                    "{} already exists with {existing_lines} lines and write_file would replace                      all of it. To change part of a file use edit_file, hashline_edit or                      apply_patch. To replace it deliberately, pass \"overwrite\": true.",
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

    async fn edit_file(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            path: String,
            old: String,
            new: String,
        }
        let args: Args = parse_args(args)?;
        if args.old.is_empty() {
            return Err(ToolError::BadRequest("old must not be empty".to_string()));
        }
        let path = self.rel_path(&args.path, true)?;
        let content = std::fs::read_to_string(&path).map_err(fs_err)?;
        let count = content.matches(&args.old).count();
        if count == 0 {
            return Err(ToolError::BadRequest("old text was not found".to_string()));
        }
        if count > 1 {
            return Err(ToolError::BadRequest(format!(
                "old text matched {count} times; provide more context"
            )));
        }
        let updated = content.replacen(&args.old, &args.new, 1);
        std::fs::write(&path, updated.as_bytes()).map_err(fs_err)?;
        Ok(json!({ "path": args.path, "replacements": 1 }))
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
        let mut request = CommandRequest::new("git");
        request.args = vec!["status".to_string(), "--porcelain".to_string()];
        let output = self.workspace.run_command(request).await?;
        Ok(json!({
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "timed_out": output.timed_out,
        }))
    }

    async fn git_diff(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            #[serde(default = "default_diff_mode")]
            mode: String,
        }
        let args: Args = parse_args(args)?;
        let mut request = CommandRequest::new("git");
        request.args = match args.mode.as_str() {
            "name_only" => vec!["diff".to_string(), "--name-only".to_string()],
            "stat" => vec!["diff".to_string(), "--stat".to_string()],
            "patch" => vec!["diff".to_string()],
            other => {
                return Err(ToolError::BadRequest(format!(
                    "unsupported diff mode: {other}"
                )));
            }
        };
        let output = self.workspace.run_command(request).await?;
        Ok(json!({
            "mode": args.mode,
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "timed_out": output.timed_out,
        }))
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
        let mut request = CommandRequest::new("git");
        request.args = vec!["checkout".to_string(), "-b".to_string(), args.name];
        let output = self.workspace.run_command(request).await?;
        Ok(json!({
            "branch": branch_name,
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "timed_out": output.timed_out,
        }))
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

        let mut stage = CommandRequest::new("git");
        if args.files.is_empty() {
            stage.args = vec!["add".to_string(), "-A".to_string()];
        } else {
            stage.args = vec!["add".to_string(), "--".to_string()];
            stage.args.extend(args.files.clone());
        }
        let stage_output = self.workspace.run_command(stage).await?;
        if stage_output.exit_code != Some(0) {
            return Ok(json!({
                "committed": false,
                "stage_exit_code": stage_output.exit_code,
                "stage_stderr": stage_output.stderr,
                "exit_code": null,
                "stdout": "",
                "stderr": "",
                "timed_out": stage_output.timed_out,
            }));
        }

        let mut request = CommandRequest::new("git");
        request.args = vec!["commit".to_string(), "-m".to_string(), args.message];
        request
            .env
            .insert("GIT_AUTHOR_NAME".to_string(), "liberado".to_string());
        request
            .env
            .insert("GIT_AUTHOR_EMAIL".to_string(), "liberado@local".to_string());
        request
            .env
            .insert("GIT_COMMITTER_NAME".to_string(), "liberado".to_string());
        request.env.insert(
            "GIT_COMMITTER_EMAIL".to_string(),
            "liberado@local".to_string(),
        );
        let output = self.workspace.run_command(request).await?;
        Ok(json!({
            "committed": output.exit_code == Some(0),
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "timed_out": output.timed_out,
        }))
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
        let mut request = CommandRequest::new("git");
        request.args = vec!["push".to_string()];
        if args.set_upstream {
            request.args.push("--set-upstream".to_string());
        }
        request.args.push(args.remote);
        if let Some(branch) = args.branch {
            request.args.push(branch);
        }
        let output = self.workspace.run_command(request).await?;
        Ok(json!({
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "timed_out": output.timed_out,
        }))
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
        let mut request = CommandRequest::new("git");
        let fmt = args.format.unwrap_or_else(|| "%h %s".to_string());
        request.args = vec![
            "log".to_string(),
            format!("--max-count={limit}"),
            format!("--format={fmt}"),
        ];
        if let Some(ref branch) = args.branch
            && !branch.is_empty()
        {
            request.args.push(branch.clone());
        }
        let output = self.workspace.run_command(request).await?;
        Ok(json!({
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "timed_out": output.timed_out,
        }))
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
        let mut request = CommandRequest::new("git");
        request.args = vec!["fetch".to_string(), args.remote];
        if let Some(ref branch) = args.branch
            && !branch.is_empty()
        {
            request.args.push(branch.clone());
        }
        let output = self.workspace.run_command(request).await?;
        Ok(json!({
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "timed_out": output.timed_out,
        }))
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
        let mut request = CommandRequest::new("git");
        request.args = vec!["merge".to_string()];
        if args.fast_forward_only {
            request.args.push("--ff-only".to_string());
        }
        request.args.push(args.branch);
        let output = self.workspace.run_command(request).await?;
        Ok(json!({
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "timed_out": output.timed_out,
        }))
    }

    async fn run_command_background(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            program: String,
            #[serde(default)]
            args: Vec<String>,
        }
        let args: Args = parse_args(args)?;
        let mut request = CommandRequest::new(args.program.clone());
        request.args = args.args;

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

        let mut jobs = self
            .background_jobs
            .lock()
            .map_err(|e| ToolError::BadRequest(format!("background job lock: {e}")))?;
        jobs.insert(job_id.clone(), BackgroundJob { receiver: rx });

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
            Ok(Ok(output)) => Ok(json!({
                "job_id": jid,
                "status": "completed",
                "exit_code": output.exit_code,
                "stdout": output.stdout,
                "stderr": output.stderr,
                "timed_out": output.timed_out,
            })),
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
        // Dogfood finding #5: refuse `gh pr create --base X` when origin has no X, so we never
        // silently land a PR on main with a multi-PR stack.
        if let Some(err) = preflight_gh_pr_create(&args.program, &args.args) {
            return Err(ToolError::BadRequest(err));
        }
        let mut request = CommandRequest::new(args.program);
        request.args = args.args;
        let output = self.workspace.run_command(request).await?;
        Ok(json!({
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "timed_out": output.timed_out,
        }))
    }

    async fn validate(&self) -> Result<Value, ToolError> {
        let Some(command) = self.validation_command.clone() else {
            return Ok(json!({ "configured": false, "passed": null }));
        };
        let output = self.workspace.run_command(command).await?;
        Ok(json!({
            "configured": true,
            "passed": output.exit_code == Some(0) && !output.timed_out,
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "timed_out": output.timed_out,
        }))
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
                "search_text",
                "Search workspace files for exact text.",
                json!({
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "query": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1 }
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
                "Create a new file, or replace one whole file. To change part of an existing                  file use edit_file, hashline_edit or apply_patch instead — write_file replaces                  the entire contents.",
                json!({
                    "type": "object",
                    "required": ["path", "content"],
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" },
                        "overwrite": {
                            "type": "boolean",
                            "description": "Required to replace a file that already has content.                                             Omit it when creating a new file."
                        }
                    }
                }),
            ),
            tool(
                "edit_file",
                "Replace one exact text span in a file.",
                json!({
                    "type": "object",
                    "required": ["path", "old", "new"],
                    "properties": {
                        "path": { "type": "string" },
                        "old": { "type": "string" },
                        "new": { "type": "string" }
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
        let full: Vec<ToolDef> = if self.hashline.enabled {
            full
        } else {
            full.into_iter()
                .filter(|t| t.name != "hashline_edit")
                .collect()
        };
        if self.path_policy.writes_disabled() {
            full.into_iter()
                .filter(|t| EXPLORE_TOOL_NAMES.contains(&t.name.as_str()))
                .collect()
        } else {
            full
        }
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        // Defense in depth: even if a write tool is named, read-only policy refuses it.
        if self.path_policy.writes_disabled() && !EXPLORE_TOOL_NAMES.contains(&call.name.as_str()) {
            return Err(format!(
                "tool '{}' is not available in explore (read-only) mode",
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

fn parse_args<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|e| ToolError::BadRequest(e.to_string()))
}

fn default_limit() -> usize {
    200
}

/// How many files `list_symbols` will look at while trying to fill its result limit.
///
/// The walk still has to terminate on a repository far larger than any limit could describe, but
/// this is a scan bound rather than a result bound — the two are not the same number.
const SYMBOL_SCAN_MAX_FILES: usize = 20_000;

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

fn cap_bytes(mut bytes: Vec<u8>, max: usize) -> Vec<u8> {
    if bytes.len() > max {
        bytes.truncate(max);
    }
    bytes
}

fn slice_lines(content: &str, start_line: Option<usize>, line_count: Option<usize>) -> String {
    let start = start_line.unwrap_or(1).saturating_sub(1);
    let count = line_count.unwrap_or(usize::MAX);
    content
        .lines()
        .skip(start)
        .take(count)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Walk `root` breadth-first, visiting up to `limit` files the policy does not deny.
///
/// The deny list prunes **directories** as well as files, and a denied entry does not spend a
/// slot. Filtering at the visitor instead — after the walk has already counted the file — means
/// the budget goes to paths the caller was never allowed to read. `PathPolicy::default()` denies
/// `.git/**`, `target/**` and `node_modules/**`, which between them are almost every file in a
/// working checkout: run against this repo, `list_symbols` at the default limit of 200 came back
/// with one file, and it was a stray Python script.
/// `visit` returns whether to keep walking — `false` stops immediately. A caller whose limit is on
/// what it *collects* rather than what it *reads* needs that: `list_symbols` has to look past the
/// markdown and lockfiles to find any source at all, so its `limit` cannot also be the walk budget.
fn walk_files(
    root: &Path,
    limit: usize,
    policy: &PathPolicy,
    mut visit: impl FnMut(&Path, &str) -> bool,
) -> Result<(), ToolError> {
    let mut queue = VecDeque::from([root.to_path_buf()]);
    let mut visited = 0usize;
    while let Some(dir) = queue.pop_front() {
        for entry in std::fs::read_dir(&dir).map_err(fs_err)? {
            let entry = entry.map_err(fs_err)?;
            let path = entry.path();
            let rel = relative_string(root, &path);
            if path_denied(&rel, policy) {
                continue;
            }
            if path.is_dir() {
                queue.push_back(path);
            } else if path.is_file() {
                if !visit(&path, &rel) {
                    return Ok(());
                }
                visited += 1;
                if visited >= limit {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

fn relative_string(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn lang_from_path(path: &str) -> &str {
    let lower = path.to_lowercase();
    if lower.ends_with(".rs") {
        "rust"
    } else if lower.ends_with(".py") {
        "python"
    } else if lower.ends_with(".ts")
        || lower.ends_with(".tsx")
        || lower.ends_with(".js")
        || lower.ends_with(".jsx")
    {
        "typescript"
    } else if lower.ends_with(".go") {
        "go"
    } else if lower.ends_with(".java") {
        "java"
    } else {
        ""
    }
}

fn extract_symbols(path: &str, content: &str) -> Vec<String> {
    let lang = lang_from_path(path);
    if lang.is_empty() {
        return Vec::new();
    }
    let mut symbols = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        let sym = match lang {
            "rust" => extract_rust_symbol(trimmed),
            // Python alone gets the raw line: indentation *is* its nesting, so a trimmed line
            // cannot tell a module-level `def` from a method. Passing `trimmed` here made the
            // column-0 guard in `extract_python_symbol` a check that could not fire.
            "python" => extract_python_symbol(line),
            "typescript" => extract_ts_symbol(trimmed),
            "go" => extract_go_symbol(trimmed),
            "java" => extract_java_symbol(trimmed),
            _ => None,
        };
        if let Some(s) = sym {
            if symbols.len() >= 50 {
                break;
            }
            symbols.push(s);
        }
    }
    symbols
}

fn extract_rust_symbol(line: &str) -> Option<String> {
    let line = line.trim();
    if line.starts_with("//")
        || line.starts_with("///")
        || line.starts_with("//!")
        || line.starts_with("/*")
        || line.starts_with("* ")
        || line == "*"
    {
        return None;
    }
    // Strip #[derive(...)] and other same-line attributes.
    let line = if line.starts_with("#[") {
        line.split(']').nth(1).unwrap_or("").trim()
    } else {
        line
    };
    if line.is_empty() {
        return None;
    }
    // Strip visibility and qualifier prefixes, leaving just the keyword prefix.
    // Handles: pub, pub(crate), pub(super), pub(in path), const, unsafe, async, extern "C"
    let rest = line
        .trim_start_matches("pub(crate) ")
        .trim_start_matches("pub(super) ")
        .trim_start_matches("pub ")
        .trim_start_matches("const ")
        .trim_start_matches("unsafe ")
        .trim_start_matches("async ")
        .trim_start_matches("extern \"C\" ");
    // fn
    if rest.starts_with("fn ") {
        let name = rest
            .trim_start_matches("fn ")
            .split(['(', '<'])
            .next()?
            .trim()
            .to_string();
        if !name.is_empty() {
            return Some(format!("fn {name}"));
        }
    }
    // struct
    if rest.starts_with("struct ") {
        let name = rest
            .trim_start_matches("struct ")
            .split(['<', '{', '(', ';'])
            .next()?
            .trim()
            .to_string();
        if !name.is_empty() {
            return Some(format!("struct {name}"));
        }
    }
    // enum
    if rest.starts_with("enum ") {
        let name = rest
            .trim_start_matches("enum ")
            .split(['<', '{'])
            .next()?
            .trim()
            .to_string();
        if !name.is_empty() {
            return Some(format!("enum {name}"));
        }
    }
    // trait
    if rest.starts_with("trait ") {
        let name = rest
            .trim_start_matches("trait ")
            .split(['<', '{'])
            .next()?
            .trim()
            .to_string();
        if !name.is_empty() {
            return Some(format!("trait {name}"));
        }
    }
    // impl
    if rest.starts_with("impl<") || rest.starts_with("impl ") {
        let rest = rest.trim_start_matches("impl").trim_start_matches(' ');
        let name = if rest.starts_with('<') {
            rest.split('>')
                .nth(1)
                .and_then(|s| s.trim().split(' ').next())?
                .to_string()
        } else {
            rest.split(['<', '{', ' ']).next()?.trim().to_string()
        };
        if !name.is_empty() && !name.starts_with("for") {
            return Some(format!("impl {name}"));
        }
    }
    // mod
    if rest.starts_with("mod ") {
        let name = rest
            .trim_start_matches("mod ")
            .split(['{', ';'])
            .next()?
            .trim()
            .to_string();
        if !name.is_empty() {
            return Some(format!("mod {name}"));
        }
    }
    None
}

fn extract_python_symbol(line: &str) -> Option<String> {
    if line.starts_with(' ') || line.starts_with('\t') {
        return None;
    }
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return None;
    }
    if trimmed.starts_with("def ") {
        let name = trimmed
            .trim_start_matches("def ")
            .split('(')
            .next()?
            .trim()
            .to_string();
        if !name.is_empty() && !name.starts_with('_') {
            return Some(format!("def {name}"));
        }
    }
    if trimmed.starts_with("class ") {
        let name = trimmed
            .trim_start_matches("class ")
            .split(['(', ':'])
            .next()?
            .trim()
            .to_string();
        if !name.is_empty() {
            return Some(format!("class {name}"));
        }
    }
    None
}

fn extract_ts_symbol(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
        return None;
    }
    if trimmed.starts_with("function ") {
        let name = trimmed
            .trim_start_matches("function ")
            .split('(')
            .next()?
            .trim()
            .to_string();
        if !name.is_empty() {
            return Some(format!("function {name}"));
        }
    }
    if trimmed.starts_with("export function ")
        || trimmed.starts_with("export default function ")
        || trimmed.starts_with("export default async function ")
    {
        let name = trimmed
            .trim_start_matches("export default async ")
            .trim_start_matches("export default ")
            .trim_start_matches("export ")
            .trim_start_matches("function ")
            .split('(')
            .next()?
            .trim()
            .to_string();
        if !name.is_empty() {
            return Some(format!("export function {name}"));
        }
    }
    if trimmed.contains(" class ") || trimmed.starts_with("class ") {
        let rest = if trimmed.starts_with("export ") {
            trimmed
                .trim_start_matches("export ")
                .trim_start_matches("default ")
                .trim_start_matches("abstract ")
        } else {
            trimmed.trim_start_matches("abstract ")
        };
        if rest.starts_with("class ") {
            let name = rest
                .trim_start_matches("class ")
                .split(['<', '{', ' ', ':'])
                .next()?
                .trim()
                .to_string();
            if !name.is_empty() && name != "extends" && name != "implements" {
                return Some(format!("class {name}"));
            }
        }
    }
    if trimmed.starts_with("interface ") || trimmed.starts_with("export interface ") {
        let name = trimmed
            .trim_start_matches("export ")
            .trim_start_matches("interface ")
            .split(['<', '{'])
            .next()?
            .trim()
            .to_string();
        if !name.is_empty() {
            return Some(format!("interface {name}"));
        }
    }
    if trimmed.starts_with("export const ") || trimmed.starts_with("const ") {
        let rest = trimmed.trim_start_matches("export ");
        if rest.starts_with("const ") {
            let name = rest
                .trim_start_matches("const ")
                .split([':', '='])
                .next()?
                .trim()
                .to_string();
            if !name.is_empty() && rest.contains("=>") {
                return Some(format!("const {name}"));
            }
        }
    }
    None
}

fn extract_go_symbol(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
        return None;
    }
    if trimmed.starts_with("func ") {
        let name = trimmed
            .trim_start_matches("func ")
            .split('(')
            .next()?
            .trim()
            .to_string();
        if !name.is_empty() {
            return Some(format!("func {name}"));
        }
    }
    if trimmed.starts_with("type ") && trimmed.contains(" struct") {
        let name = trimmed
            .trim_start_matches("type ")
            .split_whitespace()
            .next()?
            .to_string();
        if !name.is_empty() {
            return Some(format!("type {name} struct"));
        }
    }
    if trimmed.starts_with("type ") && trimmed.contains(" interface") {
        let name = trimmed
            .trim_start_matches("type ")
            .split_whitespace()
            .next()?
            .to_string();
        if !name.is_empty() {
            return Some(format!("type {name} interface"));
        }
    }
    None
}

fn extract_java_symbol(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
        return None;
    }
    if trimmed.starts_with("public class ") || trimmed.starts_with("class ") {
        let name = trimmed
            .trim_start_matches("public ")
            .trim_start_matches("class ")
            .split(['<', '{'])
            .next()?
            .trim()
            .to_string();
        if !name.is_empty() {
            return Some(format!("class {name}"));
        }
    }
    if trimmed.starts_with("public interface ") || trimmed.starts_with("interface ") {
        let name = trimmed
            .trim_start_matches("public ")
            .trim_start_matches("interface ")
            .split(['<', '{'])
            .next()?
            .trim()
            .to_string();
        if !name.is_empty() {
            return Some(format!("interface {name}"));
        }
    }
    if trimmed.starts_with("public enum ") || trimmed.starts_with("enum ") {
        let name = trimmed
            .trim_start_matches("public ")
            .trim_start_matches("enum ")
            .split(['<', '{'])
            .next()?
            .trim()
            .to_string();
        if !name.is_empty() {
            return Some(format!("enum {name}"));
        }
    }
    None
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

    #[tokio::test]
    async fn read_file_caps_large_content() {
        let (dir, mut runtime) = runtime();
        runtime.path_policy.read_max_bytes = 4;
        std::fs::write(dir.path().join("big.txt"), "abcdef").unwrap();

        let result = runtime
            .invoke_json("read_file", json!({"path": "big.txt"}))
            .await
            .unwrap();

        assert_eq!(result["content"], "abcd");
    }

    #[tokio::test]
    async fn denied_path_is_not_read() {
        let (dir, runtime) = runtime();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/config"), "secret").unwrap();
        let err = runtime
            .invoke_json("read_file", json!({"path": ".git/config"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::PathDenied(_)));
    }

    #[tokio::test]
    async fn search_text_returns_line_matches() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("notes.txt"), "alpha\nbeta\n").unwrap();
        let result = runtime
            .invoke_json("search_text", json!({"query": "beta"}))
            .await
            .unwrap();
        assert_eq!(result["matches"][0]["path"], "notes.txt");
        assert_eq!(result["matches"][0]["line"], 2);
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
    async fn list_files_returns_workspace_contents() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();

        let result = runtime.invoke_json("list_files", json!({})).await.unwrap();

        let files = result["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert!(files.contains(&"a.txt".to_string()), "should contain a.txt");
        assert!(files.contains(&"b.txt".to_string()), "should contain b.txt");
    }

    #[tokio::test]
    async fn list_files_respects_limit() {
        let (dir, runtime) = runtime();
        for i in 0..10 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "x\n").unwrap();
        }

        let result = runtime
            .invoke_json("list_files", json!({"limit": 3}))
            .await
            .unwrap();

        assert_eq!(result["limit"], 3);
        let files = result["files"].as_array().unwrap();
        assert!(files.len() <= 3, "limit 3 should cap results");
    }

    #[tokio::test]
    async fn search_text_respects_limit_and_multi_match_file() {
        let (dir, runtime) = runtime();
        std::fs::write(dir.path().join("notes.txt"), "alpha\nalpha\nbeta\n").unwrap();

        let result = runtime
            .invoke_json("search_text", json!({"query": "alpha", "limit": 1}))
            .await
            .unwrap();

        assert_eq!(result["matches"].as_array().unwrap().len(), 1);
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

    #[tokio::test]
    async fn catalog_contains_expected_tools() {
        let (_dir, runtime) = runtime();
        let catalog = runtime.catalog();
        let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();
        for tool in &[
            "list_files",
            "search_text",
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

    #[tokio::test]
    async fn walk_files_respects_limit() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "x\n").unwrap();
        }

        let mut count = 0usize;
        walk_files(dir.path(), 3, &PathPolicy::default(), |_, _| {
            count += 1;
            true
        })
        .unwrap();

        assert_eq!(count, 3, "walk_files should visit at most 3 files");
    }

    #[tokio::test]
    async fn a_denied_directory_neither_counts_against_the_limit_nor_is_descended_into() {
        // The budget is what makes this matter. `.git` alone is thousands of files in a real
        // checkout, so a walk that counts them first and filters them second returns nothing the
        // caller can use — which is what `list_symbols` did against this repo.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        for i in 0..20 {
            std::fs::write(dir.path().join(".git").join(format!("o{i}")), "x\n").unwrap();
        }
        std::fs::write(dir.path().join("real.rs"), "pub fn kept() {}\n").unwrap();

        let mut seen = Vec::new();
        walk_files(dir.path(), 5, &PathPolicy::default(), |_, rel| {
            seen.push(rel.to_string());
            true
        })
        .unwrap();

        assert_eq!(
            seen,
            vec!["real.rs".to_string()],
            "a denied directory must not spend the caller's budget"
        );
    }

    #[test]
    fn extract_rust_visibility_variants() {
        let content = "pub(crate) fn internal() {}\npub(super) fn semi_public() {}\npub fn public() {}\nfn private() {}";
        let symbols = extract_symbols("src/lib.rs", content);
        assert!(symbols.contains(&"fn internal".to_string()));
        assert!(symbols.contains(&"fn semi_public".to_string()));
        assert!(symbols.contains(&"fn public".to_string()));
        assert!(symbols.contains(&"fn private".to_string()));
    }

    #[test]
    fn extract_rust_const_and_unsafe_fn() {
        let content = "const fn compile_time() -> u32 { 42 }\npub const fn pub_compile() {}\nunsafe fn raw_op() {}\npub unsafe fn pub_raw() {}";
        let symbols = extract_symbols("src/lib.rs", content);
        assert!(symbols.contains(&"fn compile_time".to_string()));
        assert!(symbols.contains(&"fn pub_compile".to_string()));
        assert!(symbols.contains(&"fn raw_op".to_string()));
        assert!(symbols.contains(&"fn pub_raw".to_string()));
    }

    #[test]
    fn extract_rust_attribute_prefixed() {
        let content = "#[derive(Debug)]\npub struct Foo;\n\n#[inline]\npub fn bar() {}";
        let symbols = extract_symbols("src/lib.rs", content);
        assert!(symbols.contains(&"struct Foo".to_string()));
        assert!(symbols.contains(&"fn bar".to_string()));
    }

    #[test]
    fn extract_rust_generic_impl() {
        let content = "impl<T> MyType<T> {\n    fn new() -> Self {}\n}\n\nimpl PlainType {\n    fn method() {}\n}";
        let symbols = extract_symbols("src/lib.rs", content);
        assert!(
            symbols.iter().any(|s| s.contains("MyType")),
            "should find generic impl"
        );
        assert!(symbols.contains(&"impl PlainType".to_string()));
    }

    #[test]
    fn extract_ts_export_default_function() {
        let content = "export default function main() {}\nexport function helper() {}";
        let symbols = extract_symbols("app.ts", content);
        assert!(symbols.contains(&"export function main".to_string()));
        assert!(symbols.contains(&"export function helper".to_string()));
    }

    #[test]
    fn extract_rust_traits_and_enums() {
        let content = "pub trait Handler {\n    fn handle(&self);\n}\n\npub enum Status {\n    Ok,\n    Err,\n}";
        let symbols = extract_symbols("src/types.rs", content);
        assert!(symbols.contains(&"trait Handler".to_string()));
        assert!(symbols.contains(&"enum Status".to_string()));
    }

    #[test]
    fn extract_rust_mods_and_impls() {
        let content =
            "pub mod sub;\nmod private_mod {\n}\n\nimpl MyType {\n    fn new() -> Self {}\n}";
        let symbols = extract_symbols("src/lib.rs", content);
        assert!(symbols.contains(&"mod sub".to_string()));
        assert!(symbols.contains(&"mod private_mod".to_string()));
        assert!(symbols.contains(&"impl MyType".to_string()));
    }

    #[test]
    fn extract_python_symbols() {
        let content = "def handle_request(req):\n    pass\n\nclass Router:\n    def route(self):\n        pass";
        let symbols = extract_symbols("app.py", content);
        assert!(symbols.contains(&"def handle_request".to_string()));
        assert!(symbols.contains(&"class Router".to_string()));
        // Indentation is Python's nesting. `route` is a method on `Router`; listing it beside the
        // module-level names says the module exports something it does not. The guard for this
        // was already written — it just never saw an indented line to reject.
        assert!(
            !symbols.contains(&"def route".to_string()),
            "a nested def is not a top-level symbol: {symbols:?}"
        );
    }

    #[test]
    fn extract_typescript_symbols() {
        let content = "export function main() {}\n\nclass App {\n    render() {}\n}\n\ninterface Config {\n    port: number;\n}\n\nconst handler = (req: Request) => { return 200; };";
        let symbols = extract_symbols("app.ts", content);
        assert!(symbols.contains(&"export function main".to_string()));
        assert!(symbols.contains(&"class App".to_string()));
        assert!(symbols.contains(&"interface Config".to_string()));
        assert!(symbols.contains(&"const handler".to_string()));
    }

    #[test]
    fn extract_go_symbols() {
        let content = "func main() {\n}\n\ntype Server struct {\n    port int\n}\n\ntype Handler interface {\n    Serve()\n}";
        let symbols = extract_symbols("main.go", content);
        assert!(symbols.contains(&"func main".to_string()));
        assert!(symbols.contains(&"type Server struct".to_string()));
        assert!(symbols.contains(&"type Handler interface".to_string()));
    }

    #[test]
    fn extract_java_symbols() {
        let content = "public class Main {\n    public static void main(String[] args) {}\n}\n\npublic enum State {\n    ON, OFF\n}\n\npublic interface Runnable {\n    void run();\n}";
        let symbols = extract_symbols("Main.java", content);
        assert!(symbols.contains(&"class Main".to_string()));
        assert!(symbols.contains(&"enum State".to_string()));
        assert!(symbols.contains(&"interface Runnable".to_string()));
    }

    #[test]
    fn unknown_extension_returns_empty() {
        let content = "fn main() {}";
        let symbols = extract_symbols("README.md", content);
        assert!(symbols.is_empty());
    }

    #[test]
    fn skips_comments() {
        let content = "// fn not_a_fn() {}\n/* struct Fake {} */\n\nfn real() {}";
        let symbols = extract_symbols("src/lib.rs", content);
        assert_eq!(symbols.len(), 1);
        assert!(symbols.contains(&"fn real".to_string()));
    }

    #[tokio::test]
    async fn list_symbols_returns_workspace_symbols() {
        let (dir, runtime) = runtime();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn answer() -> u8 { 42 }\n\npub struct Config { pub port: u16 }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("README.md"),
            "# Project\nfn not_extracted() {}\n",
        )
        .unwrap();

        let result = runtime
            .invoke_json("list_symbols", json!({}))
            .await
            .unwrap();
        let files = result["files"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["path"], "src/lib.rs");
        let symbols: Vec<String> = files[0]["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect();
        assert!(symbols.contains(&"fn answer".to_string()));
        assert!(symbols.contains(&"struct Config".to_string()));
    }

    #[tokio::test]
    async fn list_symbols_respects_limit() {
        let (dir, runtime) = runtime();
        for i in 0..5 {
            std::fs::write(
                dir.path().join(format!("f{i}.rs")),
                format!("fn f{i}() {{}}"),
            )
            .unwrap();
        }

        let result = runtime
            .invoke_json("list_symbols", json!({"limit": 2}))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        assert!(files.len() <= 2, "limit 2 should cap results");
        assert_eq!(result["limit"], 2);
    }

    fn init_temp_git_repo(dir: &std::path::Path) {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@test")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@test")
                .output()
                .unwrap()
        };
        run(&["init", "--quiet"]);
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
    #[tokio::test]
    async fn list_symbols_limit_bounds_what_is_returned_not_what_is_walked() {
        // A source tree is mostly not source. When `limit` doubled as the walk budget, the
        // non-source files ahead of the code in breadth-first order consumed it — against this
        // repo the tool returned exactly one file, a stray `validate.py`, and no Rust at all.
        let (dir, runtime) = runtime();
        for i in 0..10 {
            std::fs::write(
                dir.path().join(format!("a{i}.md")),
                "# not source
",
            )
            .unwrap();
        }
        for i in 0..3 {
            std::fs::write(
                dir.path().join(format!("z{i}.rs")),
                format!(
                    "pub fn thing{i}() {{}}
"
                ),
            )
            .unwrap();
        }

        let result = runtime
            .invoke_json("list_symbols", json!({"limit": 2}))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        assert_eq!(
            files.len(),
            2,
            "limit is a result bound, and there are 3 source files past 10 non-source ones: {files:?}"
        );
        for f in files {
            let path = f["path"].as_str().unwrap();
            assert!(
                path.ends_with(".rs"),
                "only source files carry symbols: {path}"
            );
        }
    }

    #[tokio::test]
    async fn list_symbols_stops_reading_a_file_at_the_policy_cap() {
        // `read_file` caps at `read_max_bytes`; this walked every file with an uncapped
        // `read_to_string`, which on a binary reads the whole thing before failing to decode it.
        let dir = tempfile::tempdir().unwrap();
        let policy = PathPolicy {
            read_max_bytes: 64,
            ..PathPolicy::default()
        };
        let runtime = CodingToolRuntime::new(dir.path(), CommandPolicy::default(), policy).unwrap();
        let mut content = String::from(
            "pub fn early() {}
",
        );
        content.push_str(
            &"// padding padding padding
"
            .repeat(20),
        );
        content.push_str(
            "pub fn beyond_the_cap() {}
",
        );
        std::fs::write(dir.path().join("big.rs"), &content).unwrap();

        let result = runtime
            .invoke_json("list_symbols", json!({}))
            .await
            .unwrap();

        let symbols = result["files"][0]["symbols"].as_array().unwrap();
        assert!(
            symbols.iter().any(|s| s == "fn early"),
            "the head of the file is still read: {symbols:?}"
        );
        assert!(
            !symbols.iter().any(|s| s == "fn beyond_the_cap"),
            "nothing past read_max_bytes should be reachable: {symbols:?}"
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

    #[tokio::test]
    async fn search_text_rejects_empty_query() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello\n").unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();
        let err = runtime
            .invoke_json("search_text", json!({"query": ""}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("query must not be empty"));
    }

    // ── Java symbol extraction ────────────────────────────────────────

    #[test]
    fn extract_java_non_public_class_interface_enum() {
        assert_eq!(
            extract_java_symbol("class PackagePrivate { }"),
            Some("class PackagePrivate".to_string())
        );
        assert_eq!(
            extract_java_symbol("interface Contract { }"),
            Some("interface Contract".to_string())
        );
        assert_eq!(
            extract_java_symbol("enum Color { RED, GREEN }"),
            Some("enum Color".to_string())
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
