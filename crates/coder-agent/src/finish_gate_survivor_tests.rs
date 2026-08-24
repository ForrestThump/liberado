//! Split from `finish_gate.rs`: kills the baseline campaign's survivors.
//!
//! A `succeeded` report in a compiling workspace is accepted; in a broken
//! workspace it is refused; and a host-shaped failure gets the host message,
//! not repair advice.

use super::*;
use liberado_common::Outcome;
use std::path::Path;

fn gate_for(root: &Path) -> WorkspaceCompileGate {
    WorkspaceCompileGate {
        workspace_root: root.to_string_lossy().into_owned(),
    }
}

fn succeeded() -> Report {
    Report {
        outcome: Outcome::Succeeded,
        summary: "done".into(),
        artifacts: Vec::new(),
        new_high_signal_facts: Vec::new(),
        follow_up: None,
        deferred_to_human: false,
        repeat_calls: 0,
    }
}

/// A crate whose only defect is a compile error carrying an infrastructure
/// phrase, so the classifier routes it as a host failure deterministically.
fn broken_workspace_with_host_phrase() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname=\"probe\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src").join("lib.rs"),
        "compile_error!(\"rustc-LLVM ERROR: IO failure: No space left on device\");\n",
    )
    .unwrap();
    dir
}

#[tokio::test]
async fn a_succeeded_report_in_a_green_workspace_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname=\"probe\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src").join("lib.rs"), "pub fn f() {}\n").unwrap();
    let result = gate_for(dir.path()).accept(&succeeded(), false).await;
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
async fn a_succeeded_report_in_a_broken_workspace_is_refused() {
    let dir = broken_workspace_with_host_phrase();
    let result = gate_for(dir.path()).accept(&succeeded(), false).await;
    assert!(result.is_err(), "a red workspace must not accept success");
}

#[tokio::test]
async fn a_host_failure_gets_the_host_message_not_repair_advice() {
    let dir = broken_workspace_with_host_phrase();
    let err = gate_for(dir.path())
        .accept(&succeeded(), false)
        .await
        .expect_err("refused");
    assert!(
        err.contains("the host failed"),
        "infrastructure must not be sent to the model as repair work: {err}"
    );
}
