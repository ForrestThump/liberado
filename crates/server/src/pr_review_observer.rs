//! Bounded, read-only GitHub polling for daemon-native PR review.

use liberado_coder_core::pr_review::{
    CheckConclusion, ReviewPolicy, ShaChecks, bounded_pages, check_endpoints, required_checks_pass,
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
    let number = row["number"].as_u64().ok_or("pull request has no number")?;
    let sha = row
        .pointer("/head/sha")
        .and_then(Value::as_str)
        .ok_or("pull request has no head SHA")?;
    let task_id = shepherd_task_id(Some(&project.repository), number);
    let root = topology
        .projects
        .iter()
        .find(|item| item.name == project.coding_project)
        .map(|item| item.root.as_path())
        .ok_or("coding project root is missing")?;
    let mut ledger = open_ledger(root, project, number, &task_id)?;
    let prior_tip = ledger
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            TaskEventKind::HeadRevisionObserved { sha } => Some(sha.clone()),
            _ => None,
        });
    let armed = ledger
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            TaskEventKind::ReadyArmed { head_sha, .. } => Some(head_sha.clone()),
            _ => None,
        });
    if let Some(old_sha) = prior_tip.filter(|old| old != sha)
        && armed.as_deref() == Some(old_sha.as_str())
    {
        append(
            &mut ledger,
            &task_id,
            format!("review-sync:{number}:{old_sha}:{sha}"),
            TaskEventKind::ReviewSynchronizeIntent {
                old_sha,
                new_sha: sha.into(),
            },
        )?;
    }
    append(
        &mut ledger,
        &task_id,
        format!("tip:{number}:{sha}"),
        TaskEventKind::HeadRevisionObserved { sha: sha.into() },
    )?;
    if row["state"] == "closed" {
        append(
            &mut ledger,
            &task_id,
            format!("review-close:{number}:{sha}"),
            TaskEventKind::PullRequestClosed {
                head_sha: sha.into(),
            },
        )?;
        return Ok(());
    }
    if row["draft"].as_bool().unwrap_or(true) {
        append(
            &mut ledger,
            &task_id,
            format!("review-draft:{number}:{sha}"),
            TaskEventKind::ReviewDraftObserved {
                head_sha: sha.into(),
            },
        )?;
    }
    let target = ReadyTarget {
        number,
        sha,
        task_id: &task_id,
    };
    record_ready_events(client, topology, project, token, target, &mut ledger).await?;
    let checks = fetch_checks(client, token, &project.repository, sha).await?;
    let policy = ReviewPolicy {
        controller: project.controller.clone().unwrap_or_default(),
        check_names: project.check_names.clone(),
        shadow: project.controller.as_deref() != Some("liberado-shepherd"),
    };
    let passed = required_checks_pass(&policy, sha, &checks);
    append(
        &mut ledger,
        &task_id,
        format!("review-ci:{number}:{sha}:{passed}"),
        TaskEventKind::CiObserved {
            github_run_id: None,
            head_sha: Some(sha.into()),
            state: if passed { "success" } else { "pending" }.into(),
            failures: Vec::new(),
        },
    )?;
    Ok(())
}

#[derive(Clone, Copy)]
struct ReadyTarget<'a> {
    number: u64,
    sha: &'a str,
    task_id: &'a str,
}

async fn record_ready_events(
    client: &Client,
    topology: &Topology,
    project: &ShepherdProjectConfig,
    token: &str,
    target: ReadyTarget<'_>,
    ledger: &mut TaskLedger,
) -> Result<(), String> {
    let ReadyTarget {
        number,
        sha,
        task_id,
    } = target;
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
        for row in rows {
            if row["event"] != "ready_for_review" {
                continue;
            }
            let Some(commit) = row["commit_id"].as_str().filter(|commit| *commit == sha) else {
                continue;
            };
            let event_id = row["id"].as_u64().unwrap_or(0);
            append(
                ledger,
                task_id,
                format!("review-arm:{number}:{commit}:{event_id}"),
                TaskEventKind::ReadyArmed {
                    repository: project.repository.clone(),
                    pr_number: number,
                    head_sha: commit.into(),
                    source: "ready_for_review".into(),
                },
            )?;
        }
        if rows.len() < review.page_size {
            break;
        }
    }
    Ok(())
}

fn open_ledger(
    root: &Path,
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
    if project.controller.as_deref() == Some("liberado-shepherd") {
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
        let rows = value["check_runs"]
            .as_array()
            .or_else(|| value["statuses"].as_array());
        for row in rows.into_iter().flatten() {
            let Some(name) = row["name"].as_str().or_else(|| row["context"].as_str()) else {
                continue;
            };
            let raw = row["conclusion"]
                .as_str()
                .or_else(|| row["state"].as_str())
                .unwrap_or("");
            let state = if raw.eq_ignore_ascii_case("success") {
                CheckConclusion::Success
            } else if matches!(raw, "pending" | "queued" | "in_progress") {
                CheckConclusion::Pending
            } else {
                CheckConclusion::Failure
            };
            checks.push((name.into(), state));
        }
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
