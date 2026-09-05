//! Dry-run execution of shepherd action helpers. No daemon, no `gh`.

use super::*;
use std::path::PathBuf;

fn test_config(root: PathBuf) -> Config {
    Config {
        root,
        repository: None,
        check_names: Vec::new(),
        daemon: String::new(),
        project: String::new(),
        base: "main".into(),
        profile: String::new(),
        max_kickbacks: 2,
        cold_reviews: 2,
        cold_turns: 60,
        max_concurrent: 2,
        poll: 120,
    }
}

fn sample_pr(labels: &[&str]) -> Pr {
    Pr {
        number: 7,
        title: "t".into(),
        branch: "feat".into(),
        base_sha: String::new(),
        head_sha: "abc1234".into(),
        url: "https://example.test/pr/7".into(),
        labels: labels.iter().map(|label| (*label).to_string()).collect(),
    }
}

#[test]
fn pending_parent_rejects_a_root_path() {
    let err = pending_parent(Path::new("/")).unwrap_err().to_string();
    assert!(err.contains("no parent"), "{err}");
}

#[test]
fn pending_parent_returns_the_directory() {
    let path = Path::new("/tmp/pending_reviews/7.json");
    assert_eq!(
        pending_parent(path).unwrap(),
        Path::new("/tmp/pending_reviews")
    );
}

#[test]
fn persist_pending_review_writes_the_session_file() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    persist_pending_review(&cfg, 7, "sess-1", 2).unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(pending(&cfg, 7)).unwrap()).unwrap();
    assert_eq!(value["session_id"], "sess-1");
    assert_eq!(value["round"], 2);
}

#[test]
fn dry_cold_review_does_not_write_a_pending_file() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let pr = sample_pr(&[]);
    start_cold_review(&cfg, &pr, true, &BTreeSet::new(), 1).unwrap();
    assert!(
        !pending(&cfg, pr.number).exists(),
        "--dry-run must not persist a pending review"
    );
}

#[test]
fn dry_kickback_does_not_label_the_pr() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let mut pr = sample_pr(&[RERUN]);
    let new = BTreeSet::from(["job|test".into()]);
    kickback(&cfg, &mut pr, true, &new, &BTreeSet::new(), 0).unwrap();
    assert!(
        !pr.labels
            .iter()
            .any(|label| label.starts_with("shepherd:kickback-")),
        "--dry-run must not add a kickback label"
    );
    assert!(
        pr.has(RERUN),
        "--dry-run must leave the rerun label in place"
    );
}

#[test]
fn dry_new_failures_rerun_does_not_label() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let mut pr = sample_pr(&[]);
    handle_new_failures(
        &cfg,
        &mut pr,
        true,
        &BTreeSet::new(),
        &BTreeSet::new(),
        &None,
    )
    .unwrap();
    assert!(!pr.has(RERUN), "--dry-run must not add {RERUN}");
}

#[test]
fn dry_new_failures_blocked_does_not_label() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let mut pr = sample_pr(&[RERUN, "shepherd:kickback-1", "shepherd:kickback-2"]);
    handle_new_failures(
        &cfg,
        &mut pr,
        true,
        &BTreeSet::new(),
        &BTreeSet::new(),
        &None,
    )
    .unwrap();
    assert!(!pr.has(BLOCKED), "--dry-run must not add {BLOCKED}");
}

#[test]
fn dry_clean_ready_does_not_label() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let mut pr = sample_pr(&["shepherd:review-1", "shepherd:review-2"]);
    handle_clean(&cfg, &mut pr, true, &BTreeSet::new(), Some(99)).unwrap();
    assert!(!pr.has(READY), "--dry-run must not add {READY}");
}
