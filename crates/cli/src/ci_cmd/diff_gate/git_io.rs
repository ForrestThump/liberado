//! Git diff, merge-base, and a temporary worktree for base measurement.
//!
//! `git diff -M -U0 <merge-base>` compares the working tree with the merge-base, so a local
//! run includes uncommitted edits. Untracked `.rs` files are marked touched on every line.
//! The worktree lives at `.liberado/diff-gate-worktree` and is removed on drop. It is a real
//! git worktree, not a directory junction.

use std::path::{Path, PathBuf};

use super::hunks::{ParsedDiff, parse_unified_diff, touch_entire_file};

pub struct ResolvedBase {
    pub base_rev: String,
    pub merge_base: String,
}

pub fn resolve_base(
    root: &Path,
    explicit: Option<&str>,
) -> Result<ResolvedBase, Box<dyn std::error::Error>> {
    let head = rev(root, "HEAD")?;
    let base_rev = match explicit {
        Some(name) => name.to_string(),
        None => default_base(root, &head)?,
    };
    let merge_base = merge_of(root, "HEAD", &base_rev)?;
    Ok(ResolvedBase {
        base_rev,
        merge_base,
    })
}

fn default_base(root: &Path, head: &str) -> Result<String, Box<dyn std::error::Error>> {
    let with_main = merge_of(root, "HEAD", "origin/main")?;
    if with_main == head {
        Ok("HEAD^1".into())
    } else {
        Ok("origin/main".into())
    }
}

fn rev(root: &Path, spec: &str) -> Result<String, Box<dyn std::error::Error>> {
    Ok(super::git(root, &["rev-parse", spec])?.trim().to_string())
}

fn merge_of(root: &Path, left: &str, right: &str) -> Result<String, Box<dyn std::error::Error>> {
    Ok(super::git(root, &["merge-base", left, right])?
        .trim()
        .to_string())
}

pub fn load_diff(root: &Path, merge_base: &str) -> Result<ParsedDiff, Box<dyn std::error::Error>> {
    let text = super::git(root, &["diff", "-M", "--find-renames", "-U0", merge_base])?;
    let mut diff = parse_unified_diff(&text);
    add_untracked(root, &mut diff)?;
    Ok(diff)
}

fn add_untracked(root: &Path, diff: &mut ParsedDiff) -> Result<(), Box<dyn std::error::Error>> {
    let listed = super::git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    for path in listed.split('\0').filter(|path| path.ends_with(".rs")) {
        let count = std::fs::read_to_string(root.join(path))
            .map(|text| text.lines().count() as u32)
            .unwrap_or(1);
        touch_entire_file(diff, path, count);
    }
    Ok(())
}

pub struct Worktree {
    repo: PathBuf,
    path: PathBuf,
}

impl Worktree {
    pub fn add(repo: &Path, sha: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let path = repo.join(".liberado").join("diff-gate-worktree");
        remove_worktree(repo, &path);
        std::fs::create_dir_all(repo.join(".liberado"))?;
        let shown = path.to_string_lossy().into_owned();
        super::git(repo, &["worktree", "add", "--detach", &shown, sha])?;
        Ok(Self {
            repo: repo.to_path_buf(),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        remove_worktree(&self.repo, &self.path);
    }
}

fn remove_worktree(repo: &Path, path: &Path) {
    let shown = path.to_string_lossy().into_owned();
    let _ = super::git(repo, &["worktree", "remove", "--force", &shown]);
    let _ = std::fs::remove_dir_all(path);
    let _ = super::git(repo, &["worktree", "prune"]);
}

#[cfg(test)]
mod tests {
    use super::{Worktree, load_diff, resolve_base};
    use crate::ci_cmd::diff_gate::classify::classify;
    use crate::ci_cmd::diff_gate::model::{Class, ScoredFn};
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "test@example.com"]);
        git(dir.path(), &["config", "user.name", "Test"]);
        git(dir.path(), &["config", "core.autocrlf", "false"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        dir
    }

    fn commit_file(dir: &Path, relative: &str, body: &str, message: &str) {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
        git(dir, &["add", "--", relative]);
        git(dir, &["commit", "-m", message]);
    }

    fn scored(file: &str, function: &str, line: u32, end: u32) -> ScoredFn {
        ScoredFn {
            file: file.into(),
            function: function.into(),
            line,
            end_line: end,
            cyclomatic: 1.0,
            crap: None,
            body: String::new(),
        }
    }

    const BASE_SRC: &str = "fn keep() {\n    let _ = 1;\n}\n\nfn edit() {\n    let _ = 1;\n}\n";
    const HEAD_SRC: &str = "\
fn keep() {
    let _ = 1;
}

fn edit() {
    let _ = 2;
}

fn fresh() {
    let _ = 3;
}
";

