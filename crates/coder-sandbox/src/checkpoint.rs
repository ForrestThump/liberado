//! Kilo-style shadow-git checkpoints (coding-tui S4 / G3).
//!
//! A side repo outside the project holds tree snapshots of a workspace. Restore is
//! **files-only** (conversation/transcript untouched). Used for mid-build park/resume
//! and `POST /api/goals/{id}/rewind`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{path_for_cli, strip_extended_path_prefix};

#[derive(Debug, Error)]
pub enum CheckpointError {
    #[error("checkpoint io: {0}")]
    Io(String),
    #[error("checkpoint git: {0}")]
    Git(String),
    #[error("unknown checkpoint: {0}")]
    NotFound(String),
}

/// One durable workspace snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Stable id (commit SHA in the shadow repo).
    pub id: String,
    pub label: String,
    /// Tree hash (git write-tree).
    pub tree_hash: String,
}

/// Side-repo checkpoint store for one workspace + session.
#[derive(Debug, Clone)]
pub struct ShadowGit {
    git_dir: PathBuf,
    work_tree: PathBuf,
}

impl ShadowGit {
    /// Side repo at `<data>/checkpoints/<session_id>/` (or `LIBERADO_DATA_DIR`).
    pub fn open_or_init(workspace_root: &Path, session_id: &str) -> Result<Self, CheckpointError> {
        let data = std::env::var("LIBERADO_DATA_DIR").unwrap_or_else(|_| ".liberado".into());
        Self::open_or_init_at(Path::new(&data), workspace_root, session_id)
    }

