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

type CargoStep = fn(&CiLog, &[&str]) -> Result<(), Box<dyn std::error::Error>>;

pub(super) fn run(log: &CiLog) -> Result<(), Box<dyn std::error::Error>> {
    run_cargo(log, BEFORE_SECURITY, cargo_step)?;
    crate::dependency_security_cmd::run(log)?;
    run_cargo(log, AFTER_SECURITY, cargo_step)
}

fn cargo_step(log: &CiLog, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    run_cmd(log, "cargo", args)
}

fn run_cargo(
    log: &CiLog,
    steps: &[&[&str]],
    cargo: CargoStep,
) -> Result<(), Box<dyn std::error::Error>> {
    for args in steps {
        cargo(log, args)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::{AFTER_SECURITY, BEFORE_SECURITY, CiLog, run_cargo};

    thread_local! {
        static SEEN: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    fn temp_log() -> (tempfile::TempDir, CiLog) {
        let dir = tempfile::tempdir().unwrap();
        let log = CiLog::create(dir.path()).unwrap();
        (dir, log)
    }

    fn record(log: &CiLog, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
        let _ = log;
        SEEN.with(|seen| seen.borrow_mut().push(args.join(" ")));
        Ok(())
    }

    fn fail_on_test(log: &CiLog, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
        record(log, args)?;
        if args.first().copied() == Some("test") {
            Err("stopped before a build".into())
        } else {
            Ok(())
        }
    }

    fn seen() -> Vec<String> {
        SEEN.with(|seen| std::mem::take(&mut *seen.borrow_mut()))
    }

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

    #[test]
    fn run_cargo_invokes_each_step_in_order() {
        let _ = seen();
        let (_dir, log) = temp_log();
        run_cargo(&log, BEFORE_SECURITY, record).unwrap();
        run_cargo(&log, &AFTER_SECURITY[..2], record).unwrap();
        assert_eq!(
            seen(),
            vec![
                "fmt --check".to_string(),
                AFTER_SECURITY[0].join(" "),
                AFTER_SECURITY[1].join(" "),
            ]
        );
    }

    #[test]
    fn run_cargo_stops_when_a_step_fails() {
        let _ = seen();
        let (_dir, log) = temp_log();
        let err = run_cargo(&log, AFTER_SECURITY, fail_on_test).unwrap_err();
        assert!(err.to_string().contains("stopped before a build"));
        let got = seen();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].starts_with("clippy "));
        assert!(got[1].starts_with("test "));
    }
}
