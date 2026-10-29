//! Cargo commands for `liberado ci check`.
//!
//! The list lives here so `ci_cmd.rs` keeps its module-health cyclomatic baseline.
//! One loop runs each slice, so these helpers stay under the new-function CRAP
//! ceiling without a test that shells out to the full preflight.

use super::{CiLog, run_cmd};

/// `cargo` steps that run before dependency security.
const BEFORE_SECURITY: &[&[&str]] = &[&["fmt", "--check"]];

/// `cargo` steps that run after dependency security.
const AFTER_SECURITY: &[&[&str]] = &[
    &[
        "clippy",
        "--workspace",
        "--exclude",
        "liberado-webui",
        "--all-targets",
        "--",
        "-D",
        "warnings",
        "-D",
        "clippy::cognitive_complexity",
    ],
    &["test", "--workspace", "--no-fail-fast"],
    &[
        "clippy",
        "--locked",
        "-p",
        "liberado-cli",
        "-p",
        "liberado-server",
        "-p",
        "liberado-bootstrap",
        "-p",
        "liberado-dispatch-pack",
        "--all-targets",
        "--features",
        "coding",
        "--",
        "-D",
        "warnings",
        "-D",
        "clippy::cognitive_complexity",
    ],
    &[
        "test",
        "--locked",
        "-p",
        "liberado-cli",
        "-p",
        "liberado-server",
        "-p",
        "liberado-bootstrap",
        "-p",
        "liberado-dispatch-pack",
        "--features",
        "coding",
        "--no-fail-fast",
    ],
];

pub(super) fn run(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    run_cargo(log, BEFORE_SECURITY)?;
    crate::dependency_security_cmd::run(log)?;
    run_cargo(log, AFTER_SECURITY)
}

fn run_cargo(log: &CiLog, steps: &[&[&str]]) -> Result<(), Box<dyn std::error::Error>> {
    for args in steps {
        run_cmd(log, "cargo", args)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{AFTER_SECURITY, BEFORE_SECURITY};

    #[test]
    fn cargo_steps_keep_fmt_workspace_and_coding_commands() {
        assert_eq!(BEFORE_SECURITY.len(), 1);
        assert_eq!(BEFORE_SECURITY[0], &["fmt", "--check"][..]);
        assert_eq!(
            AFTER_SECURITY[1],
            &["test", "--workspace", "--no-fail-fast"][..]
        );
        assert!(AFTER_SECURITY[0].contains(&"--exclude"));
        assert!(AFTER_SECURITY[0].contains(&"liberado-webui"));
        assert!(AFTER_SECURITY[0].contains(&"clippy::cognitive_complexity"));
        for args in [AFTER_SECURITY[2], AFTER_SECURITY[3]] {
            assert!(args.contains(&"--features"));
            assert!(args.contains(&"coding"));
            for package in [
                "liberado-cli",
                "liberado-server",
                "liberado-bootstrap",
                "liberado-dispatch-pack",
            ] {
                assert!(args.contains(&package), "{package} missing from {args:?}");
            }
        }
        assert!(AFTER_SECURITY[2].contains(&"clippy"));
        assert!(AFTER_SECURITY[3].contains(&"--no-fail-fast"));
    }
}
