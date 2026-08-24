//! Run-command support for [`super::run`] — argument parsing, scratch
//! hygiene, the cargo spawn, and the outcome announcements. Split out so the
//! parent file stays under its function-count ratchet while every usage rule
//! stays unit-testable.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use liberado_common::process::std_command;

use super::{MUTANTS_TARGET_DIR, RecordOutcome, RunProfile};

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

/// Spawn cargo-mutants in an isolated target dir and wait for it.
///
/// The artifact dir keeps mutant builds out of `target/debug`, so a campaign
/// never evicts the developer's own incremental cache.
pub(super) fn spawn_mutants(
    root: &Path,
    command: &str,
) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    let mutants_target: PathBuf = root.join(MUTANTS_TARGET_DIR);
    eprintln!(
        "[mutants] artifact dir: {} (isolated from target/debug)",
        mutants_target.display()
    );
    let status = std_command("cargo")
        .args(command.split_whitespace().skip(1))
        .current_dir(root)
        .env("CARGO_TARGET_DIR", &mutants_target)
        .status()?;
    Ok(status)
}

/// Say what recording did, and say it honestly when cargo-mutants failed.
pub(super) fn announce_record(outcome: RecordOutcome, cargo_success: bool) {
    match outcome {
        RecordOutcome::Appended { package, commit } => {
            eprintln!("[mutants] recorded campaign for {package} at {commit}");
        }
        RecordOutcome::SkippedIncomplete => {
            eprintln!("[mutants] run finished but outcomes were incomplete; nothing recorded");
        }
    }
    if !cargo_success {
        eprintln!(
            "[mutants] cargo mutants exited with a failure; campaign recorded if outcomes were complete"
        );
    }
}
