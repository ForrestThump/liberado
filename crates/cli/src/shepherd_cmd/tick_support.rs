use super::actions::{handle_clean, handle_new_failures};
use super::record::{self, ShepherdFact};
use super::*;

pub(super) fn handle_settled_tick(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (new, old, run) = ci_delta(cfg, pr)?;
    record_settled_ci(cfg, pr, dry, &new, &run)?;
    dispatch_settled_tick(cfg, pr, dry, &new, &old, &run)
}

fn record_settled_ci(
    cfg: &Config,
    pr: &Pr,
    dry: bool,
    new: &BTreeSet<String>,
    run: &Option<Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    let github_run_id = run.as_ref().and_then(|row| row["databaseId"].as_u64());
    record::record_facts(
        cfg,
        pr,
        dry,
        &[ShepherdFact::Ci {
            github_run_id,
            state: settled_ci_state(new.is_empty()).into(),
            failures: new.iter().cloned().collect(),
        }],
    )?;
    Ok(())
}

fn settled_ci_state(clean: bool) -> &'static str {
    if clean { "success" } else { "failure" }
}

fn dispatch_settled_tick(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
    new: &BTreeSet<String>,
    old: &BTreeSet<String>,
    run: &Option<Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    let github_run_id = run.as_ref().and_then(|row| row["databaseId"].as_u64());
    if !new.is_empty() {
        return handle_new_failures(cfg, pr, dry, new, old, run);
    }
    handle_clean(cfg, pr, dry, old, github_run_id)
}

pub(super) fn tick_live(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    record::record_facts(cfg, pr, dry, &[])?;
    finish_live_tick(cfg, pr, dry)
}

fn finish_live_tick(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if tick_idle(ci_status(cfg, pr)?) {
        return Ok(());
    }
    settle_or_dispatch(cfg, pr, dry)
}

fn settle_or_dispatch(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if settle(cfg, pr, dry)? == ReviewTransition::Waiting {
        return Ok(());
    }
    handle_settled_tick(cfg, pr, dry)
}

#[cfg(test)]
#[path = "tick_support_tests.rs"]
mod tick_support_tests;
