//! Restart-safe, policy-versioned pull-request publication saga.
//!
//! GitHub I/O stays behind [`ReviewPublishPort`]. Harnesses never see forge credentials.

use std::collections::BTreeSet;
use std::path::Path;

use crate::pr_review::{POLICY_VERSION, ReviewFinding, ReviewResult, review_key};
use crate::{TaskEvent, TaskEventKind, TaskLedger};

/// Locked publication event. Config cannot override this.
pub const REVIEW_EVENT: &str = "COMMENT";
/// Locked blocker action. Config cannot override this.
pub const BLOCKER_ACTION: &str = "draft";
/// Locked synchronize policy. Config cannot override this.
pub const ON_SYNCHRONIZE: &str = "draft_if_armed";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishPr {
    pub head_sha: String,
    pub draft: bool,
    pub open: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineComment {
    pub path: String,
    pub line: u32,
    pub body: String,
}

/// Effect port for GitHub publication. Not part of [`crate::pr_review_port::ReviewPort`].
pub trait ReviewPublishPort {
    fn read_pr(&self, repo: &str, number: u64) -> Result<PublishPr, String>;
    fn authenticated_login(&self) -> Result<String, String>;
    fn find_comment_review_by_marker(
        &self,
        repo: &str,
        number: u64,
        marker: &str,
    ) -> Result<Option<u64>, String>;
    fn create_comment_review(
        &self,
        repo: &str,
        number: u64,
        commit_id: &str,
        body: &str,
        inline: &[InlineComment],
    ) -> Result<u64, String>;
    fn find_issue_comment_by_marker(
        &self,
        repo: &str,
        number: u64,
        marker: &str,
    ) -> Result<Option<u64>, String>;
    fn create_issue_comment(&self, repo: &str, number: u64, body: &str) -> Result<u64, String>;
    fn convert_to_draft(&self, repo: &str, number: u64) -> Result<(), String>;
}

#[derive(Debug, Clone)]
pub struct PublishRequest<'a> {
    pub task_id: &'a str,
    pub repository: &'a str,
    pub pr_number: u64,
    pub expected_login: &'a str,
    pub coding_root: &'a Path,
    pub changed_lines: &'a BTreeSet<(String, u32)>,
}

pub fn review_marker(repo: &str, number: u64, sha: &str) -> String {
    format!(
        "<!-- liberado-review:{} -->",
        review_key(repo, number, sha, POLICY_VERSION)
    )
}

pub fn checklist_marker(repo: &str, number: u64, sha: &str) -> String {
    format!(
        "<!-- liberado-checklist:{} -->",
        review_key(repo, number, sha, POLICY_VERSION)
    )
}

pub fn sync_marker(repo: &str, number: u64, old: &str, new: &str) -> String {
    format!("<!-- liberado-review-sync:{repo}#{number}@{old}->{new}:{POLICY_VERSION} -->")
}

pub fn artifact_path(coding_root: &Path, digest: &str) -> std::path::PathBuf {
    coding_root
        .join(".liberado")
        .join("review-artifacts")
        .join(format!("{digest}.json"))
}

/// Resume only missing publication steps for the latest finished review run.
pub fn reconcile_publication(
    ledger: &mut TaskLedger,
    port: &dyn ReviewPublishPort,
    req: &PublishRequest<'_>,
) -> Result<(), String> {
    let Some(run) = terminal_run(ledger) else {
        return Ok(());
    };
    let result = load_artifact(req.coding_root, &run.digest, &run.sha)?;
    publish_comment_if_needed(ledger, port, req, &run, &result)?;
    publish_blockers_if_needed(ledger, port, req, &run, &result)?;
    execute_synchronize_intents(ledger, port, req)?;
    Ok(())
}

fn publish_comment_if_needed(
    ledger: &mut TaskLedger,
    port: &dyn ReviewPublishPort,
    req: &PublishRequest<'_>,
    run: &Run,
    result: &ReviewResult,
) -> Result<(), String> {
    let key = review_key(req.repository, req.pr_number, &run.sha, POLICY_VERSION);
    if published_review(ledger, &key).is_some() {
        return Ok(());
    }
    guard_ready(port, req, &run.sha)?;
    let marker = review_marker(req.repository, req.pr_number, &run.sha);
    let (body, inline) = render_review(&marker, result, req.changed_lines);
    let id = match port.find_comment_review_by_marker(req.repository, req.pr_number, &marker)? {
        Some(id) => id,
        None => {
            port.create_comment_review(req.repository, req.pr_number, &run.sha, &body, &inline)?
        }
    };
    append(
        ledger,
        req.task_id,
        format!("publish:{key}"),
        TaskEventKind::ReviewPublished {
            head_sha: run.sha.clone(),
            review_id: id,
            login: req.expected_login.into(),
            command_id: run.run_id.clone(),
            run_id: run.run_id.clone(),
            worker_id: run.worker.clone(),
            artifact_digest: run.digest.clone(),
            policy_version: POLICY_VERSION.into(),
            review_key: key,
        },
    )
}

