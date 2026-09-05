//! Ledger recording tests, split from `shepherd_cmd_tests.rs` for module-health.

use super::super::{Config, Pr};
use super::{ShepherdFact, open_recorded, record_facts};
use liberado_coder_core::{durable_tasks_root, shepherd_task_id};
use std::path::{Path, PathBuf};
use std::process::Command;

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

fn sample_pr(number: u64, head_sha: &str) -> Pr {
    Pr {
        number,
        title: "feat".into(),
        branch: "feat/x".into(),
        base_sha: "bbbb".into(),
        head_sha: head_sha.into(),
        url: format!("https://github.com/ForrestThump/liberado/pull/{number}"),
        labels: Vec::new(),
    }
}

#[test]
fn shepherd_dry_run_writes_no_task_ledger() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let pr = sample_pr(12, "aaa111");
    let recorded = record_facts(
        &cfg,
        &pr,
        true,
        &[ShepherdFact::Ci {
            github_run_id: Some(9),
            state: "success".into(),
            failures: Vec::new(),
        }],
    )
    .unwrap();
    assert!(recorded.is_none());
    assert!(
        !temp.path().join(".liberado/tasks").exists(),
        "dry-run must not create the durable ledger root"
    );
}

#[test]
fn shepherd_repeated_facts_are_idempotent_and_survive_reload() {
    let temp = tempfile::tempdir().unwrap();
    let mut cfg = test_config(temp.path().to_path_buf());
    cfg.repository = Some("ForrestThump/liberado".into());
    let pr = sample_pr(15, "ccc222ddd333");
    let facts = [
        ShepherdFact::Ci {
            github_run_id: Some(88),
            state: "failure".into(),
            failures: vec!["job|test".into()],
        },
        ShepherdFact::Rerun {
            github_run_id: Some(88),
        },
        ShepherdFact::Repair {
            goal_id: Some("goal-a".into()),
            reason: "1 new CI failures".into(),
            kick: 1,
        },
    ];
    let first = record_facts(&cfg, &pr, false, &facts)
        .unwrap()
        .expect("live record");
    assert_eq!(first.rerun_count, 1);
    assert_eq!(first.repair_count, 1);
    assert_eq!(first.pull_request_number, Some(15));
    assert_eq!(first.head_revision.as_deref(), Some("ccc222ddd333"));
    assert_eq!(first.controller.as_deref(), Some("liberado-shepherd"));
    assert!(!first.is_pr_ready());
    let first_len = open_recorded(&cfg, &pr).unwrap().events().len();

    let second = record_facts(&cfg, &pr, false, &facts)
        .unwrap()
        .expect("repeat record");
    assert_eq!(second.rerun_count, first.rerun_count);
    assert_eq!(second.repair_count, first.repair_count);
    let second_len = open_recorded(&cfg, &pr).unwrap().events().len();
    assert_eq!(second_len, first_len);

    let reloaded = open_recorded(&cfg, &pr).unwrap();
    let restored = reloaded.project().unwrap();
    assert_eq!(restored.rerun_count, 1);
    assert_eq!(restored.repair_count, 1);
    assert_eq!(restored.github_run_id, Some(88));
    assert_eq!(reloaded.events().len(), first_len);
}

#[test]
fn shepherd_ready_binds_head_ci_and_review() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = test_config(temp.path().to_path_buf());
    let pr = sample_pr(3, "fff444");
    let ready = record_facts(
        &cfg,
        &pr,
        false,
        &[
            ShepherdFact::Ci {
                github_run_id: Some(21),
                state: "success".into(),
                failures: Vec::new(),
            },
            ShepherdFact::ReviewApproved { round: 2 },
            ShepherdFact::Ready {
                github_run_id: Some(21),
                review_round: 2,
            },
        ],
    )
    .unwrap()
    .expect("ready record");
    assert!(ready.is_pr_ready());
    assert_eq!(ready.ready_evidence.as_ref().unwrap().head_sha, "fff444");
    assert_eq!(
        ready.ready_evidence.as_ref().unwrap().ci_github_run_id,
        Some(21)
    );
    assert_eq!(ready.status, liberado_coder_core::TaskStatus::Completed);
}

#[test]
fn shepherd_ledger_lives_on_the_main_repository_not_a_linked_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let main = temp.path().join("main");
    let leaf = temp.path().join("leaf");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init"]);
    git(&main, &["config", "user.name", "Liberado Test"]);
    git(&main, &["config", "user.email", "test@liberado.local"]);
    std::fs::write(main.join("README.md"), "shepherd\n").unwrap();
    git(&main, &["add", "README.md"]);
    git(&main, &["commit", "-m", "init"]);
    git(
        &main,
        &["worktree", "add", "--detach", leaf.to_str().unwrap()],
    );

    let cfg = test_config(leaf.clone());
    let pr = sample_pr(24, "abc123def456");
    record_facts(
        &cfg,
        &pr,
        false,
        &[ShepherdFact::Ci {
            github_run_id: Some(3),
            state: "success".into(),
            failures: Vec::new(),
        }],
    )
    .unwrap()
    .expect("live record");

    let task_id = shepherd_task_id(None, 24);
    let main_ledger = durable_tasks_root(canonicalize_dir(&main))
        .join(&task_id)
        .join("ledger.jsonl");
    assert!(
        main_ledger.is_file(),
        "shepherd must store the ledger on the main repository, missing {}",
        main_ledger.display()
    );
    assert!(
        !leaf.join(".liberado").exists(),
        "a disposable worktree must not be the sole ledger home"
    );
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn canonicalize_dir(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
