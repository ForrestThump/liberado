//! Tick helpers that can run without `gh` or a daemon.

use super::*;

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
fn settled_ci_state_follows_the_empty_failure_set() {
    assert_eq!(settled_ci_state(true), "success");
    assert_eq!(settled_ci_state(false), "failure");
}

#[test]
fn tick_idles_on_a_ready_or_blocked_pr() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    for label in [READY, BLOCKED] {
        let mut pr = sample_pr(&[label]);
        tick(&cfg, &mut pr, true).unwrap();
        assert!(pr.has(label), "terminal tick must not change labels");
    }
}

#[test]
fn record_settled_ci_is_a_noop_when_dry() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let pr = sample_pr(&[]);
    let new = BTreeSet::from(["job|test".into()]);
    let run = Some(json!({"databaseId": 99}));
    record_settled_ci(&cfg, &pr, true, &new, &run).unwrap();
    assert!(
        !cfg.state().exists(),
        "--dry-run must not create a shepherd state directory"
    );
}
