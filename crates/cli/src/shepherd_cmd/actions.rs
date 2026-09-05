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
        active_goals(cfg) < cfg.max_concurrent
    });
    match action {
        FailureAction::Rerun => rerun_failed_run(cfg, pr, dry, run),
        FailureAction::Blocked => block_pr(cfg, pr, dry),
        FailureAction::WaitForSlot => Ok(()),
        FailureAction::Kickback => kickback(cfg, pr, dry, new, old, kicks),
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
) -> Result<(), Box<dyn std::error::Error>> {
    let mut goal_id = None;
    if !dry {
        let prompt = kickback_prompt(pr, new, old);
        if let Some(id) = start_goal(cfg, prompt, 0) {
            label(cfg, pr, format!("shepherd:kickback-{}", kicks + 1));
            remove_label(cfg, pr, RERUN);
            log(
                cfg,
                "kickback_started",
                json!({"pr":pr.number,"session":id}),
            );
            goal_id = Some(id);
        }
    }
    if !dry && goal_id.is_none() {
        return Ok(());
    }
    record::record_facts(
        cfg,
        pr,
        dry,
        &[ShepherdFact::Repair {
            goal_id,
            reason: format!("{} new CI failures", new.len()),
            kick: kicks + 1,
        }],
    )?;
    Ok(())
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
    let mut goal_id = None;
    if !dry {
        let prompt = cold_review_prompt(cfg, pr, round, old);
        if let Some(id) = start_goal(cfg, prompt, cfg.cold_turns) {
            let path = pending(cfg, pr.number);
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(
                path,
                serde_json::to_vec(&json!({"session_id":id,"round":round}))?,
            )?;
            goal_id = Some(id);
        }
    }
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
