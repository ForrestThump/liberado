//! Bounded, read-only GitHub polling for daemon-native PR review.

use liberado_coder_core::pr_review::{
    CycleFact, ObserverIntent, ReviewCycle, ReviewPolicy, ShaChecks, bounded_pages,
    check_endpoints, collect_github_checks, controller_lease_required, pull_request_snapshot,
    ready_for_review_commit, reconcile_snapshot, review_cycle, slice01_records,
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
use std::{path::Path, time::Duration};

pub(crate) fn spawn(topology: &Topology) -> Option<tokio::task::JoinHandle<()>> {
    let review = &topology.shepherd.review;
    if !review.enabled {
        return None;
    }
    let topology = topology.clone();
    Some(tokio::spawn(async move {
        let mut ticker =
            tokio::time::interval(Duration::from_secs(topology.shepherd.review.poll_seconds));
        if !topology.shepherd.review.reconcile_on_start {
            ticker.tick().await;
        }
        loop {
            ticker.tick().await;
            if let Err(error) = reconcile(&topology).await {
                tracing::warn!(%error, "PR review observer pass failed");
            }
        }
    }))
}

async fn reconcile(topology: &Topology) -> Result<(), String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    for project in &topology.shepherd.projects {
        if project.controller.is_none() {
            continue;
        }
        let token = token_for(topology, project)?;
        poll_project(&client, topology, project, &token).await?;
    }
    Ok(())
}

fn token_for(topology: &Topology, project: &ShepherdProjectConfig) -> Result<String, String> {
    let name = project
        .auth
        .as_deref()
        .ok_or_else(|| format!("{} has no shepherd auth", project.name))?;
    let auth = topology
        .shepherd
        .auth
        .iter()
        .find(|auth| auth.name == name)
        .ok_or_else(|| format!("{} names unknown shepherd auth {name}", project.name))?;
    std::env::var(&auth.token_ref).map_err(|_| format!("{} is not set", auth.token_ref))
}

async fn poll_project(
    client: &Client,
    topology: &Topology,
    project: &ShepherdProjectConfig,
    token: &str,
) -> Result<(), String> {
    let review = &topology.shepherd.review;
    for page in bounded_pages(review.max_pages) {
        let path = format!(
            "/repos/{}/pulls?state=all&sort=updated&direction=desc&per_page={}&page={page}",
            project.repository, review.page_size
        );
        let rows = github(client, token, &path).await?;
        let Some(rows) = rows.as_array() else {
            return Err("pull list was not an array".into());
        };
        for row in rows {
            observe_pr(client, topology, project, token, row).await?;
        }
        if rows.len() < review.page_size {
            break;
        }
    }
    Ok(())
}

async fn observe_pr(
    client: &Client,
    topology: &Topology,
    project: &ShepherdProjectConfig,
    token: &str,
    row: &Value,
) -> Result<(), String> {
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
    let mut ledger = open_ledger(root, &policy, project, pr.number, &task_id)?;
    let cycle = cycle_from_ledger(&ledger);
    let prior_tip = last_observed_tip(&ledger);
    let ready = marked_ready(client, topology, project, token, &pr.head_sha, pr.number).await?;
    let checks = fetch_checks(client, token, &project.repository, &pr.head_sha).await?;
    let intents = reconcile_snapshot(&policy, &pr, &cycle, &checks, prior_tip.as_deref(), ready);
    record_intents(
        &mut ledger,
        &task_id,
        &project.repository,
        pr.number,
        &intents,
    )
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
    let review = &topology.shepherd.review;
    for page in bounded_pages(review.max_pages) {
        let path = format!(
            "/repos/{}/issues/{number}/events?per_page={}&page={page}",
            project.repository, review.page_size
        );
        let value = github(client, token, &path).await?;
        let Some(rows) = value.as_array() else {
            return Err("pull event list was not an array".into());
        };
        if rows
            .iter()
            .any(|row| ready_for_review_commit(row) == Some(sha))
        {
            return Ok(true);
        }
        if rows.len() < review.page_size {
            break;
        }
    }
    Ok(false)
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

async fn github(client: &Client, token: &str, path: &str) -> Result<Value, String> {
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
