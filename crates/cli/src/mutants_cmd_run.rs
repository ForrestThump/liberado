//! Argument parsing and scratch hygiene for [`super::run`] — split out so the
//! parent file stays under its function-count ratchet while every usage rule
//! stays unit-testable.

use std::fs;
use std::path::Path;

use super::RunProfile;

const USAGE_RUN: &str = "usage: liberado mutants run [--lib-only] <crate-dir>";

/// A parsed `mutants run` invocation.
#[derive(Debug)]
pub(super) struct RunInvocation {
    pub(super) profile: RunProfile,
    pub(super) crate_dir: String,
}

/// Pure argument parsing so every usage rule is checkable without spawning cargo.
///
/// `--lib-only` is only recognised as the FIRST argument; elsewhere it would be
/// a crate directory name like any other.
pub(super) fn parse_run_invocation(arguments: &[String]) -> Result<RunInvocation, String> {
    let (profile, rest) = match arguments.split_first() {
        Some((flag, rest)) if flag == "--lib-only" => (RunProfile::LibOnly, rest),
        _ => (RunProfile::Default, arguments),
    };
    match rest {
        [crate_dir] => Ok(RunInvocation {
            profile,
            crate_dir: crate_dir.clone(),
        }),
        _ => Err(USAGE_RUN.to_string()),
    }
}

/// Delete outcomes.json left by an earlier (possibly crashed) campaign.
///
/// The file is persistent scratch. If this run dies before cargo-mutants
/// rewrites it, the file still holds the previous campaign of (often) this same
/// crate; recording would then append those stale counts under today's commit
/// and reset the drift clock. Remove it first so a row can only ever come from
/// the run that just finished. (The recorder's completeness check is the second
/// line of defence; this removes the trigger.)
pub(super) fn clear_stale_outcomes(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let stale_outcomes = root.join(super::OUTCOMES_FILE);
    if stale_outcomes.exists() {
        fs::remove_file(&stale_outcomes)
            .map_err(|e| format!("could not clear stale {}: {e}", super::OUTCOMES_FILE))?;
    }
    Ok(())
}
