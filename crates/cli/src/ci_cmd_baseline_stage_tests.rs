//! Split from `ci_cmd_tests.rs`: CRAP baseline staging and cargo-crap.toml twin sync.

use super::{BASELINE_FILE, StageOutcome, git, porcelain_path, stage_ratcheted_baseline};
use liberado_common::process::std_command;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn init_repo() -> tempfile::TempDir {
    let temp = tempdir().unwrap();
    let root = temp.path();
    assert!(
        std_command("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    for (key, value) in [
        ("user.email", "liberado@example.invalid"),
        ("user.name", "Liberado Test"),
    ] {
        assert!(
            std_command("git")
                .args(["config", key, value])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
    }
    fs::write(root.join("README"), "base\n").unwrap();
    git(root, &["add", "README"]).unwrap();
    git(root, &["commit", "-q", "-m", "base"]).unwrap();
    temp
}

fn commit_contains(root: &Path, needle: &str) -> bool {
    git(root, &["show", "--name-only", "--pretty=format:", "HEAD"])
        .unwrap()
        .lines()
        .any(|line| line.trim() == needle)
}

#[test]
fn porcelain_path_skips_the_two_status_columns() {
    assert_eq!(
        porcelain_path("M  code-metrics/crap-baseline.json"),
        Some("code-metrics/crap-baseline.json")
    );
    assert_eq!(porcelain_path("?? other.rs"), Some("other.rs"));
    assert_eq!(porcelain_path("M"), None);
}

#[test]
fn a_clean_tree_amends_the_baseline_onto_head() {
    let temp = init_repo();
    let root = temp.path();
    fs::create_dir_all(root.join("code-metrics")).unwrap();
    fs::write(root.join(BASELINE_FILE), "{\"entries\":[]}\n").unwrap();
    assert_eq!(
        stage_ratcheted_baseline(root).unwrap(),
        StageOutcome::Amended
    );
    assert!(commit_contains(root, BASELINE_FILE));
    assert!(git(root, &["status", "--porcelain"]).unwrap().is_empty());
}

#[test]
fn a_dirty_tree_only_stages_the_baseline() {
    let temp = init_repo();
    let root = temp.path();
    fs::write(root.join("dirty.rs"), "fn f() {}\n").unwrap();
    fs::create_dir_all(root.join("code-metrics")).unwrap();
    fs::write(root.join(BASELINE_FILE), "{\"entries\":[]}\n").unwrap();
    assert_eq!(
        stage_ratcheted_baseline(root).unwrap(),
        StageOutcome::Staged
    );
    assert!(!commit_contains(root, BASELINE_FILE));
    let status = git(root, &["status", "--porcelain"]).unwrap();
    assert!(
        status.lines().any(|line| line.contains(BASELINE_FILE)
            && line.as_bytes().first().is_some_and(|c| *c != b'?')),
        "baseline should be staged:\n{status}"
    );
    assert!(
        status.lines().any(|line| line.contains("dirty.rs")),
        "other dirty files stay unstaged:\n{status}"
    );
}

#[test]
fn an_unchanged_baseline_is_a_no_op() {
    let temp = init_repo();
    let root = temp.path();
    fs::create_dir_all(root.join("code-metrics")).unwrap();
    fs::write(root.join(BASELINE_FILE), "{\"entries\":[]}\n").unwrap();
    git(root, &["add", BASELINE_FILE]).unwrap();
    git(root, &["commit", "-q", "-m", "baseline"]).unwrap();
    assert_eq!(
        stage_ratcheted_baseline(root).unwrap(),
        StageOutcome::Unchanged
    );
    assert_eq!(
        git(root, &["log", "-1", "--pretty=%s"]).unwrap().trim(),
        "baseline"
    );
}

#[test]
fn cargo_crap_toml_twins_stay_identical() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate is crates/cli");
    let discovered = std::fs::read_to_string(root.join(".cargo-crap.toml")).expect("root toml");
    let documented = std::fs::read_to_string(root.join("code-metrics/cargo-crap.toml"))
        .expect("code-metrics toml");
    // Strip comment-only header differences: compare from the first `threshold` line.
    assert_eq!(
        toml_body_from_threshold(&discovered),
        toml_body_from_threshold(&documented),
        "root .cargo-crap.toml and code-metrics/cargo-crap.toml bodies must stay identical"
    );
}

fn toml_body_from_threshold(s: &str) -> String {
    let mut lines = s.lines();
    while let Some(line) = lines.next() {
        if line.starts_with("threshold") {
            let mut out = String::from(line);
            for rest in lines {
                out.push('\n');
                out.push_str(rest);
            }
            return out;
        }
    }
    String::new()
}
