use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use super::*;
use crate::pr_review::{
    POLICY_VERSION, REVIEW_SCHEMA_VERSION, ReviewFinding, ReviewResult, ReviewSeverity, review_key,
};
use crate::pr_review_port::{artifact_digest, serialize_review_result};
use crate::{TaskEvent, TaskEventKind, TaskLedger};

struct MockPort {
    login: String,
    pr: RefCell<PublishPr>,
    reviews: RefCell<HashMap<String, u64>>,
    comments: RefCell<HashMap<String, u64>>,
    created_reviews: RefCell<u64>,
    created_comments: RefCell<u64>,
    drafts: RefCell<u64>,
    writes_blocked: RefCell<bool>,
}

impl Default for MockPort {
    fn default() -> Self {
        Self {
            login: String::new(),
            pr: RefCell::new(PublishPr {
                head_sha: String::new(),
                draft: false,
                open: false,
            }),
            reviews: RefCell::new(HashMap::new()),
            comments: RefCell::new(HashMap::new()),
            created_reviews: RefCell::new(0),
            created_comments: RefCell::new(0),
            drafts: RefCell::new(0),
            writes_blocked: RefCell::new(false),
        }
    }
}

impl ReviewPublishPort for MockPort {
    fn read_pr(&self, _repo: &str, _number: u64) -> Result<PublishPr, String> {
        Ok(self.pr.borrow().clone())
    }
    fn authenticated_login(&self) -> Result<String, String> {
        Ok(self.login.clone())
    }
    fn find_comment_review_by_marker(
        &self,
        _repo: &str,
        _number: u64,
        marker: &str,
    ) -> Result<Option<u64>, String> {
        Ok(self.reviews.borrow().get(marker).copied())
    }
    fn create_comment_review(
        &self,
        _repo: &str,
        _number: u64,
        _commit_id: &str,
        body: &str,
        _inline: &[InlineComment],
    ) -> Result<u64, String> {
        if *self.writes_blocked.borrow() {
            return Err("write blocked".into());
        }
        assert!(body.contains("<!-- liberado-review:"));
        assert!(!body.contains("APPROVE"));
        let id = 100 + *self.created_reviews.borrow();
        *self.created_reviews.borrow_mut() += 1;
        let marker = body.lines().next().unwrap().to_string();
        self.reviews.borrow_mut().insert(marker, id);
        Ok(id)
    }
    fn find_issue_comment_by_marker(
        &self,
        _repo: &str,
        _number: u64,
        marker: &str,
    ) -> Result<Option<u64>, String> {
        Ok(self.comments.borrow().get(marker).copied())
    }
    fn create_issue_comment(&self, _repo: &str, _number: u64, body: &str) -> Result<u64, String> {
        if *self.writes_blocked.borrow() {
            return Err("write blocked".into());
        }
        let id = 200 + *self.created_comments.borrow();
        *self.created_comments.borrow_mut() += 1;
        let marker = body.lines().next().unwrap().to_string();
        self.comments.borrow_mut().insert(marker, id);
        Ok(id)
    }
    fn convert_to_draft(&self, _repo: &str, _number: u64) -> Result<(), String> {
        if *self.writes_blocked.borrow() {
            return Err("write blocked".into());
        }
        *self.drafts.borrow_mut() += 1;
        self.pr.borrow_mut().draft = true;
        Ok(())
    }
}

fn sha(n: u8) -> String {
    format!("{:040x}", n as u128)
}

fn result(sha: &str, blocker: bool, in_diff: bool) -> ReviewResult {
    ReviewResult {
        schema: REVIEW_SCHEMA_VERSION.into(),
        reviewed_sha: sha.into(),
        summary: "summary".into(),
        findings: vec![ReviewFinding {
            id: "F1".into(),
            severity: ReviewSeverity::High,
            blocker,
            path: "src/lib.rs".into(),
            line: Some(if in_diff { 10 } else { 99 }),
            explanation: "problem".into(),
            check: None,
        }],
    }
}

