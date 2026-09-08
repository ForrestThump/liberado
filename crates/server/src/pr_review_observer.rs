//! Bounded, read-only GitHub polling for daemon-native PR review.

use crate::pr_review_dispatch::{DispatchRequest, maybe_dispatch};
use liberado_coder_core::ReviewWorkerConfig;
use liberado_coder_core::pr_review::{
    CycleFact, ObserverIntent, PullRequestSnapshot, ReviewCycle, ReviewPolicy, ShaChecks,
    bounded_pages, check_endpoints, collect_github_checks, controller_lease_required,
    pull_request_snapshot, ready_for_review_commit, reconcile_snapshot, review_cycle,
    slice01_records,
};
use liberado_coder_core::{
    TaskEvent, TaskEventKind, TaskLedger, shepherd_task_id, tasks_root_from_worktree,
};
use liberado_config::{ShepherdProjectConfig, Topology};
use reqwest::{
    Client,
    header::{ACCEPT, AUTHORIZATION, USER_AGENT},
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::{path::Path, path::PathBuf};

pub(crate) use crate::pr_review_poll::spawn;

pub(super) async fn observe_pr(
    client: &Client,
    topology: &Topology,
    project: &ShepherdProjectConfig,
    token: &str,
    row: &Value,
    review_workers: &BTreeMap<String, ReviewWorkerConfig>,
) -> Result<(), String> {
    let prepared = prepare_observation(topology, project, row)?;
    finish_observation(
        client,
        topology,
        project,
        token,
        prepared,
        review_workers,
        row,
    )
    .await
}

struct PreparedObservation {
    pr: PullRequestSnapshot,
    task_id: String,
    policy: ReviewPolicy,
    ledger: TaskLedger,
    cycle: ReviewCycle,
    prior_tip: Option<String>,
}

fn prepare_observation(
    topology: &Topology,
    project: &ShepherdProjectConfig,
    row: &Value,
) -> Result<PreparedObservation, String> {
    let pr = pull_request_snapshot(&project.repository, row)
        .ok_or_else(|| "pull request snapshot is incomplete".to_string())?;
    let task_id = shepherd_task_id(Some(&project.repository), pr.number);
    let root = topology
        .projects
        .iter()
        .find(|item| item.name == project.coding_project)
        .map(|item| item.root.as_path())
        .ok_or("coding project root is missing")?;
    let policy = review_policy(project);
    let ledger = open_ledger(root, &policy, project, pr.number, &task_id)?;
    let cycle = cycle_from_ledger(&ledger);
    let prior_tip = last_observed_tip(&ledger);
    Ok(PreparedObservation {
        pr,
        task_id,
        policy,
        ledger,
        cycle,
        prior_tip,
    })
}

async fn finish_observation(
    client: &Client,
    topology: &Topology,
    project: &ShepherdProjectConfig,
    token: &str,
    mut prepared: PreparedObservation,
    review_workers: &BTreeMap<String, ReviewWorkerConfig>,
    row: &Value,
) -> Result<(), String> {
    let ready = marked_ready(
        client,
        topology,
        project,
        token,
        &prepared.pr.head_sha,
        prepared.pr.number,
    )
    .await?;
    let checks = fetch_checks(client, token, &project.repository, &prepared.pr.head_sha).await?;
    let intents = reconcile_snapshot(
        &prepared.policy,
        &prepared.pr,
        &prepared.cycle,
        &checks,
        prepared.prior_tip.as_deref(),
        ready,
    );
    record_and_dispatch(
        topology,
        project,
        &mut prepared,
        row,
        &intents,
        review_workers,
    )
}

fn record_and_dispatch(
    topology: &Topology,
    project: &ShepherdProjectConfig,
    prepared: &mut PreparedObservation,
    row: &Value,
    intents: &[ObserverIntent],
    review_workers: &BTreeMap<String, ReviewWorkerConfig>,
) -> Result<(), String> {
    record_intents(
        &mut prepared.ledger,
        &prepared.task_id,
        &project.repository,
        prepared.pr.number,
        intents,
    )?;
    let coding_root = coding_root(topology, project)?;
    let base_sha = row
        .pointer("/base/sha")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    maybe_dispatch(DispatchRequest {
        ledger: &mut prepared.ledger,
        task_id: &prepared.task_id,
        repository: &project.repository,
        pr_number: prepared.pr.number,
        head_sha: &prepared.pr.head_sha,
        base_sha: &base_sha,
        coding_root: &coding_root,
        review_workers,
        intents,
        shadow: prepared.policy.shadow,
    })
}

fn coding_root(topology: &Topology, project: &ShepherdProjectConfig) -> Result<PathBuf, String> {
    topology
        .projects
        .iter()
        .find(|item| item.name == project.coding_project)
        .map(|item| item.root.clone())
        .ok_or_else(|| "coding project root is missing".into())
}

pub(crate) fn review_policy(project: &ShepherdProjectConfig) -> ReviewPolicy {
    ReviewPolicy {
        controller: project.controller.clone().unwrap_or_default(),
        check_names: project.check_names.clone(),
        shadow: project.controller.as_deref() != Some("liberado-shepherd"),
    }
}

pub(crate) fn cycle_from_ledger(ledger: &TaskLedger) -> ReviewCycle {
    review_cycle(
        ledger
            .events()
            .iter()
            .filter_map(|event| cycle_fact(&event.payload)),
    )
}

fn last_observed_tip(ledger: &TaskLedger) -> Option<String> {
    ledger
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            TaskEventKind::HeadRevisionObserved { sha } => Some(sha.clone()),
            _ => None,
        })
}

