//! `liberado delegate kickback | merge | checks` — the delegator's review verdicts
//! (plan §10). Split from the parent router for module health, same as the other
//! task-addressed subcommands.
//!
//! A kickback is one action, two records: the instruction travels to the worker via
//! the answers endpoint, and — when forge flags are given — a review comment lands on
//! the PR for the human-visible audit trail. Merge is delegator-only and verifies the
//! spec's required checks first: the forge claims green, the delegator confirms.

use std::error::Error;

use super::{Connection, checked, connection, emit, fetch_task, request, routes};

/// One entry for the review family (plan §10): verdicts a delegator renders on a
/// finished PR. New verbs join the inner match.
pub(super) async fn route(
    name: &str,
    args: &mut impl Iterator<Item = String>,
) -> Result<(), Box<dyn Error>> {
    match name {
        "kickback" => coldreview::run(args).await,
        "review" => coldreview::run_review(args).await,
        "merge" => coldreview::run_merge(args).await,
        other => Err(super::usage(&format!("unknown or missing subcommand: {other}")).into()),
    }
}

#[path = "delegate_coldreview.rs"]
mod coldreview;

#[cfg(test)]
#[path = "delegate_cmd_review_tests.rs"]
mod tests;