fn publish_blockers_if_needed(
    ledger: &mut TaskLedger,
    port: &dyn ReviewPublishPort,
    req: &PublishRequest<'_>,
    run: &Run,
    result: &ReviewResult,
) -> Result<(), String> {
    let blockers: Vec<_> = result.findings.iter().filter(|f| f.blocker).collect();
    if blockers.is_empty() {
        return Ok(());
    }
    if !has_checklist(ledger, &run.sha) {
        guard_ready(port, req, &run.sha)?;
        let marker = checklist_marker(req.repository, req.pr_number, &run.sha);
        let body = render_checklist(&marker, &blockers);
        let id = match port.find_issue_comment_by_marker(req.repository, req.pr_number, &marker)? {
            Some(id) => id,
            None => port.create_issue_comment(req.repository, req.pr_number, &body)?,
        };
        append(
            ledger,
            req.task_id,
            format!(
                "checklist:{}",
                review_key(req.repository, req.pr_number, &run.sha, POLICY_VERSION)
            ),
            TaskEventKind::ReviewChecklistPublished {
                head_sha: run.sha.clone(),
                comment_id: id,
            },
        )?;
    }
    if !has_draft(ledger, &run.sha) {
        guard_ready(port, req, &run.sha)?;
        port.convert_to_draft(req.repository, req.pr_number)?;
        append(
            ledger,
            req.task_id,
            format!(
                "draft:{}",
                review_key(req.repository, req.pr_number, &run.sha, POLICY_VERSION)
            ),
            TaskEventKind::ReviewDraftConverted {
                head_sha: run.sha.clone(),
            },
        )?;
    }
    Ok(())
}

fn execute_synchronize_intents(
    ledger: &mut TaskLedger,
    port: &dyn ReviewPublishPort,
    req: &PublishRequest<'_>,
) -> Result<(), String> {
    for (old_sha, new_sha) in synchronize_intents(ledger) {
        execute_synchronize_intent(ledger, port, req, &old_sha, &new_sha)?;
    }
    Ok(())
}

fn synchronize_intents(ledger: &TaskLedger) -> Vec<(String, String)> {
    ledger
        .events()
        .iter()
        .filter_map(|event| match &event.payload {
            TaskEventKind::ReviewSynchronizeIntent { old_sha, new_sha } => {
                Some((old_sha.clone(), new_sha.clone()))
            }
            _ => None,
        })
        .collect()
}

fn execute_synchronize_intent(
    ledger: &mut TaskLedger,
    port: &dyn ReviewPublishPort,
    req: &PublishRequest<'_>,
    old_sha: &str,
    new_sha: &str,
) -> Result<(), String> {
    let command = format!("sync-done:{}:{old_sha}:{new_sha}", req.pr_number);
    if command_recorded(ledger, &command) {
        return Ok(());
    }
    let Some(draft) = guard_synchronize(port, req, new_sha)? else {
        return Ok(());
    };
    ensure_synchronize_draft(port, req, draft)?;
    ensure_synchronize_comment(port, req, old_sha, new_sha)?;
    append(
        ledger,
        req.task_id,
        command,
        TaskEventKind::ReviewDraftConverted {
            head_sha: new_sha.to_owned(),
        },
    )
}

fn command_recorded(ledger: &TaskLedger, command: &str) -> bool {
    ledger
        .events()
        .iter()
        .any(|event| event.command_id.as_deref() == Some(command))
}

fn guard_synchronize(
    port: &dyn ReviewPublishPort,
    req: &PublishRequest<'_>,
    new_sha: &str,
) -> Result<Option<bool>, String> {
    if port.authenticated_login()? != req.expected_login {
        return Err("publication guard failed: login mismatch".into());
    }
    let pr = port.read_pr(req.repository, req.pr_number)?;
    if !pr.open || pr.head_sha != new_sha {
        return Ok(None);
    }
    Ok(Some(pr.draft))
}

fn ensure_synchronize_draft(
    port: &dyn ReviewPublishPort,
    req: &PublishRequest<'_>,
    draft: bool,
) -> Result<(), String> {
    if draft {
        Ok(())
    } else {
        port.convert_to_draft(req.repository, req.pr_number)
    }
}

