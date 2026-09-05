use super::actions::{handle_clean, handle_new_failures};
use super::record::{self, ShepherdFact};
use super::*;

pub(super) fn handle_settled_tick(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (new, old, run) = ci_delta(cfg, pr)?;
    let github_run_id = run.as_ref().and_then(|row| row["databaseId"].as_u64());
    let state = if new.is_empty() { "success" } else { "failure" };
    record::record_facts(
        cfg,
        pr,
        dry,
        &[ShepherdFact::Ci {
            github_run_id,
            state: state.into(),
            failures: new.iter().cloned().collect(),
        }],
    )?;
    if !new.is_empty() {
        return handle_new_failures(cfg, pr, dry, &new, &old, &run);
    }
    handle_clean(cfg, pr, dry, &old, github_run_id)
}