pub(crate) fn cycle_fact(kind: &TaskEventKind) -> Option<CycleFact> {
    match kind {
        TaskEventKind::ReadyArmed { head_sha, .. } => Some(CycleFact::Armed {
            sha: head_sha.clone(),
        }),
        TaskEventKind::ReviewDraftObserved { .. } | TaskEventKind::ReviewDraftConverted { .. } => {
            Some(CycleFact::Draft)
        }
        TaskEventKind::PullRequestClosed { .. } => Some(CycleFact::Closed),
        TaskEventKind::ReviewSynchronizeIntent { .. } => Some(CycleFact::Synchronized),
        TaskEventKind::ReviewPublished { head_sha, .. } => Some(CycleFact::Published {
            sha: head_sha.clone(),
        }),
        _ => None,
    }
}

async fn marked_ready(
    client: &Client,
    topology: &Topology,
    project: &ShepherdProjectConfig,
    token: &str,
    sha: &str,
    number: u64,
) -> Result<bool, String> {
    for page in bounded_pages(topology.shepherd.review.max_pages) {
        if let Some(found) =
            scan_outcome(ready_page(client, topology, project, token, sha, number, page).await?)
        {
            return Ok(found);
        }
    }
    Ok(false)
}

struct ReadyScan {
    found: bool,
    more: bool,
}

fn scan_outcome(scan: ReadyScan) -> Option<bool> {
    scan.found.then_some(true).or((!scan.more).then_some(false))
}

async fn ready_page(
    client: &Client,
    topology: &Topology,
    project: &ShepherdProjectConfig,
    token: &str,
    sha: &str,
    number: u64,
    page: usize,
) -> Result<ReadyScan, String> {
    let path = format!(
        "/repos/{}/issues/{number}/events?per_page={}&page={page}",
        project.repository, topology.shepherd.review.page_size
    );
    let value = github(client, token, &path).await?;
    let rows = value
        .as_array()
        .ok_or_else(|| "pull event list was not an array".to_string())?;
    Ok(ReadyScan {
        found: rows
            .iter()
            .any(|row| ready_for_review_commit(row) == Some(sha)),
        more: rows.len() >= topology.shepherd.review.page_size,
    })
}