fn ledger_with_finished(root: &Path, sha: &str, result: &ReviewResult) -> (TaskLedger, String) {
    let bytes = serialize_review_result(result).unwrap();
    let digest = artifact_digest(&bytes);
    std::fs::create_dir_all(root.join(".liberado/review-artifacts")).unwrap();
    std::fs::write(artifact_path(root, &digest), &bytes).unwrap();
    let task = "task-1";
    let created = TaskEvent::new(
        "evt-created",
        task,
        TaskEventKind::TaskCreated {
            objective: "o".into(),
            acceptance_criteria: vec![],
            worktree: root.display().to_string(),
            branch: "b".into(),
            base_ref: "main".into(),
            repo: Some("o/r".into()),
        },
    )
    .with_command_id("create");
    let mut ledger = TaskLedger::create_in(root, created).unwrap();
    ledger
        .append(
            TaskEvent::new(
                "evt-finished",
                task,
                TaskEventKind::ReviewRunFinished {
                    run_id: "run-1".into(),
                    worker_id: "codex".into(),
                    head_sha: sha.into(),
                    artifact_digest: digest.clone(),
                },
            )
            .with_command_id("run-1:finished"),
        )
        .unwrap();
    (ledger, digest)
}

fn req<'a>(
    root: &'a Path,
    login: &'a str,
    changed: &'a BTreeSet<(String, u32)>,
) -> PublishRequest<'a> {
    PublishRequest {
        task_id: "task-1",
        repository: "o/r",
        pr_number: 7,
        expected_login: login,
        coding_root: root,
        changed_lines: changed,
    }
}

#[test]
fn login_mismatch_performs_no_write() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let sha = sha(1);
    let review = result(&sha, true, true);
    let (mut ledger, _) = ledger_with_finished(&root, &sha, &review);
    let port = MockPort {
        login: "other".into(),
        pr: RefCell::new(PublishPr {
            head_sha: sha.clone(),
            draft: false,
            open: true,
        }),
        ..MockPort::default()
    };
    let changed = BTreeSet::from([("src/lib.rs".into(), 10)]);
    let err = reconcile_publication(&mut ledger, &port, &req(&root, "me", &changed)).unwrap_err();
    assert!(err.contains("guard"));
    assert_eq!(*port.created_reviews.borrow(), 0);
    assert_eq!(*port.created_comments.borrow(), 0);
    assert_eq!(*port.drafts.borrow(), 0);
}

#[test]
fn out_of_diff_goes_to_body_and_only_comment_event() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let sha = sha(2);
    let review = result(&sha, false, false);
    let (mut ledger, _) = ledger_with_finished(&root, &sha, &review);
    let port = MockPort {
        login: "me".into(),
        pr: RefCell::new(PublishPr {
            head_sha: sha.clone(),
            draft: false,
            open: true,
        }),
        ..MockPort::default()
    };
    let changed = BTreeSet::new();
    reconcile_publication(&mut ledger, &port, &req(&root, "me", &changed)).unwrap();
    assert_eq!(*port.created_reviews.borrow(), 1);
    assert_eq!(*port.created_comments.borrow(), 0);
    assert_eq!(*port.drafts.borrow(), 0);
    let body_marker = review_marker("o/r", 7, &sha);
    let stored = port.reviews.borrow().get(&body_marker).copied();
    assert_eq!(stored, Some(100));
}

#[test]
fn blockers_create_one_checklist_and_draft() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let sha = sha(3);
    let review = result(&sha, true, true);
    let (mut ledger, _) = ledger_with_finished(&root, &sha, &review);
    let port = MockPort {
        login: "me".into(),
        pr: RefCell::new(PublishPr {
            head_sha: sha.clone(),
            draft: false,
            open: true,
        }),
        ..MockPort::default()
    };
    let changed = BTreeSet::from([("src/lib.rs".into(), 10)]);
    reconcile_publication(&mut ledger, &port, &req(&root, "me", &changed)).unwrap();
    assert_eq!(*port.created_reviews.borrow(), 1);
    assert_eq!(*port.created_comments.borrow(), 1);
    assert_eq!(*port.drafts.borrow(), 1);
    // Resume is a no-op.
    reconcile_publication(&mut ledger, &port, &req(&root, "me", &changed)).unwrap();
    assert_eq!(*port.created_reviews.borrow(), 1);
    assert_eq!(*port.created_comments.borrow(), 1);
    assert_eq!(*port.drafts.borrow(), 1);
}

