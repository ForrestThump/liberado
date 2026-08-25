//! The `mutants next` selection ladder, exercised through the real
//! [`next_crate_in`] with an injected sink and root — no cwd mutation, no re-implemented
//! ladder. Split from `mutants_cmd_tests.rs` to stay under its module-health boundary.

use super::tests::{init_git_repo, run_git};
use super::*;

use std::path::Path as TestPath;

/// Campaign for one package at an optional commit; everything else defaults.
fn campaign(package: &str, commit: Option<String>) -> Campaign {
    Campaign {
        package: package.into(),
        commit,
        recorded_at: "2026-08-01".into(),
        command: None,
        tool_version: None,
        scope: "package".into(),
        counts: Counts {
            viable: 3,
            caught: 3,
            survived: 0,
            timeout: 0,
            unviable: 0,
        },
        source: None,
    }
}

fn ledger_with(campaigns: Vec<Campaign>) -> Ledger {
    Ledger {
        schema: 1,
        campaigns,
    }
}

fn write_ledger(root: &TestPath, ledger: &Ledger) {
    fs::write(root.join(LEDGER_FILE), serde_json::to_vec(ledger).unwrap()).unwrap();
}

/// Run the real selection ladder and return what it wrote.
fn run_next(root: &TestPath, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let mut out: Vec<u8> = Vec::new();
    let result = next_crate_in(
        root,
        &mut args
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter(),
        &mut out,
    );
    let printed = String::from_utf8(out).unwrap();
    result.map(|_| printed)
}

#[test]
fn next_rejects_unknown_flags_with_usage() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());
    let error = run_next(dir.path(), &["--all", "junk"]).unwrap_err();
    assert!(error.to_string().contains("usage"), "{error}");
}

#[test]
fn next_prefers_a_crate_that_has_never_been_campaigned() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_git_repo(root);
    // Only beta has a campaign; alpha has never been campaigned and must win.
    write_ledger(
        root,
        &ledger_with(vec![campaign(
            "liberado-beta",
            Some(current_commit(root).unwrap()),
        )]),
    );
    assert_eq!(run_next(root, &[]).unwrap(), "alpha\n");
}

#[test]
fn next_falls_through_to_drift_before_historical_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_git_repo(root);
    let base = current_commit(root).unwrap();
    fs::write(
        root.join("crates/beta/lib.rs"),
        "pub fn value() -> i32 { 2 }\n",
    )
    .unwrap();
    run_git(root, &["add", "crates/beta/lib.rs"]);
    run_git(root, &["commit", "-m", "change beta"]);
    // Alpha campaigned at an ancestor (drift); beta campaigned with no recorded
    // commit (historical-only). No never-campaigned crate remains.
    write_ledger(
        root,
        &ledger_with(vec![
            campaign("liberado-alpha", Some(base)),
            campaign("liberado-beta", None),
        ]),
    );
    assert_eq!(
        run_next(root, &[]).unwrap(),
        "alpha\n",
        "drift outranks historical-only"
    );
}

#[test]
fn next_answers_historical_only_when_nothing_else_qualifies() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_git_repo(root);
    // Beta is role testing, so the default selection skips it entirely; alpha's only
    // campaign recorded no commit, making it historical-only.
    fs::write(
        root.join("crates/beta/Cargo.toml"),
        "[package]\nname = \"liberado-beta\"\n\n[package.metadata.liberado]\nrole = \"testing\"\n",
    )
    .unwrap();
    write_ledger(root, &ledger_with(vec![campaign("liberado-alpha", None)]));
    assert_eq!(run_next(root, &[]).unwrap(), "alpha\n");
}

#[test]
fn next_errors_when_every_filter_excludes_every_crate() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_git_repo(root);
    // Both fixtures are role testing, so the default selection skips them and the
    // ladder runs out of candidates.
    for name in ["alpha", "beta"] {
        fs::write(
            root.join(format!("crates/{name}/Cargo.toml")),
            format!(
                "[package]\nname = \"liberado-{name}\"\n\n[package.metadata.liberado]\nrole = \"testing\"\n"
            ),
        )
        .unwrap();
    }
    let error = run_next(root, &[]).unwrap_err();
    assert!(error.to_string().contains("no crates matched"), "{error}");
}
