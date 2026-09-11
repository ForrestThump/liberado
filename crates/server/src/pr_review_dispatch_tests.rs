use super::*;
use liberado_coder_core::pr_review::ObserverIntent;
use liberado_coder_core::{ReviewWorkerConfig, TaskEvent, TaskEventKind, TaskLedger};
use std::collections::BTreeMap;
use std::path::Path;

fn git(root: &Path, args: &[&str]) -> String {
    let output = std_command("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git should start");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output should be UTF-8")
        .trim()
        .to_owned()
}

fn remote_only_pr() -> (tempfile::TempDir, tempfile::TempDir, String, String) {
    let source = tempfile::tempdir().expect("source");
    git(source.path(), &["init"]);
    git(
        source.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    git(source.path(), &["config", "user.name", "Liberado Test"]);
    std::fs::write(source.path().join("review.txt"), "base\n").expect("write base");
    git(source.path(), &["add", "review.txt"]);
    git(source.path(), &["commit", "-m", "base"]);
    let base = git(source.path(), &["rev-parse", "HEAD"]);
    std::fs::write(source.path().join("review.txt"), "base\nhead\n").expect("write head");
    git(source.path(), &["commit", "-am", "head"]);
    let head = git(source.path(), &["rev-parse", "HEAD"]);

    let remote = tempfile::tempdir().expect("remote");
    git(remote.path(), &["init", "--bare"]);
    let remote_url = remote.path().to_string_lossy().into_owned();
    git(
        source.path(),
        &["push", &remote_url, &format!("{base}:refs/heads/main")],
    );
    git(
        source.path(),
        &["push", &remote_url, &format!("{head}:refs/pull/7/head")],
    );

    let local = tempfile::tempdir().expect("local");
    git(local.path(), &["init"]);
    git(
        local.path(),
        &[
            "fetch",
            &remote_url,
            "+refs/heads/main:refs/remotes/origin/main",
        ],
    );
    assert!(!commit_exists(local.path(), &head));
    (remote, local, base, head)
}

fn ledger(task_id: &str) -> TaskLedger {
    TaskLedger::new(TaskEvent::new(
        "evt-created",
        task_id,
        TaskEventKind::TaskCreated {
            objective: "observe".into(),
            acceptance_criteria: vec!["slice2".into()],
            worktree: "/tmp".into(),
            branch: String::new(),
            base_ref: "main".into(),
            repo: Some("owner/repo".into()),
        },
    ))
    .unwrap()
}

#[test]
fn shadow_or_missing_eligible_does_not_require_workers() {
    let mut ledger = ledger("pr-owner-repo-1");
    let workers = BTreeMap::new();
    let intents = vec![ObserverIntent::ObserveTip {
        sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
    }];
    maybe_dispatch(DispatchRequest {
        ledger: &mut ledger,
        task_id: "pr-owner-repo-1",
        repository: "owner/repo",
        pr_number: 1,
        head_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        base_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        base_branch: "main",
        coding_root: Path::new("/tmp"),
        token: "unused",
        review_workers: &workers,
        harness_order: &["codex".into()],
        intents: &intents,
        shadow: true,
    })
    .unwrap();
    assert!(
        ledger
            .events()
            .iter()
            .all(|e| !matches!(e.payload, TaskEventKind::ReviewCommandIssued { .. }))
    );
}

#[test]
fn disabled_codex_does_not_issue() {
    let mut ledger = ledger("pr-owner-repo-1");
    let mut workers = BTreeMap::new();
    workers.insert(
        "codex".into(),
        ReviewWorkerConfig::Codex {
            executable: "/bin/false".into(),
            enabled: false,
        },
    );
    let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let intents = vec![ObserverIntent::Eligible { sha: sha.into() }];
    maybe_dispatch(DispatchRequest {
        ledger: &mut ledger,
        task_id: "pr-owner-repo-1",
        repository: "owner/repo",
        pr_number: 1,
        head_sha: sha,
        base_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        base_branch: "main",
        coding_root: Path::new("/tmp"),
        token: "unused",
        review_workers: &workers,
        harness_order: &["codex".into()],
        intents: &intents,
        shadow: false,
    })
    .unwrap();
    assert!(
        ledger
            .events()
            .iter()
            .all(|e| !matches!(e.payload, TaskEventKind::ReviewCommandIssued { .. }))
    );
}

#[test]
fn dispatch_plan_selects_the_enabled_codex_worker() {
    let mut ledger = ledger("pr-owner-repo-1");
    let mut workers = BTreeMap::new();
    workers.insert(
        "codex".into(),
        ReviewWorkerConfig::Codex {
            executable: "/usr/bin/codex".into(),
            enabled: true,
        },
    );
    let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let intents = vec![ObserverIntent::Eligible { sha: sha.into() }];
    let order = ["codex".into()];
    let request = DispatchRequest {
        ledger: &mut ledger,
        task_id: "pr-owner-repo-1",
        repository: "owner/repo",
        pr_number: 1,
        head_sha: sha,
        base_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        base_branch: "main",
        coding_root: Path::new("/tmp"),
        token: "unused",
        review_workers: &workers,
        harness_order: &order,
        intents: &intents,
        shadow: false,
    };

    let plan = dispatch_plan(&request).expect("eligible dispatch plan");
    assert_eq!(plan.worker_id, "codex");
    assert_eq!(plan.command_id, review_command_id("owner/repo", 1, sha));
    assert_eq!(plan.run_id, plan.command_id);
    assert!(matches!(
        plan.worker,
        ReviewWorkerConfig::Codex {
            executable,
            ..
        } if executable == "/usr/bin/codex"
    ));
}

#[test]
fn exhausted_codex_selects_opencode_next() {
    use liberado_coder_core::OPENCODE_NAMED_REVIEW_MODEL;
    use liberado_coder_core::pr_review::WorkerFailure;
    use liberado_coder_core::pr_review_admission::{admit_review_command, record_review_outcome};
    use liberado_coder_core::pr_review_port::ReviewInvokeOutcome;

    let mut book = ledger("pr-owner-repo-1");
    let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let command = review_command_id("owner/repo", 1, sha);
    admit_review_command(&mut book, "pr-owner-repo-1", &command, sha).unwrap();
    record_review_outcome(
        &mut book,
        "pr-owner-repo-1",
        "codex",
        &command,
        sha,
        Path::new("/unused"),
        ReviewInvokeOutcome::Unavailable {
            failure: WorkerFailure::Exhausted,
        },
    )
    .unwrap();

    let mut workers = BTreeMap::new();
    workers.insert(
        "codex".into(),
        ReviewWorkerConfig::Codex {
            executable: "/usr/bin/codex".into(),
            enabled: true,
        },
    );
    workers.insert(
        "open_code".into(),
        ReviewWorkerConfig::OpenCode {
            executable: "/usr/bin/opencode".into(),
            model: OPENCODE_NAMED_REVIEW_MODEL.into(),
            permission_mode: "deny_writes".into(),
            pricing_policy: "named".into(),
            enabled: true,
        },
    );
    let intents = vec![ObserverIntent::Eligible { sha: sha.into() }];
    let order = ["codex".into(), "open_code".into()];
    let request = DispatchRequest {
        ledger: &mut book,
        task_id: "pr-owner-repo-1",
        repository: "owner/repo",
        pr_number: 1,
        head_sha: sha,
        base_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        base_branch: "main",
        coding_root: Path::new("/tmp"),
        token: "unused",
        review_workers: &workers,
        harness_order: &order,
        intents: &intents,
        shadow: false,
    };
    let plan = dispatch_plan(&request).expect("fallback plan");
    assert_eq!(plan.worker_id, "open_code");
    assert_eq!(plan.run_id, format!("{command}:open_code"));
}

#[test]
fn fetch_setup_failure_does_not_write_the_command_fence() {
    let mut ledger = ledger("pr-owner-repo-1");
    let mut workers = BTreeMap::new();
    workers.insert(
        "codex".into(),
        ReviewWorkerConfig::Codex {
            executable: "/usr/bin/codex".into(),
            enabled: true,
        },
    );
    let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let intents = vec![ObserverIntent::Eligible { sha: sha.into() }];
    let order = ["codex".into()];
    let error = maybe_dispatch(DispatchRequest {
        ledger: &mut ledger,
        task_id: "pr-owner-repo-1",
        repository: "not-an-owner-name-pair",
        pr_number: 1,
        head_sha: sha,
        base_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        base_branch: "main",
        coding_root: Path::new("/tmp"),
        token: "unused",
        review_workers: &workers,
        harness_order: &order,
        intents: &intents,
        shadow: false,
    })
    .expect_err("invalid fetch setup must remain retryable");

    assert!(error.contains("owner/name"));
    assert!(
        ledger
            .events()
            .iter()
            .all(|event| !matches!(event.payload, TaskEventKind::ReviewCommandIssued { .. }))
    );
}

#[test]
fn fetches_a_remote_only_pr_head_and_pins_the_observed_sha() {
    let (remote, local, base, head) = remote_only_pr();
    fetch_pr_commits_from(
        local.path(),
        &remote.path().to_string_lossy(),
        7,
        "main",
        &base,
        &head,
        None,
    )
    .expect("fetch remote PR");

    assert!(commit_exists(local.path(), &base));
    assert!(commit_exists(local.path(), &head));
    let workspace = pin_checkout(local.path(), &head).expect("pin fetched head");
    assert_eq!(
        git_stdout(&workspace, &["rev-parse", "HEAD"])
            .unwrap()
            .trim(),
        head
    );
}

#[test]
fn rejects_a_pr_ref_that_does_not_match_the_observed_sha() {
    let (remote, local, base, _head) = remote_only_pr();
    let stale = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let error = fetch_pr_commits_from(
        local.path(),
        &remote.path().to_string_lossy(),
        7,
        "main",
        &base,
        stale,
        None,
    )
    .expect_err("moving ref must fail closed");

    assert!(error.contains("moved"));
}

#[test]
fn github_fetch_url_accepts_only_an_owner_and_repository_name() {
    assert_eq!(
        github_repository_url("owner/repo").unwrap(),
        "https://github.com/owner/repo.git"
    );
    assert!(github_repository_url("owner/repo/extra").is_err());
    assert!(github_repository_url("owner/repo:other").is_err());
    assert!(github_repository_url("/repo").is_err());
}

#[test]
fn review_port_accepts_only_codex_and_opencode() {
    use liberado_coder_core::OPENCODE_NAMED_REVIEW_MODEL;
    assert!(
        review_port(&ReviewWorkerConfig::Codex {
            executable: "/usr/bin/codex".into(),
            enabled: true,
        })
        .is_ok()
    );
    assert!(
        review_port(&ReviewWorkerConfig::OpenCode {
            executable: "/usr/bin/opencode".into(),
            model: OPENCODE_NAMED_REVIEW_MODEL.into(),
            permission_mode: "deny_writes".into(),
            pricing_policy: "named".into(),
            enabled: true,
        })
        .is_ok()
    );
    assert!(matches!(
        review_port(&ReviewWorkerConfig::GrokBuild {
            executable: "/usr/bin/grok".into(),
            enabled: true,
        }),
        Err(error) if error.contains("adapter")
    ));
}

#[test]
fn planned_review_from_a_local_remote_issues_the_command() {
    let (remote, local, base, head) = remote_only_pr();
    let mut book = ledger("pr-owner-repo-7");
    let mut workers = BTreeMap::new();
    workers.insert(
        "codex".into(),
        ReviewWorkerConfig::Codex {
            executable: "liberado-review-adapter-must-not-exist".into(),
            enabled: true,
        },
    );
    let intents = vec![ObserverIntent::Eligible { sha: head.clone() }];
    let order = ["codex".into()];
    let request = DispatchRequest {
        ledger: &mut book,
        task_id: "pr-owner-repo-7",
        repository: "owner/repo",
        pr_number: 7,
        head_sha: &head,
        base_sha: &base,
        base_branch: "main",
        coding_root: local.path(),
        token: "unused",
        review_workers: &workers,
        harness_order: &order,
        intents: &intents,
        shadow: false,
    };
    let plan = dispatch_plan(&request).expect("eligible plan");
    execute_planned_review(request, plan, &remote.path().to_string_lossy())
        .expect("local fetch and pin");
    assert!(
        book.events()
            .iter()
            .any(|event| matches!(event.payload, TaskEventKind::ReviewCommandIssued { .. }))
    );
}

#[test]
fn fetch_authorization_is_not_a_command_argument() {
    let secret = "Authorization: Basic private-value";
    let mut command = std_command("git");
    command.args(["fetch", "https://github.com/owner/repo.git"]);
    set_fetch_auth(&mut command, Path::new("/repo"), secret);

    assert!(
        command
            .get_args()
            .all(|arg| !arg.to_string_lossy().contains("private-value"))
    );
    assert!(command.get_envs().any(|(name, value)| {
        name == "GIT_CONFIG_VALUE_1" && value.is_some_and(|value| value == secret)
    }));
}
