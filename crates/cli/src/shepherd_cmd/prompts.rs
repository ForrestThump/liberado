use super::*;

pub(super) fn note(set: &BTreeSet<String>) -> String {
    if set.is_empty() {
        String::new()
    } else {
        format!(
            "{} failures were already on base; do not fix them:\n{}\n",
            set.len(),
            set.iter()
                .take(10)
                .map(|s| format!("  - {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

pub(super) fn kickback_prompt(
    pr: &Pr,
    failures: &BTreeSet<String>,
    old: &BTreeSet<String>,
) -> String {
    let list = failures
        .iter()
        .map(|failure| format!("  - {failure}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Pull request #{} (branch `{}`: {}) introduced {} new CI failure(s).\n\nNew failures:\n{}\n\n{}Do this:\n1. `git fetch origin` and check out `{}`.\n2. Reproduce a new failure locally before changing anything. A fix you never watched fail is a guess.\n3. Fix the cause. Do not delete, skip, or `#[ignore]` a test to get green. If a test is genuinely wrong, explain why in the commit message.\n4. Commit and push to `{}`.\n\nStay inside this scope. Do not refactor, reformat, or fix unrelated things.",
        pr.number,
        pr.branch,
        pr.title,
        failures.len(),
        list,
        note(old),
        pr.branch,
        pr.branch,
    )
}

pub(super) fn cold_review_prompt(
    cfg: &Config,
    pr: &Pr,
    round: usize,
    old: &BTreeSet<String>,
) -> String {
    format!(
        "Cold review of pull request #{} (branch `{}`: {}). Round {} of {}.\n\nYou have no prior context on this change. Review it as written.\n\n1. `git fetch origin`, check out `{}`, and read `git diff origin/{}...HEAD`.\n2. Find real problems: bugs, missing edge cases, security holes, or broken invariants. Ignore style and formatting; CI already enforces those.\n3. For each suspicion, read the actual code and classify it as Real, Exaggerated, or Hallucinated. Fix only what is Real.\n4. For each real fix, add a test that fails without it and passes with it. Run it both ways; a test you never watched fail proves nothing.\n5. Commit and push to `{}`. If you found nothing Real, push nothing and say so.\n\n{}",
        pr.number,
        pr.branch,
        pr.title,
        round,
        cfg.cold_reviews,
        pr.branch,
        cfg.base,
        pr.branch,
        note(old),
    )
}
