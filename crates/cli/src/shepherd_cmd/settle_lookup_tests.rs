//! A failed daemon status lookup must not delete the pending review.

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

#[test]
fn settle_with_keeps_the_pending_file_when_status_lookup_fails() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let mut pr = Pr {
        number: 42,
        title: "test".into(),
        branch: "test".into(),
        base_sha: String::new(),
        head_sha: String::new(),
        url: String::new(),
        labels: Vec::new(),
    };
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
