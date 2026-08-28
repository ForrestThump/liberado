//! CI boundary for the standalone Python branch cleaner.

use crate::ci_cmd::{CiLog, run_cmd};

const TEST: &str = "scripts/test_cleanup_merged_branches.py";

/// Run the branch cleaner tests in the Python runtime that operators use.
pub(crate) fn run(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(windows) {
        run_cmd(log, "py", &["-3", "-m", "unittest", TEST])
    } else {
        run_cmd(log, "python3", &["-m", "unittest", TEST])
    }
}
