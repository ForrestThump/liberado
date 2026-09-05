use super::prompts::{cold_review_prompt, kickback_prompt};
use super::record::{self, ShepherdFact};
use super::*;

/// What a PR with fresh CI failures should get, decided from facts alone so tests can pin the
/// escalation ladder without `gh` or a daemon on the wire: rerun once, then kick back up to the
/// cap (a free slot required), then block.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FailureAction {
    Rerun,
    Blocked,
    WaitForSlot,
    Kickback,
}

pub(super) fn next_failure_action(
    has_rerun: bool,
    kicks: usize,
    max_kickbacks: usize,
    slot_free: impl FnOnce() -> bool,
) -> FailureAction {
    if !has_rerun {
        FailureAction::Rerun
    } else if kicks >= max_kickbacks {
        FailureAction::Blocked
    } else if !slot_free() {
        FailureAction::WaitForSlot
    } else {
        FailureAction::Kickback
    }
}

/// The clean-PR mirror of [`next_failure_action`]: ready once the cold-review cap is met,
/// otherwise spend a free slot on one more cold review.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CleanAction {
    Ready,
    WaitForSlot,
    Review { round: usize },
}

pub(super) fn next_clean_action(
    reviews: usize,
    cold_reviews: usize,
    slot_free: impl FnOnce() -> bool,
) -> CleanAction {
    if reviews >= cold_reviews {
        CleanAction::Ready
    } else if !slot_free() {
        CleanAction::WaitForSlot
    } else {
        CleanAction::Review { round: reviews + 1 }
    }
}

/// A PR with fresh CI failures: rerun once, then kick back a goal (up to the cap), then block.
pub(super) fn handle_new_failures(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
    new: &BTreeSet<String>,
    old: &BTreeSet<String>,
    run: &Option<Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    let kicks = pr.count("shepherd:kickback-");
    let action = next_failure_action(pr.has(RERUN), kicks, cfg.max_kickbacks, || {
        slot_is_free(cfg)
    });
    apply_failure_action(cfg, pr, dry, new, old, run, action)
}

fn slot_is_free(cfg: &Config) -> bool {
    active_goals(cfg) < cfg.max_concurrent
}

fn apply_failure_action(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
    new: &BTreeSet<String>,
    old: &BTreeSet<String>,
    run: &Option<Value>,
    action: FailureAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        FailureAction::Rerun => rerun_failed_run(cfg, pr, dry, run),
        FailureAction::Blocked => block_pr(cfg, pr, dry),
        FailureAction::WaitForSlot => Ok(()),
        FailureAction::Kickback => {
            let kicks = pr.count("shepherd:kickback-");
            kickback(cfg, pr, dry, new, old, kicks, run)
        }
    }
}

fn rerun_failed_run(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
    run: &Option<Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    let github_run_id = run.as_ref().and_then(|r| r["databaseId"].as_u64());
    if !dry {
        let Some(id) = github_run_id else {
            return Ok(());
        };
        let id = id.to_string();
        let _ = gh(cfg, &["run", "rerun", &id, "--failed"], false);
        label(cfg, pr, RERUN.into());
    }
    record::record_facts(cfg, pr, dry, &[ShepherdFact::Rerun { github_run_id }])?;
    Ok(())
}

fn block_pr(cfg: &Config, pr: &mut Pr, dry: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !dry {
        label(cfg, pr, BLOCKED.into())
    }
    record::record_facts(
        cfg,
        pr,
        dry,
        &[ShepherdFact::Blocked {
            reason: "kickback cap reached".into(),
        }],
    )?;
    Ok(())
}

