//! Mirror shepherd facts into the durable task ledger. Record only; no new dispatch.

use super::*;
use liberado_coder_core::{
    CONTROLLER_LIBERADO_SHEPHERD, TaskEvent, TaskEventKind, TaskLedger, TaskRecord,
    durable_tasks_root, shepherd_task_id,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ShepherdFact {
    Ci {
        github_run_id: Option<u64>,
        state: String,
        failures: Vec<String>,
    },
    Rerun {
        github_run_id: Option<u64>,
    },
    Repair {
        goal_id: Option<String>,
        reason: String,
        kick: usize,
    },
    ReviewRequested {
        round: usize,
        goal_id: Option<String>,
    },
    ReviewApproved {
        round: usize,
    },
    Ready {
        github_run_id: Option<u64>,
        review_round: u32,
    },
    Blocked {
        reason: String,
    },
}

pub(super) fn record_facts(
    cfg: &Config,
    pr: &Pr,
    dry: bool,
    facts: &[ShepherdFact],
) -> Result<Option<TaskRecord>, Box<dyn std::error::Error>> {
    if dry {
        return Ok(None);
    }
    let task_id = shepherd_task_id(cfg.repository.as_deref(), pr.number);
    let mut ledger = open_pr_ledger(cfg, pr, &task_id)?;
    for fact in facts {
        ledger.append(fact_event(&task_id, pr, fact))?;
    }
    Ok(Some(ledger.project()?))
}

fn open_pr_ledger(
    cfg: &Config,
    pr: &Pr,
    task_id: &str,
) -> Result<TaskLedger, Box<dyn std::error::Error>> {
    let created = TaskEvent::new(
        format!("evt-{task_id}-created"),
        task_id,
        TaskEventKind::TaskCreated {
            objective: format!("Shepherd pull request #{}", pr.number),
            acceptance_criteria: vec![
                "Ready binds head SHA plus CI and review evidence".into(),
                "Human merge remains the hard gate".into(),
            ],
            worktree: cfg.root.to_string_lossy().into_owned(),
            branch: pr.branch.clone(),
            base_ref: cfg.base.clone(),
            repo: cfg.repository.clone(),
        },
    )
    .with_command_id(format!("create:{task_id}"));
    let mut ledger = TaskLedger::create_in(durable_tasks_root(&cfg.root), created)?;
    ledger.append(
        TaskEvent::new(
            format!("evt-{task_id}-lease"),
            task_id,
            TaskEventKind::ControllerLeaseClaimed {
                controller: CONTROLLER_LIBERADO_SHEPHERD.into(),
            },
        )
        .with_command_id(format!(
            "lease:{}:{CONTROLLER_LIBERADO_SHEPHERD}",
            pr.number
        )),
    )?;
    ledger.append(
        TaskEvent::new(
            format!("evt-{task_id}-pr"),
            task_id,
            TaskEventKind::PullRequestOpened {
                pr_number: pr.number,
                url: pr_url(cfg, pr),
            },
        )
        .with_command_id(format!("pr:{}", pr.number)),
    )?;
    if !pr.head_sha.is_empty() {
        ledger.append(
            TaskEvent::new(
                format!("evt-{task_id}-head-{}", short_sha(&pr.head_sha)),
                task_id,
                TaskEventKind::HeadRevisionObserved {
                    sha: pr.head_sha.clone(),
                },
            )
            .with_command_id(format!("head:{}:{}", pr.number, pr.head_sha)),
        )?;
    }
    Ok(ledger)
}

fn fact_event(task_id: &str, pr: &Pr, fact: &ShepherdFact) -> TaskEvent {
    match fact {
        ShepherdFact::Ci {
            github_run_id,
            state,
            failures,
        } => TaskEvent::new(
            format!(
                "evt-ci-{}-{}-{state}",
                pr.number,
                github_run_id.unwrap_or(0)
            ),
            task_id,
            TaskEventKind::CiObserved {
                github_run_id: *github_run_id,
                head_sha: nonempty_sha(pr),
                state: state.clone(),
                failures: failures.clone(),
            },
        )
        .with_command_id(format!(
            "ci:{}:{}:{}:{state}",
            pr.number,
            pr.head_sha,
            github_run_id.unwrap_or(0)
        )),
        ShepherdFact::Rerun { github_run_id } => TaskEvent::new(
            format!("evt-rerun-{}-{}", pr.number, github_run_id.unwrap_or(0)),
            task_id,
            TaskEventKind::RerunDecided {
                github_run_id: *github_run_id,
            },
        )
        .with_command_id(format!(
            "rerun:{}:{}",
            pr.number,
            github_run_id.unwrap_or(0)
        )),
        ShepherdFact::Repair {
            goal_id,
            reason,
            kick,
        } => TaskEvent::new(
            format!("evt-repair-{}-{kick}", pr.number),
            task_id,
            TaskEventKind::RepairRequested {
                goal_id: goal_id.clone(),
                reason: reason.clone(),
            },
        )
        .with_command_id(format!("repair:{}:{}:{kick}", pr.number, pr.head_sha)),
        ShepherdFact::ReviewRequested { round, goal_id } => TaskEvent::new(
            format!("evt-review-req-{}-{round}", pr.number),
            task_id,
            TaskEventKind::ReviewRequested {
                round: *round,
                goal_id: goal_id.clone(),
            },
        )
        .with_command_id(format!("review:{}:{}:{round}", pr.number, pr.head_sha)),
        ShepherdFact::ReviewApproved { round } => TaskEvent::new(
            format!("evt-review-ok-{}-{round}", pr.number),
            task_id,
            TaskEventKind::ReviewApproved {
                reviewer: CONTROLLER_LIBERADO_SHEPHERD.into(),
                round: *round,
            },
        )
        .with_command_id(format!("review-ok:{}:{}:{round}", pr.number, pr.head_sha)),
        ShepherdFact::Ready {
            github_run_id,
            review_round,
        } => TaskEvent::new(
            format!("evt-ready-{}-{}", pr.number, short_sha(&pr.head_sha)),
            task_id,
            TaskEventKind::ReadyDecided {
                head_sha: pr.head_sha.clone(),
                ci_github_run_id: *github_run_id,
                review_round: *review_round,
            },
        )
        .with_command_id(format!("ready:{}:{}", pr.number, pr.head_sha)),
        ShepherdFact::Blocked { reason } => TaskEvent::new(
            format!("evt-blocked-{}", pr.number),
            task_id,
            TaskEventKind::BlockedDecided {
                reason: reason.clone(),
            },
        )
        .with_command_id(format!("blocked:{}:{}", pr.number, pr.head_sha)),
    }
}

fn nonempty_sha(pr: &Pr) -> Option<String> {
    (!pr.head_sha.is_empty()).then(|| pr.head_sha.clone())
}

fn short_sha(sha: &str) -> &str {
    let end = sha.len().min(12);
    &sha[..end]
}

pub(super) fn pr_url(cfg: &Config, pr: &Pr) -> String {
    if !pr.url.is_empty() {
        return pr.url.clone();
    }
    match &cfg.repository {
        Some(repo) => format!("https://github.com/{repo}/pull/{}", pr.number),
        None => format!("pull/{}", pr.number),
    }
}

#[cfg(test)]
pub(super) fn open_recorded(
    cfg: &Config,
    pr: &Pr,
) -> Result<TaskLedger, Box<dyn std::error::Error>> {
    let task_id = shepherd_task_id(cfg.repository.as_deref(), pr.number);
    let path = durable_tasks_root(&cfg.root)
        .join(task_id)
        .join("ledger.jsonl");
    Ok(TaskLedger::load_from_path(path)?)
}
