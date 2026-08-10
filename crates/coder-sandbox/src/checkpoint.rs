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
            // A checkpoint promises the workspace comes back byte-identical, so the shadow repo
            // must not translate anything on the way in or out. With `core.autocrlf=true` — the
            // default for Git for Windows, and set in this machine's *system* config — restore
            // rewrites every LF to CRLF, quietly corrupting the tree it was meant to preserve.
            // Pinned here rather than inherited, because the host's setting is not ours to trust.
            sg.run_git(&["config", "core.autocrlf", "false"])?;
            sg.run_git(&["config", "core.safecrlf", "false"])?;
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

    fn run_git(&self, args: &[&str]) -> Result<(), CheckpointError> {
        let git_dir = path_for_cli(&self.git_dir);
        let work = path_for_cli(&self.work_tree);
        let output = liberado_common::process::std_command("git")
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
        let output = liberado_common::process::command("git")
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
        let output = liberado_common::process::command("git")
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
        let data = base.join("data");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        std::fs::write(root.join("b.txt"), "world\n").unwrap();

        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", &data);
        }
        let sg = ShadowGit::open_or_init(&root, "sess1").unwrap();
        drop(_guard); // drop before .await for clippy await_holding_lock

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
        drop(_guard);

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
        let data = root.join("data");
        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", &data);
        }
        let sg = ShadowGit::open_or_init(&root, "nested").unwrap();
        drop(_guard);

        let cp = sg.snapshot("base").await.unwrap();
        std::fs::write(root.join("f.txt"), "mut\n").unwrap();
        std::fs::write(root.join("extra.txt"), "x\n").unwrap();
        sg.restore(&cp.id).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "keep\n"
        );
        assert!(!root.join("extra.txt").exists());
        let list = sg.list(5).await.unwrap();
        assert_eq!(list[0].id, cp.id);
        unsafe {
            std::env::remove_var("LIBERADO_DATA_DIR");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `git_dir` is stored canonicalized, in the same spelling as `work_tree`.
    ///
    /// `restore` decides whether the shadow repo sits inside the work tree by `strip_prefix`-ing
    /// one against the other, which compares components literally. `work_tree` is canonicalized;
    /// if `git_dir` keeps whatever spelling the environment supplied, the two disagree over paths
    /// that name the same directory — on Windows a runner's `TEMP` gives the 8.3 short form
    /// (`RUNNER~1`) while canonicalize yields `runneradmin`. The guard then matches nothing and
    /// `git clean -fd` deletes the checkpoint history it exists to protect, surfacing as an empty
    /// `list()` rather than as anything that mentions paths.
    ///
    /// Asserted as an invariant rather than by staging an odd spelling: `Path::components()`
    /// normalizes `.` away on its own, so the obvious repro tests nothing, and the spellings that
    /// *do* break it (8.3, case-insensitivity) exist only on some platforms.
    #[test]
    fn git_dir_is_stored_canonicalized_so_the_clean_guard_can_match() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("lib-ckpt-canon-{}", unique()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        // A `..` segment: names `root/data`, but is not the canonical spelling of it. Unlike `.`,
        // which `Path::components()` quietly drops, `..` survives into the literal comparison —
        // so this stands in for the runner's 8.3 `TEMP` without needing a Windows-only fixture.
        let data = root.join("sub").join("..").join("data");
        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", &data);
        }
        let sg = ShadowGit::open_or_init(&root, "canon").unwrap();

        let canonical = strip_extended_path_prefix(&sg.git_dir().canonicalize().unwrap());
        assert_eq!(
            sg.git_dir(),
            canonical.as_path(),
            "git_dir must be canonical or the clean-exclusion guard silently misses"
        );
        // And with both sides canonical the guard actually resolves.
        assert!(
            sg.git_dir().strip_prefix(sg.work_tree()).is_ok(),
            "git_dir under the work tree must strip cleanly: {:?} vs {:?}",
            sg.git_dir(),
            sg.work_tree()
        );

        unsafe {
            std::env::remove_var("LIBERADO_DATA_DIR");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_or_init_rejects_path_traversal() {
        assert!(ShadowGit::open_or_init(Path::new("."), "a/b").is_err());
        assert!(ShadowGit::open_or_init(Path::new("."), "a\\b").is_err());
        assert!(ShadowGit::open_or_init(Path::new("."), "..").is_err());
        assert!(ShadowGit::open_or_init(Path::new("."), "").is_err());
        assert!(ShadowGit::open_or_init(Path::new("."), "a../b").is_err());
    }

    #[test]
    fn git_dir_and_work_tree_accessors() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("lib-ckpt-getters-{}", unique()));
        let root = base.join("ws");
        let data = base.join("data");
        std::fs::create_dir_all(&root).unwrap();
        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", &data);
        }
        let sg = ShadowGit::open_or_init(&root, "g1").unwrap();
        assert!(sg.git_dir().to_string_lossy().contains("checkpoints"));
        assert!(sg.git_dir().to_string_lossy().contains("g1"));
        assert!(sg.work_tree().ends_with("ws"));
        unsafe {
            std::env::remove_var("LIBERADO_DATA_DIR");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn list_clamps_limit_to_valid_range() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("lib-ckpt-list-{}", unique()));
        let root = base.join("ws");
        let data = base.join("data");
        std::fs::create_dir_all(&root).unwrap();
        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", &data);
        }
        let sg = ShadowGit::open_or_init(&root, "sess-list").unwrap();
        drop(_guard);

        for i in 1..=3 {
            std::fs::write(root.join("f.txt"), format!("v{i}\n")).unwrap();
            sg.snapshot(&format!("v{i}")).await.unwrap();
        }
        let items = sg.list(0).await.unwrap();
        assert!(!items.is_empty(), "list(0) should clamp to at least 1");
        let items = sg.list(500).await.unwrap();
        assert!(!items.is_empty());
        assert!(items.len() <= 100);
        unsafe {
            std::env::remove_var("LIBERADO_DATA_DIR");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Restore stays byte-exact even when the host turns line-ending translation on.
    ///
    /// `core.autocrlf=true` is the Git for Windows default and is set in this machine's *system*
    /// config; a developer-level `false` was the only thing hiding it locally, so the three
    /// round-trip tests passed here and failed on every CI runner. Left inherited, restore
    /// rewrites every LF to CRLF — silent corruption in the one operation whose entire promise is
    /// that the bytes come back unchanged.
    ///
    /// The other tests would catch this only on a host that happens to enable autocrlf. This one
    /// forces it on regardless, so the guarantee is pinned rather than left to the environment.
    #[tokio::test]
    async fn restore_is_byte_exact_even_when_the_host_enables_autocrlf() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("lib-ckpt-crlf-{}", unique()));
        let root = base.join("ws");
        let data = base.join("data");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();

        let cfg = base.join("gitconfig");
        std::fs::write(&cfg, "[core]\n\tautocrlf = true\n").unwrap();

        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", &data);
            std::env::set_var("GIT_CONFIG_GLOBAL", &cfg);
        }
        let sg = ShadowGit::open_or_init(&root, "sess-crlf").unwrap();
        drop(_guard);

        let cp = sg.snapshot("base").await.unwrap();
        std::fs::write(root.join("a.txt"), "MUTATED\n").unwrap();
        sg.restore(&cp.id).await.unwrap();

        // Compare bytes, not a string: `\r` is exactly what would be smuggled in.
        assert_eq!(
            std::fs::read(root.join("a.txt")).unwrap(),
            b"one\ntwo\nthree\n",
            "restore must not translate line endings"
        );

        unsafe {
            std::env::remove_var("LIBERADO_DATA_DIR");
            std::env::remove_var("GIT_CONFIG_GLOBAL");
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
