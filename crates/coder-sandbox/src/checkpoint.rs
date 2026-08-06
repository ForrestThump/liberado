//! Kilo-style shadow-git checkpoints (coding-tui S4 / G3).
//!
//! A side repo outside the project holds tree snapshots of a workspace. Restore is
//! **files-only** (conversation/transcript untouched). Used for mid-build park/resume
//! and `POST /api/goals/{id}/rewind`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::process::Command;

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
        let data = std::env::var("LIBERADO_DATA_DIR").unwrap_or_else(|_| ".liberado".into());
        let git_dir = PathBuf::from(data).join("checkpoints").join(session_id);
        std::fs::create_dir_all(&git_dir).map_err(|e| CheckpointError::Io(e.to_string()))?;

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
        let mut commit_args = vec!["commit-tree".to_string(), tree.clone(), "-m".into(), label.into()];
        if let Some(p) = parent {
            let p = p.trim().to_string();
            if !p.is_empty() && !p.contains("fatal") {
                commit_args.push("-p".into());
                commit_args.push(p);
            }
        }
        let args_ref: Vec<&str> = commit_args.iter().map(|s| s.as_str()).collect();
        let commit = self.run_git_async_stdout(&args_ref).await?.trim().to_string();
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
        let limit = limit.max(1).min(100);
        let log = self
            .run_git_async_stdout(&[
                "log",
                "--format=%H\t%T\t%s",
                &format!("-n{limit}"),
                "HEAD",
            ])
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

    fn run_git(&self, args: &[&str]) -> Result<(), CheckpointError> {
        let git_dir = path_for_cli(&self.git_dir);
        let work = path_for_cli(&self.work_tree);
        let output = std::process::Command::new("git")
            .args(["--git-dir", &git_dir, "--work-tree", &work])
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
        let git_dir = path_for_cli(&self.git_dir);
        let work = path_for_cli(&self.work_tree);
        let output = Command::new("git")
            .args(["--git-dir", &git_dir, "--work-tree", &work])
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
        let git_dir = path_for_cli(&self.git_dir);
        let work = path_for_cli(&self.work_tree);
        let output = Command::new("git")
            .args(["--git-dir", &git_dir, "--work-tree", &work])
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
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// `LIBERADO_DATA_DIR` is process-global — serialize tests that mutate it.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn unique() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[tokio::test]
    async fn snapshot_then_restore_is_byte_identical() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("lib-ckpt-{}", unique()));
        let root = base.join("ws");
        // Side repo lives outside the work tree (production-shaped); also covers the
        // under-worktree case via exclude rules in restore.
        let data = base.join("data");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        std::fs::write(root.join("b.txt"), "world\n").unwrap();

        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", &data);
        }
        let sg = ShadowGit::open_or_init(&root, "sess1").unwrap();
        let cp = sg.snapshot("base").await.unwrap();
        assert!(!cp.id.is_empty());
        assert!(!cp.tree_hash.is_empty());

        std::fs::write(root.join("a.txt"), "MUTATED\n").unwrap();
        std::fs::write(root.join("c.txt"), "new\n").unwrap();
        std::fs::remove_file(root.join("b.txt")).unwrap();

        sg.restore(&cp.id).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "hello\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("b.txt")).unwrap(),
            "world\n"
        );
        assert!(!root.join("c.txt").exists());

        let list = sg.list(10).await.unwrap();
        assert!(
            !list.is_empty(),
            "expected at least the base checkpoint in list"
        );
        assert_eq!(list[0].id, cp.id);

        unsafe {
            std::env::remove_var("LIBERADO_DATA_DIR");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn second_snapshot_chains_and_rewind_to_first() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("lib-ckpt2-{}", unique()));
        let root = base.join("ws");
        let data = base.join("data");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("f.txt"), "v1\n").unwrap();
        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", &data);
        }
        let sg = ShadowGit::open_or_init(&root, "sess2").unwrap();
        let c1 = sg.snapshot("v1").await.unwrap();
        std::fs::write(root.join("f.txt"), "v2\n").unwrap();
        let c2 = sg.snapshot("v2").await.unwrap();
        assert_ne!(c1.id, c2.id);

        sg.restore(&c1.id).await.unwrap();
        assert_eq!(std::fs::read_to_string(root.join("f.txt")).unwrap(), "v1\n");

        unsafe {
            std::env::remove_var("LIBERADO_DATA_DIR");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn restore_survives_when_side_repo_is_under_work_tree() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("lib-ckpt-nested-{}", unique()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("f.txt"), "keep\n").unwrap();
        // Deliberately nested — used to make `git clean -fd` wipe the shadow repo.
        let data = root.join("data");
        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", &data);
        }
        let sg = ShadowGit::open_or_init(&root, "nested").unwrap();
        let cp = sg.snapshot("base").await.unwrap();
        std::fs::write(root.join("f.txt"), "mut\n").unwrap();
        std::fs::write(root.join("extra.txt"), "x\n").unwrap();
        sg.restore(&cp.id).await.unwrap();
        assert_eq!(std::fs::read_to_string(root.join("f.txt")).unwrap(), "keep\n");
        assert!(!root.join("extra.txt").exists());
        let list = sg.list(5).await.unwrap();
        assert_eq!(list[0].id, cp.id);
        unsafe {
            std::env::remove_var("LIBERADO_DATA_DIR");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
