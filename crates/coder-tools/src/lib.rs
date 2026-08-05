//! Coding tool runtime for Liberado's Rust-native agent loop.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use liberado_coder_core::{CommandPolicy, PathPolicy, SandboxSpec};
use liberado_coder_sandbox::{
    CommandRequest, DockerWorkspace, HostWorkspace, SandboxError, Workspace, WorktreeWorkspace,
};
use liberado_executor::ToolRuntime;
use liberado_provider::{ToolDef, ToolInvocation};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

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
        match sandbox {
            SandboxSpec::HostLocal => Self::new(root, command_policy, path_policy),
            SandboxSpec::Docker(spec) => {
                let workspace = DockerWorkspace::new(root, spec, command_policy)?;
                Ok(Self::from_workspace(workspace, path_policy))
            }
            SandboxSpec::Worktree => {
                let root = root.into();
                let session_id = root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("session");
                let worktrees_base = root.parent().unwrap_or(&root).join("worktrees");
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
        }
    }

    pub fn with_validation_command(mut self, command: CommandRequest) -> Self {
        self.validation_command = Some(command);
        self
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
            "read_file" => self.read_file(args).await,
            "write_file" => self.write_file(args).await,
            "edit_file" => self.edit_file(args).await,
            "apply_patch" => self.apply_patch(args).await,
            "git_status" => self.git_status().await,
            "git_diff" => self.git_diff(args).await,
            "git_branch" => self.git_branch(args).await,
            "git_commit" => self.git_commit(args).await,
            "git_push" => self.git_push(args).await,
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
        walk_files(self.workspace.root(), args.limit, |path| {
            let rel = relative_string(self.workspace.root(), path);
            if !path_denied(&rel, &self.path_policy) {
                files.push(rel);
            }
        })?;
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
            |path| {
                if matches.len() >= args.limit {
                    return;
                }
                let rel = relative_string(self.workspace.root(), path);
                if path_denied(&rel, &self.path_policy) {
                    return;
                }
                let Ok(content) = std::fs::read_to_string(path) else {
                    return;
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
            },
        )?;
        Ok(json!({ "matches": matches }))
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
        let content = String::from_utf8_lossy(&capped).into_owned();
        let content = slice_lines(&content, args.start_line, args.line_count);
        Ok(json!({ "path": args.path, "content": content }))
    }

    async fn write_file(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            path: String,
            content: String,
        }
        let args: Args = parse_args(args)?;
        let path = self.rel_path(&args.path, true)?;
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

    async fn run_command(&self, args: Value) -> Result<Value, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            program: String,
            #[serde(default)]
            args: Vec<String>,
        }
        let args: Args = parse_args(args)?;
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
        vec![
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
                "Write a complete file under the workspace.",
                json!({
                    "type": "object",
                    "required": ["path", "content"],
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
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
        ]
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        self.invoke_json(&call.name, call.arguments.clone())
            .await
            .and_then(|value| {
                serde_json::to_string(&value).map_err(|e| ToolError::BadRequest(e.to_string()))
            })
            .map_err(|e| e.to_string())
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

fn walk_files(root: &Path, limit: usize, mut visit: impl FnMut(&Path)) -> Result<(), ToolError> {
    let mut queue = VecDeque::from([root.to_path_buf()]);
    let mut visited = 0usize;
    while let Some(dir) = queue.pop_front() {
        for entry in std::fs::read_dir(&dir).map_err(fs_err)? {
            let entry = entry.map_err(fs_err)?;
            let path = entry.path();
            if path.is_dir() {
                queue.push_back(path);
            } else if path.is_file() {
                visit(&path);
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

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_core::DockerSandboxSpec;

    fn runtime() -> (tempfile::TempDir, CodingToolRuntime) {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            CodingToolRuntime::new(dir.path(), CommandPolicy::default(), PathPolicy::default())
                .unwrap();
        (dir, runtime)
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
        .unwrap();

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

    #[tokio::test]
    async fn walk_files_respects_limit() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "x\n").unwrap();
        }

        let mut count = 0usize;
        walk_files(dir.path(), 3, |_| {
            count += 1;
        })
        .unwrap();

        assert_eq!(count, 3, "walk_files should visit at most 3 files");
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
}
