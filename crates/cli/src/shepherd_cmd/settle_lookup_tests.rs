//! A failed daemon status lookup must not delete the pending review.

use super::*;
use std::path::PathBuf;

fn sample_pr() -> Pr {
    Pr {
        number: 42,
        title: "test".into(),
        branch: "test".into(),
        base_sha: String::new(),
        head_sha: String::new(),
        url: String::new(),
        labels: Vec::new(),
    }
}

fn write_pending(cfg: &Config, body: &str) -> PathBuf {
    let path = pending(cfg, 42);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, body).unwrap();
    path
}

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

#[test]
fn settle_with_keeps_the_pending_file_when_status_lookup_fails() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let mut pr = sample_pr();
    let path = pending(&cfg, pr.number);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, r#"{"session_id":"one","round":1}"#).unwrap();
    let error = settle_with(
        &cfg,
        &mut pr,
        false,
        |_| {
            Err(
                "liberado daemon rejected shepherd request to /api/goals/one: HTTP 401 Unauthorized"
                    .into(),
            )
        },
        |_, _| panic!("labeler must not run when status lookup fails"),
    );
    let message = error.unwrap_err().to_string();
    assert!(message.contains("HTTP 401"));
    assert!(
        path.exists(),
        "a status error must leave the pending review"
    );
    assert!(fs::read_to_string(&path).unwrap().contains("one"));
}

#[test]
fn missing_pending_file_settles_as_none() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let mut pr = sample_pr();
    let transition = settle_with(
        &cfg,
        &mut pr,
        false,
        |_| panic!("lookup must not run without a pending file"),
        |_, _| panic!("labeler must not run without a pending file"),
    )
    .unwrap();
    assert_eq!(transition, ReviewTransition::None);
}

#[test]
fn incomplete_pending_file_is_removed_only_when_live() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    for (body, dry, removed) in [
        (r#"{"round":1}"#, false, true),
        (r#"{"round":1}"#, true, false),
        (r#"{"session_id":"one"}"#, false, true),
        (r#"{"session_id":"one"}"#, true, false),
    ] {
        let mut pr = sample_pr();
        let path = write_pending(&cfg, body);
        let transition = settle_with(
            &cfg,
            &mut pr,
            dry,
            |_| panic!("lookup must not run for an incomplete pending file"),
            |_, _| panic!("labeler must not run for an incomplete pending file"),
        )
        .unwrap();
        assert_eq!(transition, ReviewTransition::None);
        assert_eq!(path.exists(), !removed, "{body} dry={dry}");
    }
}

#[test]
fn failed_review_keeps_the_pending_file_when_dry() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let mut pr = sample_pr();
    let path = write_pending(&cfg, r#"{"session_id":"one","round":1}"#);
    let transition = settle_with(
        &cfg,
        &mut pr,
        true,
        |_| Ok(Some("failed".into())),
        |_, _| panic!("labeler must not run for a failed review"),
    )
    .unwrap();
    assert_eq!(transition, ReviewTransition::Failed);
    assert!(path.exists());
}

#[test]
fn waiting_review_leaves_the_pending_file() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let mut pr = sample_pr();
    let path = write_pending(&cfg, r#"{"session_id":"one","round":1}"#);
    let transition = settle_with(
        &cfg,
        &mut pr,
        false,
        |_| Ok(Some("running".into())),
        |_, _| panic!("labeler must not run while the review is waiting"),
    )
    .unwrap();
    assert_eq!(transition, ReviewTransition::Waiting);
    assert!(path.exists());
    let still_waiting = settle_with(
        &cfg,
        &mut pr,
        false,
        |_| Ok(None),
        |_, _| panic!("labeler must not run while the review is waiting"),
    )
    .unwrap();
    assert_eq!(still_waiting, ReviewTransition::Waiting);
    assert!(path.exists());
}

#[test]
fn invalid_pending_json_keeps_the_file() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let mut pr = sample_pr();
    let path = write_pending(&cfg, "not-json");
    let error = settle_with(
        &cfg,
        &mut pr,
        false,
        |_| panic!("lookup must not run for invalid JSON"),
        |_, _| panic!("labeler must not run for invalid JSON"),
    );
    assert!(error.is_err());
    assert!(path.exists());
}
