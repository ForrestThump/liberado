//! Publication orchestration for an observed pull request.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::pr_review_diff::changed_lines;
use crate::pr_review_publish::{GithubReviewPublishPort, expected_login_for};
use liberado_coder_core::pr_review_publish::{PublishRequest, reconcile_publication};
use liberado_coder_core::{TaskEventKind, TaskLedger};
use liberado_config::{ShepherdProjectConfig, Topology};

pub(crate) struct PublishForPrParams<'a> {
    pub(crate) ledger: &'a mut TaskLedger,
    pub(crate) topology: &'a Topology,
    pub(crate) project: &'a ShepherdProjectConfig,
    pub(crate) token: &'a str,
    pub(crate) coding_root: &'a Path,
    pub(crate) task_id: &'a str,
    pub(crate) pr_number: u64,
    pub(crate) shadow: bool,
    pub(crate) changed_lines: &'a BTreeSet<(String, u32)>,
}

pub(crate) struct PublishObservation {
    pub(crate) ledger: TaskLedger,
    pub(crate) topology: Topology,
    pub(crate) project: ShepherdProjectConfig,
    pub(crate) token: String,
    pub(crate) coding_root: PathBuf,
    pub(crate) task_id: String,
    pub(crate) pr_number: u64,
    pub(crate) base_sha: String,
    pub(crate) head_sha: String,
    pub(crate) shadow: bool,
}

pub(crate) async fn publish_for_observation(mut params: PublishObservation) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let changed = if params.shadow || !review_diff_required(&params.ledger, &params.head_sha) {
            BTreeSet::new()
        } else {
            changed_lines(&params.coding_root, &params.base_sha, &params.head_sha)?
        };
        publish_for_pr(PublishForPrParams {
            ledger: &mut params.ledger,
            topology: &params.topology,
            project: &params.project,
            token: &params.token,
            coding_root: &params.coding_root,
            task_id: &params.task_id,
            pr_number: params.pr_number,
            shadow: params.shadow,
            changed_lines: &changed,
        })
    })
    .await
    .map_err(|error| format!("PR review publication task failed: {error}"))?
}

fn review_diff_required(ledger: &TaskLedger, observed_head: &str) -> bool {
    let Some((run_id, run_sha)) =
        ledger
            .events()
            .iter()
            .rev()
            .find_map(|event| match &event.payload {
                TaskEventKind::ReviewRunFinished {
                    run_id, head_sha, ..
                } => Some((run_id.as_str(), head_sha.as_str())),
                _ => None,
            })
    else {
        return false;
    };
    run_sha == observed_head
        && !ledger.events().iter().any(|event| {
            matches!(
                &event.payload,
                TaskEventKind::ReviewPublished { run_id: published, .. } if published == run_id
            )
        })
}

pub(crate) fn publish_for_pr(params: PublishForPrParams<'_>) -> Result<(), String> {
    if params.shadow {
        return Ok(());
    }
    let expected_login = expected_login_for(params.topology, params.project)?;
    let port = GithubReviewPublishPort::new(params.token)?;
    reconcile_publication(
        params.ledger,
        &port,
        &PublishRequest {
            task_id: params.task_id,
            repository: &params.project.repository,
            pr_number: params.pr_number,
            expected_login: &expected_login,
            coding_root: params.coding_root,
            changed_lines: params.changed_lines,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_core::TaskEvent;

    fn review_ledger() -> TaskLedger {
        let mut ledger = TaskLedger::new(TaskEvent::new(
            "created",
            "task",
            TaskEventKind::TaskCreated {
                objective: "review".into(),
                acceptance_criteria: Vec::new(),
                worktree: "/tmp".into(),
                branch: String::new(),
                base_ref: "main".into(),
                repo: Some("owner/repo".into()),
            },
        ))
        .unwrap();
        ledger
            .append(TaskEvent::new(
                "finished",
                "task",
                TaskEventKind::ReviewRunFinished {
                    run_id: "run".into(),
                    worker_id: "codex".into(),
                    head_sha: "head".into(),
                    artifact_digest: "digest".into(),
                },
            ))
            .unwrap();
        ledger
    }

    #[tokio::test]
    async fn blocking_publication_work_runs_off_the_async_worker() {
        let async_thread = std::thread::current().id();
        let blocking_thread = tokio::task::spawn_blocking(|| std::thread::current().id())
            .await
            .unwrap();
        assert_ne!(async_thread, blocking_thread);
        let blocking_call = ["spawn", "blocking"].join("_");
        assert!(include_str!("pr_review_publish_flow.rs").contains(&blocking_call));
    }

    #[test]
    fn production_publisher_loads_changed_lines() {
        let changed_line_call = format!("{}(&params.coding_root", ["changed", "lines"].join("_"));
        assert!(include_str!("pr_review_publish_flow.rs").contains(&changed_line_call));
    }

    #[test]
    fn diff_is_loaded_only_for_an_unpublished_run_on_the_observed_head() {
        let mut ledger = review_ledger();
        assert!(review_diff_required(&ledger, "head"));
        assert!(!review_diff_required(&ledger, "other"));

        ledger
            .append(TaskEvent::new(
                "published",
                "task",
                TaskEventKind::ReviewPublished {
                    head_sha: "head".into(),
                    review_id: 1,
                    login: "bot".into(),
                    command_id: "run".into(),
                    run_id: "run".into(),
                    worker_id: "codex".into(),
                    artifact_digest: "digest".into(),
                    policy_version: "v1".into(),
                    review_key: "key".into(),
                },
            ))
            .unwrap();
        assert!(!review_diff_required(&ledger, "head"));
    }
}