#[test]
fn crash_after_github_success_finds_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let sha = sha(4);
    let review = result(&sha, false, true);
    let (mut ledger, _) = ledger_with_finished(&root, &sha, &review);
    let marker = review_marker("o/r", 7, &sha);
    let port = MockPort {
        login: "me".into(),
        pr: RefCell::new(PublishPr {
            head_sha: sha.clone(),
            draft: false,
            open: true,
        }),
        reviews: RefCell::new(HashMap::from([(marker, 42)])),
        ..MockPort::default()
    };
    let changed = BTreeSet::from([("src/lib.rs".into(), 10)]);
    reconcile_publication(&mut ledger, &port, &req(&root, "me", &changed)).unwrap();
    assert_eq!(*port.created_reviews.borrow(), 0);
    let expected = review_key("o/r", 7, &sha, POLICY_VERSION);
    assert!(ledger.events().iter().any(|e| match &e.payload {
        TaskEventKind::ReviewPublished {
            review_id: 42,
            review_key: key,
            ..
        } => key == &expected,
        _ => false,
    }));
}

#[test]
fn constants_lock_comment_draft_sync() {
    assert_eq!(REVIEW_EVENT, "COMMENT");
    assert_eq!(BLOCKER_ACTION, "draft");
    assert_eq!(ON_SYNCHRONIZE, "draft_if_armed");
}

#[test]
fn policy_version_separates_keys() {
    let a = review_key("o/r", 1, &sha(5), "daemon-pr-review-v1");
    let b = review_key("o/r", 1, &sha(5), "daemon-pr-review-v2");
    assert_ne!(a, b);
    assert!(review_marker("o/r", 1, &sha(5)).contains("daemon-pr-review-v1"));
}

fn ledger_with_sync(old_sha: &str, new_sha: &str) -> TaskLedger {
    let mut ledger = TaskLedger::new(TaskEvent::new(
        "evt-created",
        "task-1",
        TaskEventKind::TaskCreated {
            objective: "o".into(),
            acceptance_criteria: vec![],
            worktree: "/tmp".into(),
            branch: "b".into(),
            base_ref: "main".into(),
            repo: Some("o/r".into()),
        },
    ))
    .unwrap();
    append(
        &mut ledger,
        "task-1",
        format!("sync:{old_sha}:{new_sha}"),
        TaskEventKind::ReviewSynchronizeIntent {
            old_sha: old_sha.into(),
            new_sha: new_sha.into(),
        },
    )
    .unwrap();
    ledger
}

#[test]
fn synchronize_intent_drafts_notes_and_fences_once() {
    let tmp = tempfile::tempdir().unwrap();
    let old_sha = sha(6);
    let new_sha = sha(7);
    let mut ledger = ledger_with_sync(&old_sha, &new_sha);
    let port = MockPort {
        login: "me".into(),
        pr: RefCell::new(PublishPr {
            head_sha: new_sha.clone(),
            draft: false,
            open: true,
        }),
        ..MockPort::default()
    };
    let changed = BTreeSet::new();
    let request = req(tmp.path(), "me", &changed);

    execute_synchronize_intents(&mut ledger, &port, &request).unwrap();
    execute_synchronize_intents(&mut ledger, &port, &request).unwrap();

    assert_eq!(*port.drafts.borrow(), 1);
    assert_eq!(*port.created_comments.borrow(), 1);
}

#[test]
fn synchronize_intent_skips_a_stale_target() {
    let tmp = tempfile::tempdir().unwrap();
    let old_sha = sha(8);
    let new_sha = sha(9);
    let mut ledger = ledger_with_sync(&old_sha, &new_sha);
    let port = MockPort {
        login: "me".into(),
        pr: RefCell::new(PublishPr {
            head_sha: sha(10),
            draft: false,
            open: true,
        }),
        ..MockPort::default()
    };
    let changed = BTreeSet::new();

    execute_synchronize_intents(&mut ledger, &port, &req(tmp.path(), "me", &changed)).unwrap();

    assert_eq!(*port.drafts.borrow(), 0);
    assert_eq!(*port.created_comments.borrow(), 0);
}

#[test]
fn synchronize_intent_rejects_a_login_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let old_sha = sha(11);
    let new_sha = sha(12);
    let mut ledger = ledger_with_sync(&old_sha, &new_sha);
    let port = MockPort {
        login: "other".into(),
        pr: RefCell::new(PublishPr {
            head_sha: new_sha,
            draft: true,
            open: true,
        }),
        ..MockPort::default()
    };
    let changed = BTreeSet::new();

    let error = execute_synchronize_intents(&mut ledger, &port, &req(tmp.path(), "me", &changed))
        .unwrap_err();

    assert!(error.contains("login mismatch"));
}
