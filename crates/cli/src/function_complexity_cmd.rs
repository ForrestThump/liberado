//! Cyclomatic diff gate.
//!
//! `liberado ci complexity` scores functions the git diff touched. The ceiling is
//! `new_function_ceiling` in `code-metrics/function-complexity.toml`. There is no
//! baseline file and no waiver. The rule itself lives in [`crate::ci_cmd::diff_gate`].

use std::path::Path;

pub fn check(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    check_against(root, None)
}

pub fn check_against(root: &Path, base: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    crate::ci_cmd::diff_gate::check_complexity(root, base)
}