fn open_ledger(
    root: &Path,
    policy: &ReviewPolicy,
    project: &ShepherdProjectConfig,
    number: u64,
    task_id: &str,
) -> Result<TaskLedger, String> {
    let created = TaskEvent::new(
        format!("evt-{task_id}-created"),
        task_id,
        TaskEventKind::TaskCreated {
            objective: format!("Observe pull request #{number}"),
            acceptance_criteria: vec![
                "Record review eligibility without dispatch or GitHub writes".into(),
            ],
            worktree: root.display().to_string(),
            branch: String::new(),
            base_ref: project.base_branch.clone(),
            repo: Some(project.repository.clone()),
        },
    )
    .with_command_id(format!("create:{task_id}"));
    let mut ledger = TaskLedger::create_in(tasks_root_from_worktree(root), created)
        .map_err(|e| e.to_string())?;
    if controller_lease_required(policy) {
        append(
            &mut ledger,
            task_id,
            format!("review-lease:{number}:liberado-shepherd"),
            TaskEventKind::ControllerLeaseClaimed {
                controller: "liberado-shepherd".into(),
            },
        )?;
    }
    Ok(ledger)
}

pub(crate) fn record_intents(
    ledger: &mut TaskLedger,
    task_id: &str,
    repository: &str,
    number: u64,
    intents: &[ObserverIntent],
) -> Result<(), String> {
    for intent in intents.iter().filter(|intent| slice01_records(intent)) {
        let Some((command, kind)) = intent_event(repository, number, intent) else {
            continue;
        };
        append(ledger, task_id, command, kind)?;
    }
    Ok(())
}

pub(crate) fn intent_event(
    repository: &str,
    number: u64,
    intent: &ObserverIntent,
) -> Option<(String, TaskEventKind)> {
    Some(match intent {
        ObserverIntent::Arm { sha } => (
            format!("review-arm:{number}:{sha}"),
            TaskEventKind::ReadyArmed {
                repository: repository.into(),
                pr_number: number,
                head_sha: sha.clone(),
                source: "ready_for_review".into(),
            },
        ),
        ObserverIntent::ObserveTip { sha } => (
            format!("tip:{number}:{sha}"),
            TaskEventKind::HeadRevisionObserved { sha: sha.clone() },
        ),
        ObserverIntent::ObserveCi { sha, passed } => (
            format!("review-ci:{number}:{sha}:{passed}"),
            TaskEventKind::CiObserved {
                github_run_id: None,
                head_sha: Some(sha.clone()),
                state: if *passed { "success" } else { "pending" }.into(),
                failures: Vec::new(),
            },
        ),
        ObserverIntent::ObserveDraft { sha } => (
            format!("review-draft:{number}:{sha}"),
            TaskEventKind::ReviewDraftObserved {
                head_sha: sha.clone(),
            },
        ),
        ObserverIntent::SynchronizeDraftAndNote { old_sha, new_sha } => (
            format!("review-sync:{number}:{old_sha}:{new_sha}"),
            TaskEventKind::ReviewSynchronizeIntent {
                old_sha: old_sha.clone(),
                new_sha: new_sha.clone(),
            },
        ),
        ObserverIntent::Close { sha } => (
            format!("review-close:{number}:{sha}"),
            TaskEventKind::PullRequestClosed {
                head_sha: sha.clone(),
            },
        ),
        ObserverIntent::Eligible { .. } => return None,
    })
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

async fn fetch_checks(
    client: &Client,
    token: &str,
    repository: &str,
    sha: &str,
) -> Result<ShaChecks, String> {
    let mut checks = Vec::new();
    for endpoint in check_endpoints(repository, sha) {
        let value = github(client, token, &endpoint).await?;
        checks.extend(collect_github_checks(&value));
    }
    Ok(ShaChecks {
        sha: sha.into(),
        checks,
    })
}

pub(super) async fn github(client: &Client, token: &str, path: &str) -> Result<Value, String> {
    client
        .get(format!("https://api.github.com{path}"))
        .header(USER_AGENT, "liberado-pr-review-observer")
        .header(ACCEPT, "application/vnd.github+json")
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "pr_review_observer_tests.rs"]
mod pr_review_observer_tests;