fn kickback(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
    new: &BTreeSet<String>,
    old: &BTreeSet<String>,
    kicks: usize,
    run: &Option<Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    if dry {
        return Ok(());
    }
    let kick = kicks + 1;
    let task_id = liberado_coder_core::shepherd_task_id(cfg.repository.as_deref(), pr.number);
    let command_id = format!("repair:{}:{}:{kick}", pr.number, pr.head_sha);
    let goal_id = format!(
        "shepherd-repair-{}-{}-{kick}",
        pr.number,
        short_sha(&pr.head_sha)
    );
    let github_run_id = run
        .as_ref()
        .and_then(|value| value["databaseId"].as_u64())
        .unwrap_or(0);
    let cause_event_id = format!("evt-ci-{}-{github_run_id}-failure", pr.number);
    record::record_facts(
        cfg,
        pr,
        false,
        &[ShepherdFact::Repair {
            goal_id: Some(goal_id.clone()),
            reason: format!("{} new CI failures", new.len()),
            kick,
            cause_event_id: cause_event_id.clone(),
        }],
    )?;
    let prompt = kickback_prompt(pr, new, old);
    let id = start_goal_with(
        cfg,
        prompt,
        0,
        Some(&goal_id),
        json!({
            "control_plane": {
                "task_id": task_id,
                "command_id": command_id,
                "cause_event_id": cause_event_id,
                "revision": pr.head_sha,
            }
        }),
    )
    .ok_or("repair task service did not accept the command")?;
    label(cfg, pr, format!("shepherd:kickback-{kick}"));
    remove_label(cfg, pr, RERUN);
    log(
        cfg,
        "kickback_started",
        json!({"pr":pr.number,"session":id}),
    );
    Ok(())
}

fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// A PR whose CI is now clean: ready it once the cold-review cap is met, otherwise spend a
/// budget slot on a cold review.
pub(super) fn handle_clean(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
    old: &BTreeSet<String>,
    github_run_id: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let reviews = pr.count("shepherd:review-");
    let action = next_clean_action(reviews, cfg.cold_reviews, || {
        active_goals(cfg) < cfg.max_concurrent
    });
    match action {
        CleanAction::Ready => mark_ready(cfg, pr, dry, github_run_id, reviews),
        CleanAction::WaitForSlot => Ok(()),
        CleanAction::Review { round } => start_cold_review(cfg, pr, dry, old, round),
    }
}

fn mark_ready(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
    github_run_id: Option<u64>,
    reviews: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if !dry {
        label(cfg, pr, READY.into())
    }
    record::record_facts(
        cfg,
        pr,
        dry,
        &[ShepherdFact::Ready {
            github_run_id,
            review_round: reviews as u32,
        }],
    )?;
    Ok(())
}

fn start_cold_review(
    cfg: &Config,
    pr: &Pr,
    dry: bool,
    old: &BTreeSet<String>,
    round: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let goal_id = launch_cold_review(cfg, pr, dry, old, round)?;
    record_review_if_started(cfg, pr, dry, round, goal_id)
}

fn launch_cold_review(
    cfg: &Config,
    pr: &Pr,
    dry: bool,
    old: &BTreeSet<String>,
    round: usize,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if dry {
        return Ok(None);
    }
    start_and_persist_review(cfg, pr, old, round)
}

fn start_and_persist_review(
    cfg: &Config,
    pr: &Pr,
    old: &BTreeSet<String>,
    round: usize,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let prompt = cold_review_prompt(cfg, pr, round, old);
    let Some(id) = start_goal(cfg, prompt, cfg.cold_turns) else {
        return Ok(None);
    };
    persist_pending_review(cfg, pr.number, &id, round)?;
    Ok(Some(id))
}

fn persist_pending_review(
    cfg: &Config,
    number: u64,
    id: &str,
    round: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = pending(cfg, number);
    fs::create_dir_all(pending_parent(&path)?)?;
    fs::write(
        path,
        serde_json::to_vec(&json!({"session_id":id,"round":round}))?,
    )?;
    Ok(())
}

/// Directory that holds a pending-review file. A root path is a programming error,
/// not a crash the operator should see as an unwrap.
fn pending_parent(path: &Path) -> Result<&Path, Box<dyn std::error::Error>> {
    path.parent()
        .ok_or_else(|| format!("pending review path has no parent: {}", path.display()).into())
}

fn record_review_if_started(
    cfg: &Config,
    pr: &Pr,
    dry: bool,
    round: usize,
    goal_id: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !dry && goal_id.is_none() {
        return Ok(());
    }
    record::record_facts(
        cfg,
        pr,
        dry,
        &[ShepherdFact::ReviewRequested { round, goal_id }],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "actions_tests.rs"]
mod actions_tests;
