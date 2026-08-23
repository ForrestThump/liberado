//! GitHub annotation emitter for red CRAP gates. Split from `ci_cmd.rs` for module health.

use super::{CRAP_CEILING_GH, CRAP_REGRESSION_GH, crap_failure_hint};

/// Explain a red CRAP gate with the CI flag supplied by the caller, so tests never touch
/// process env. The `ci_cmd.rs` wrapper reads `GITHUB_ACTIONS` once and delegates here.
pub(super) fn emit_crap_failure_to(
    on_ci: bool,
    has_ratchet: bool,
    error: Box<dyn std::error::Error>,
) -> Box<dyn std::error::Error> {
    let hint = crap_failure_hint(has_ratchet);
    if on_ci {
        let title = if has_ratchet {
            "CRAP regression"
        } else {
            "CRAP ceiling"
        };
        let message = if has_ratchet {
            CRAP_REGRESSION_GH
        } else {
            CRAP_CEILING_GH
        };
        eprintln!("::error title={title}::{message}");
    }
    eprintln!("\n----------\n{hint}\n----------");
    format!("{error}\n\n{hint}").into()
}