    #[test]
    fn two_commits_classify_an_edit_as_legacy_and_a_new_function_as_new() {
        let dir = init_repo();
        commit_file(dir.path(), "src/work.rs", BASE_SRC, "base");
        commit_file(dir.path(), "src/work.rs", HEAD_SRC, "head");
        let resolved = resolve_base(dir.path(), Some("HEAD^")).unwrap();
        assert_eq!(
            resolved.merge_base,
            git_out(dir.path(), &["rev-parse", "HEAD^"])
        );
        let diff = load_diff(dir.path(), &resolved.merge_base).unwrap();
        let head = vec![
            scored("src/work.rs", "keep", 1, 3),
            scored("src/work.rs", "edit", 5, 7),
            scored("src/work.rs", "fresh", 9, 11),
        ];
        let base = vec![
            scored("src/work.rs", "keep", 1, 3),
            scored("src/work.rs", "edit", 5, 7),
        ];
        let found = classify(&head, &base, &diff);
        let class_of = |name: &str| {
            found
                .iter()
                .find(|item| head[item.head_index].function == name)
                .map(|item| item.class.clone())
        };
        assert!(
            class_of("keep").is_none(),
            "untouched keep must not be gated"
        );
        assert_eq!(class_of("edit"), Some(Class::Legacy));
        assert_eq!(class_of("fresh"), Some(Class::New));
    }

    #[test]
    fn an_uncommitted_edit_touches_the_function() {
        let dir = init_repo();
        commit_file(dir.path(), "src/work.rs", BASE_SRC, "base");
        std::fs::write(
            dir.path().join("src/work.rs"),
            "fn keep() {\n    let _ = 9;\n}\n\nfn edit() {\n    let _ = 1;\n}\n",
        )
        .unwrap();
        let diff = load_diff(dir.path(), "HEAD").unwrap();
        assert!(diff.lines_for("src/work.rs").contains(&2));
    }

    #[test]
    fn a_git_rename_plus_edit_is_renamed_from_the_old_path() {
        let dir = init_repo();
        commit_file(
            dir.path(),
            "src/work.rs",
            "fn moved() {\n    let _ = 1;\n}\n",
            "base",
        );
        git(dir.path(), &["mv", "src/work.rs", "src/moved.rs"]);
        std::fs::write(
            dir.path().join("src/moved.rs"),
            "fn moved() {\n    let _ = 2;\n}\n",
        )
        .unwrap();
        git(dir.path(), &["add", "--", "src/moved.rs"]);
        git(dir.path(), &["commit", "-m", "rename"]);
        let diff = load_diff(dir.path(), "HEAD^").unwrap();
        let head = vec![scored("src/moved.rs", "moved", 1, 3)];
        let base = vec![scored("src/work.rs", "moved", 1, 3)];
        let found = classify(&head, &base, &diff);
        match &found[0].class {
            Class::RenamedFrom { file, function } => {
                assert_eq!(file, "src/work.rs");
                assert_eq!(function, "moved");
            }
            other => panic!("expected rename, got {other:?}"),
        }
    }

    #[test]
    fn the_worktree_is_removed_when_it_drops() {
        let dir = init_repo();
        commit_file(dir.path(), "src/work.rs", BASE_SRC, "base");
        let sha = git_out(dir.path(), &["rev-parse", "HEAD"]);
        {
            let tree = Worktree::add(dir.path(), &sha).unwrap();
            assert!(tree.path().join("src/work.rs").is_file());
        }
        assert!(!dir.path().join(".liberado/diff-gate-worktree").exists());
    }
}