    /// `open_or_init` with the data root supplied instead of read from the environment.
    ///
    /// The env var is process-global, so tests that set it must serialize against every other
    /// test in the binary — a lock that has to be held across `await` points and therefore gets
    /// dropped early, which is how it leaks. Taking the directory as an argument removes the
    /// shared mutable state instead of guarding it.
    fn open_or_init_at(
        data: &Path,
        workspace_root: &Path,
        session_id: &str,
    ) -> Result<Self, CheckpointError> {
        if session_id.is_empty()
            || session_id.contains("..")
            || session_id.contains('/')
            || session_id.contains('\\')
        {
            return Err(CheckpointError::Io(format!(
                "session id '{session_id}' is not a safe checkpoint directory name"
            )));
        }
        let work_tree = strip_extended_path_prefix(
            &workspace_root
                .canonicalize()
                .map_err(|e| CheckpointError::Io(e.to_string()))?,
        );
        let git_dir = data.join("checkpoints").join(session_id);
        std::fs::create_dir_all(&git_dir).map_err(|e| CheckpointError::Io(e.to_string()))?;
        // Canonicalized the same way as `work_tree`, because `restore` decides whether the shadow
        // repo sits inside the work tree by `strip_prefix`-ing one against the other — a literal
        // component comparison. Left in whatever spelling the environment supplied, the two sides
        // disagree over things that name the same directory: a `.` segment, a case difference, or
        // on Windows an 8.3 short name (`RUNNER~1` vs `runneradmin`, which is exactly what a CI
        // runner's TEMP looks like and why this passed on every developer machine). The guard then
        // silently does nothing and `git clean -fd` deletes the checkpoint history it was added to
        // protect.
        let git_dir = strip_extended_path_prefix(&git_dir.canonicalize().unwrap_or(git_dir));

        let sg = Self { git_dir, work_tree };
        if !sg.git_dir.join("HEAD").exists() {
            sg.run_git(&["init", "--quiet"])?;
            // Identity for commit-tree.
            sg.run_git(&["config", "user.email", "checkpoint@liberado.local"])?;
            sg.run_git(&["config", "user.name", "liberado-checkpoint"])?;
        }
        Ok(sg)
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    pub fn work_tree(&self) -> &Path {
        &self.work_tree
    }

    /// Snapshot the current work-tree; returns checkpoint id (= commit SHA).
    pub async fn snapshot(&self, label: &str) -> Result<Checkpoint, CheckpointError> {
        // Stage all (respects .gitignore in work-tree when using --work-tree).
        self.run_git_async(&["add", "-A"]).await?;
        let tree = self
            .run_git_async_stdout(&["write-tree"])
            .await?
            .trim()
            .to_string();
        if tree.is_empty() {
            return Err(CheckpointError::Git("write-tree returned empty".into()));
        }
        // Parent: previous HEAD if any.
        let parent = self.run_git_async_stdout(&["rev-parse", "HEAD"]).await.ok();
        let mut commit_args = vec![
            "commit-tree".to_string(),
            tree.clone(),
            "-m".into(),
            label.into(),
        ];
        if let Some(p) = parent {
            let p = p.trim().to_string();
            if !p.is_empty() && !p.contains("fatal") {
                commit_args.push("-p".into());
                commit_args.push(p);
            }
        }
        let args_ref: Vec<&str> = commit_args.iter().map(|s| s.as_str()).collect();
        let commit = self
            .run_git_async_stdout(&args_ref)
            .await?
            .trim()
            .to_string();
        // Update HEAD ref so the next snapshot has a parent chain.
        self.run_git_async(&["update-ref", "HEAD", &commit]).await?;
        Ok(Checkpoint {
            id: commit.clone(),
            label: label.to_string(),
            tree_hash: tree,
        })
    }

    /// Restore files in the work-tree from a checkpoint id (commit) or tree hash.
    /// Conversation/transcript is not touched.
    pub async fn restore(&self, checkpoint_id: &str) -> Result<(), CheckpointError> {
        if checkpoint_id.is_empty() || checkpoint_id.starts_with('-') {
            return Err(CheckpointError::NotFound(checkpoint_id.into()));
        }
        // Resolve to a tree: try as commit first, then as tree.
        let tree = match self
            .run_git_async_stdout(&["rev-parse", &format!("{checkpoint_id}^{{tree}}")])
            .await
        {
            Ok(t) => t.trim().to_string(),
            Err(_) => self
                .run_git_async_stdout(&["rev-parse", checkpoint_id])
                .await
                .map_err(|_| CheckpointError::NotFound(checkpoint_id.into()))?
                .trim()
                .to_string(),
        };
        // Replace index + work-tree from that tree.
        self.run_git_async(&["read-tree", &tree]).await?;
        self.run_git_async(&["checkout-index", "-a", "-f"]).await?;
        // Remove untracked files that weren't in the tree (best-effort clean slate).
        // Never delete the shadow repo itself if a misconfigured LIBERADO_DATA_DIR
        // put `git_dir` under the work tree (git clean would wipe HEAD and the chain).
        let mut clean_args = vec!["clean".to_string(), "-fd".into(), "-q".into()];
        if let Ok(rel) = self.git_dir.strip_prefix(&self.work_tree) {
            let rel_s = path_for_cli(rel);
            if !rel_s.is_empty() {
                clean_args.push("-e".into());
                clean_args.push(rel_s);
                // Also exclude parent segments (e.g. data/ when git_dir is data/checkpoints/id).
                let mut cur = PathBuf::new();
                for comp in rel.components() {
                    cur.push(comp);
                    if cur.as_os_str() != rel.as_os_str() {
                        clean_args.push("-e".into());
                        clean_args.push(path_for_cli(&cur));
                    }
                }
            }
        }
        // Common Liberado data roots that may sit inside a project workspace.
        clean_args.push("-e".into());
        clean_args.push(".liberado".into());
        let args_ref: Vec<&str> = clean_args.iter().map(|s| s.as_str()).collect();
        let _ = self.run_git_async(&args_ref).await;
        Ok(())
    }

    /// List checkpoints newest-first by walking HEAD parents (bounded).
    pub async fn list(&self, limit: usize) -> Result<Vec<Checkpoint>, CheckpointError> {
        let limit = limit.clamp(1, 100);
        let log = self
            .run_git_async_stdout(&["log", "--format=%H\t%T\t%s", &format!("-n{limit}"), "HEAD"])
            .await;
        let Ok(log) = log else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for line in log.lines() {
            let mut parts = line.splitn(3, '\t');
            let Some(id) = parts.next() else { continue };
            let Some(tree) = parts.next() else { continue };
            let label = parts.next().unwrap_or("").to_string();
            out.push(Checkpoint {
                id: id.to_string(),
                label,
                tree_hash: tree.to_string(),
            });
        }
        Ok(out)
    }

    /// Global options every invocation carries.
    ///
    /// A checkpoint promises the workspace comes back byte-identical, so the shadow repo must not
    /// translate anything on the way in or out. With `core.autocrlf=true` — the default for Git
    /// for Windows, and set in this machine's *system* config — restore rewrites every LF to
    /// CRLF, quietly corrupting the tree it exists to preserve.
    ///
    /// Passed per command rather than written into the repo config at creation, for two reasons.
    /// A config write only reached repos this build created: `open_or_init` skips the write when
    /// `HEAD` already exists, so a session resumed against a repo made by an older build restored
    /// with the host's setting and no code path noticed. And `-c` outranks every config file, so
    /// a value inherited from the system, the global, or the repo itself cannot win.
    fn base_args(&self) -> Vec<String> {
        vec![
            "--git-dir".into(),
            path_for_cli(&self.git_dir),
            "--work-tree".into(),
            path_for_cli(&self.work_tree),
            "-c".into(),
            "core.autocrlf=false".into(),
            "-c".into(),
            "core.safecrlf=false".into(),
        ]
    }

    fn run_git(&self, args: &[&str]) -> Result<(), CheckpointError> {
        let output = liberado_common::process::std_command("git")
            .args(self.base_args())
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| CheckpointError::Git(e.to_string()))?;
        if !output.status.success() {
            return Err(CheckpointError::Git(format!(
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(())
    }

    async fn run_git_async(&self, args: &[&str]) -> Result<(), CheckpointError> {
        let output = liberado_common::process::command("git")
            .args(self.base_args())
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| CheckpointError::Git(e.to_string()))?;
        if !output.status.success() {
            return Err(CheckpointError::Git(format!(
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(())
    }

    async fn run_git_async_stdout(&self, args: &[&str]) -> Result<String, CheckpointError> {
        let output = liberado_common::process::command("git")
            .args(self.base_args())
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| CheckpointError::Git(e.to_string()))?;
        if !output.status.success() {
            return Err(CheckpointError::Git(format!(
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;
