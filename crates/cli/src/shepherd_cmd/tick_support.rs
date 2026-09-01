use super::*;

pub(super) fn handle_settled_tick(
    cfg: &Config,
    pr: &mut Pr,
    dry: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (new, old, run) = ci_delta(cfg, pr)?;
    if !new.is_empty() {
        return handle_new_failures(cfg, pr, dry, &new, &old, &run);
    }
    handle_clean(cfg, pr, dry, &old)
}
