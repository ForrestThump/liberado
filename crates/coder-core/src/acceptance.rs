//! What "succeeded" is allowed to mean when nobody configured verifiers.
//!
//! Without this, an unattended run's only acceptance test was `GitNonemptyDiff` — "the diff is
//! non-empty" — so `outcome: succeeded` meant *the model said so and touched a file*. PR #92
//! shipped non-compiling tests and reported success on exactly that basis (backlog F10).
//!
//! F10 fixed it in `coder-runner`. It did not reach the ACP bridge, which passes
//! `tuning.verifiers` straight through and gets an empty list by default — so the editor path
//! kept accepting work that had never been compiled. An F6 run filed `succeeded` with a fabricated
//! mutation table after `cargo check` failed three times in a row, and nothing disagreed.
//!
//! That is the third time in one week a fix landed on one path and not its sibling
//! (`preserve_work`, inherited stdin, and now this). Hence one function, in the crate both
//! consumers already depend on, rather than a second copy.
//!
//! Deliberately not in `verify.rs`: that module states it avoids git and cargo so it can graduate
//! to `liberado-common` when a second domain needs it. This one is coding-specific on purpose.

use std::path::Path;

use crate::VerifierSpec;

/// Override the default acceptance command. Whitespace-split; first token is the program.
pub const VERIFY_CMD_ENV: &str = "LIBERADO_CODER_VERIFY_CMD";

/// A cold `cargo check --workspace --all-targets` on this repo takes minutes. The default
/// timeout would report a failure that is really a stopwatch.
const CHECK_TIMEOUT_SECS: u64 = 900;

/// The acceptance checks a coding run must pass when the deployment configured none.
///
/// `cargo check` rather than `cargo build`: it catches the type and syntax errors these runs
/// actually produce, at a fraction of the time and disk — and disk is finite, as a run that
/// filled 476 GB with nine concurrent builds demonstrated.
pub fn default_verifiers(workspace: &Path) -> Vec<VerifierSpec> {
    let mut specs = vec![VerifierSpec::GitNonemptyDiff {
        id: "nonempty-diff".into(),
    }];

    // An explicit command replaces the default rather than adding to it: a deployment that says
    // how to verify itself knows better than this function does.
    if let Ok(custom) = std::env::var(VERIFY_CMD_ENV) {
        let mut parts = custom.split_whitespace().map(str::to_string);
        if let Some(program) = parts.next() {
            specs.push(VerifierSpec::Command {
                id: "verify-cmd".into(),
                program,
                args: parts.collect(),
                env: Default::default(),
                timeout_secs: Some(CHECK_TIMEOUT_SECS),
                output_max_bytes: None,
                network: false,
            });
            return specs;
        }
    }

    if workspace.join("Cargo.toml").exists() {
        specs.push(VerifierSpec::Command {
            id: "cargo-check".into(),
            program: "cargo".into(),
            args: vec!["check".into(), "--workspace".into(), "--all-targets".into()],
            env: Default::default(),
            timeout_secs: Some(CHECK_TIMEOUT_SECS),
            output_max_bytes: None,
            network: false,
        });
    }
    specs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(specs: &[VerifierSpec]) -> Vec<String> {
        specs
            .iter()
            .map(|s| match s {
                VerifierSpec::GitNonemptyDiff { id } => id.clone(),
                VerifierSpec::Command { id, .. } => id.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// Process-global env; `cargo test` runs a crate's tests concurrently in one binary.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_rust_workspace_must_compile_to_count_as_succeeded() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var(VERIFY_CMD_ENV) };
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("manifest");

        let specs = default_verifiers(dir.path());
        assert!(
            ids(&specs).contains(&"cargo-check".to_string()),
            "a non-empty diff alone must not mean success: {:?}",
            ids(&specs)
        );
    }

    #[test]
    fn a_non_rust_workspace_gets_no_cargo_check() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var(VERIFY_CMD_ENV) };
        let dir = tempfile::tempdir().expect("tempdir");

        let specs = default_verifiers(dir.path());
        assert!(
            !ids(&specs).contains(&"cargo-check".to_string()),
            "cargo check on a directory with no Cargo.toml is a guaranteed false failure"
        );
    }

    #[test]
    fn an_explicit_command_replaces_the_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("manifest");
        unsafe { std::env::set_var(VERIFY_CMD_ENV, "just test --all") };

        let specs = default_verifiers(dir.path());
        unsafe { std::env::remove_var(VERIFY_CMD_ENV) };

        let names = ids(&specs);
        assert!(names.contains(&"verify-cmd".to_string()), "{names:?}");
        assert!(
            !names.contains(&"cargo-check".to_string()),
            "an explicit command must replace the default, not run alongside it: {names:?}"
        );
    }
}
