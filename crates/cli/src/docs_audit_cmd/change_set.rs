use liberado_common::process::std_command;
use std::collections::BTreeSet;
use std::path::Path;

pub(super) fn changed_files(
    root: &Path,
    base: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut changed = git_path_set(root, &["diff", "--name-only", base, "--"])?;
    changed.extend(git_path_set(
        root,
        &["ls-files", "--others", "--exclude-standard"],
    )?);
    Ok(changed.into_iter().collect())
}

fn git_path_set(
    root: &Path,
    args: &[&str],
) -> Result<BTreeSet<String>, Box<dyn std::error::Error>> {
    let output = std_command("git").current_dir(root).args(args).output()?;
    if !output.status.success() {
        return Err(format!("git {} failed", args.join(" ")).into());
    }
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .map(|line| line.replace('\\', "/"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::changed_files;
    use liberado_common::process::std_command;
    use std::fs;

    #[test]
    fn changed_files_include_the_worktree_and_untracked_files() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = std_command("git")
                .args(args)
                .current_dir(dir.path())
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .status()
                .unwrap();
            assert!(status.success(), "git {}", args.join(" "));
        };
        git(&["init"]);
        fs::write(dir.path().join("source.rs"), "old\n").unwrap();
        git(&["add", "source.rs"]);
        git(&["commit", "-m", "base"]);
        let base = String::from_utf8(
            std_command("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(dir.path())
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();

        fs::write(dir.path().join("source.rs"), "new\n").unwrap();
        fs::write(dir.path().join("new-doc.md"), "docs\n").unwrap();

        assert_eq!(
            changed_files(dir.path(), base.trim()).unwrap(),
            vec!["new-doc.md", "source.rs"]
        );
    }
}