fn ensure_synchronize_comment(
    port: &dyn ReviewPublishPort,
    req: &PublishRequest<'_>,
    old_sha: &str,
    new_sha: &str,
) -> Result<(), String> {
    let marker = sync_marker(req.repository, req.pr_number, old_sha, new_sha);
    if port
        .find_issue_comment_by_marker(req.repository, req.pr_number, &marker)?
        .is_some()
    {
        return Ok(());
    }
    let body =
        format!("{marker}\n\nHead moved `{old_sha}` → `{new_sha}`. Prior review cycle superseded.");
    port.create_issue_comment(req.repository, req.pr_number, &body)
        .map(|_| ())
}

#[derive(Debug, Clone)]
struct Run {
    run_id: String,
    worker: String,
    sha: String,
    digest: String,
}

fn terminal_run(ledger: &TaskLedger) -> Option<Run> {
    ledger
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            TaskEventKind::ReviewRunFinished {
                run_id,
                worker_id,
                head_sha,
                artifact_digest,
            } => Some(Run {
                run_id: run_id.clone(),
                worker: worker_id.clone(),
                sha: head_sha.clone(),
                digest: artifact_digest.clone(),
            }),
            _ => None,
        })
}

fn load_artifact(root: &Path, digest: &str, sha: &str) -> Result<ReviewResult, String> {
    let bytes = std::fs::read(artifact_path(root, digest)).map_err(|e| e.to_string())?;
    if crate::pr_review_port::artifact_digest(&bytes) != digest {
        return Err("review artifact digest mismatch".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "review artifact is not UTF-8")?;
    ReviewResult::parse_success(text, sha).map_err(|e| e.to_string())
}

fn guard_ready(
    port: &dyn ReviewPublishPort,
    req: &PublishRequest<'_>,
    sha: &str,
) -> Result<(), String> {
    let pr = port.read_pr(req.repository, req.pr_number)?;
    let login = port.authenticated_login()?;
    if !pr.open || pr.draft || pr.head_sha != sha || login != req.expected_login {
        return Err("publication guard failed".into());
    }
    Ok(())
}

fn published_review(ledger: &TaskLedger, key: &str) -> Option<u64> {
    ledger
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            TaskEventKind::ReviewPublished {
                review_id,
                review_key,
                head_sha,
                policy_version,
                ..
            } if review_key == key
                || (review_key.is_empty()
                    && (policy_version.is_empty() || policy_version == POLICY_VERSION)
                    && key.contains(head_sha.as_str())) =>
            {
                Some(*review_id)
            }
            _ => None,
        })
}

fn has_checklist(ledger: &TaskLedger, sha: &str) -> bool {
    ledger.events().iter().any(|event| {
        matches!(
            &event.payload,
            TaskEventKind::ReviewChecklistPublished { head_sha, .. } if head_sha == sha
        )
    })
}

fn has_draft(ledger: &TaskLedger, sha: &str) -> bool {
    ledger.events().iter().any(|event| {
        matches!(
            &event.payload,
            TaskEventKind::ReviewDraftConverted { head_sha } if head_sha == sha
        )
    })
}

pub fn render_review(
    marker: &str,
    result: &ReviewResult,
    changed: &BTreeSet<(String, u32)>,
) -> (String, Vec<InlineComment>) {
    let mut body = format!("{marker}\n\n{}", result.summary);
    let mut inline = Vec::new();
    for finding in &result.findings {
        if let Some(line) = finding
            .line
            .filter(|line| changed.contains(&(finding.path.clone(), *line)))
        {
            inline.push(InlineComment {
                path: finding.path.clone(),
                line,
                body: format!("`{}`: {}", finding.id, finding.explanation),
            });
        } else {
            body.push_str(&format!(
                "\n\n- `{}` [{}]: {}",
                finding.path, finding.id, finding.explanation
            ));
        }
    }
    (body, inline)
}

pub fn render_checklist(marker: &str, blockers: &[&ReviewFinding]) -> String {
    let mut body = format!("{marker}\n\nBlocking review findings:");
    for finding in blockers {
        body.push_str(&format!(
            "\n- [ ] `{}` — {}",
            finding.id, finding.explanation
        ));
    }
    body
}

fn append(
    ledger: &mut TaskLedger,
    task_id: &str,
    command: String,
    kind: TaskEventKind,
) -> Result<(), String> {
    ledger
        .append(TaskEvent::new(format!("evt-{command}"), task_id, kind).with_command_id(command))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "pr_review_publish_tests.rs"]
mod tests;
