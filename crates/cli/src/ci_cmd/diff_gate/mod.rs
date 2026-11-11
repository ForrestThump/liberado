//! Diff gate for CRAP and cyclomatic complexity.
//!
//! The gate looks only at functions whose line span in the working tree intersects a line
//! `git diff` added or modified since the merge-base. Untouched functions are not scored.
//! There is no committed per-function baseline and no waiver.
//!
//! # Limits
//!
//! A new function must land at or under both limits: CRAP `threshold` from `.cargo-crap.toml`
//! (29.9) and `new_function_ceiling` from `code-metrics/function-complexity.toml` (20).
//! A function that already exists on the merge-base may stay over a limit. If it is over a
//! limit after the edit, that score must not be strictly greater than the score measured on
//! the merge-base. CRAP and cyclomatic are compared separately.
//!
//! CRAP is a float. A rise counts only when the new score exceeds the base by more than
//! [`CRAP_EPSILON`] (0.001). That is above binary rounding and below the 0.1 step cargo-crap
//! prints. Cyclomatic values are whole numbers stored as floats; [`CYCLOMATIC_EPSILON`] (0.01)
//! treats `20.0` as equal to the ceiling. Scores at or under the limit pass, so the old
//! `--min 10` noise floor is not used: a 4→5 move is still under 29.9.
//!
//! # Renames, moves, splits, copies
//!
//! Identity is the repo-relative file plus the qualified function name, after git's `-M`
//! rename map. When one file contains the same qualified name more than once, occurrence
//! order (top to bottom) keeps those entries distinct.
//!
//! A changed function whose identity is not on the merge-base is paired, one to one, with a
//! base function that no longer exists at its own identity:
//!
//! 1. The same qualified name in a renamed file, or in any other file.
//! 2. Otherwise a normalized body similarity of at least [`BODY_MATCH_MIN`] (0.8). Bodies are
//!    compared after comments and extra whitespace are removed. The function's own simple name
//!    is dropped so a rename does not hide a match.
//!
//! A match is a touched legacy function. Its cap is the matched base score. This is not a free
//! pass and not a penalty: an over-limit function can move without being forced under the
//! ceiling, and it still cannot get worse.
//!
//! Copying an over-limit function while the original identity still exists gets no match. The
//! original is not a candidate, so the copy is new and must meet the limits. Splitting works
//! the same way. The original identity stays legacy and must not rise. Each extracted function
//! is new and must meet the limits. If the original identity is deleted and several new
//! functions resemble it, only one of them inherits the base score. The others are new.
//!
//! # Base measurement
//!
//! Cyclomatic complexity on the base comes from cargo-crap on the merge-base versions of the
//! changed, renamed, and deleted files. Coverage-backed CRAP is measured only when a touched
//! or matched legacy function is over the CRAP limit. The result is cached at
//! `.liberado/crap-base-<sha>.json`. Before that run, the console and the machine report name
//! each triggering function. The base lcov path is absolute under the HEAD checkout. The
//! temporary worktree is removed on success and on error. Registry artifacts are seeded into
//! a sibling coverage target; profraw and workspace binaries are not shared. The same host
//! measures both sides, so Windows and Linux can both apply the full rule.

use super::{
    CI_LOG_FILE, CRAP_REPORT_ARGS, CURRENT_REPORT, CiLog, LLVM_COV_ARGS, coverage_tools,
    generate_lcov, git, job_budget, relativize_json_file, relativize_lcov, require_crap,
    require_tool, write_crap_json,
};

mod body;
mod cache;
mod dep_seed;
use dep_seed::{head_llvm_cov_target, scrub_profiles, seed_registry_artifacts};
mod classify;
mod decide;
mod git_io;
mod hunks;
mod limits;
mod measure;
mod model;
mod report;
mod run;
mod spans;

pub(crate) use model::{BODY_MATCH_MIN, CRAP_EPSILON, CYCLOMATIC_EPSILON};
pub(crate) use run::{check_complexity, check_crap, merge_base_sha};
