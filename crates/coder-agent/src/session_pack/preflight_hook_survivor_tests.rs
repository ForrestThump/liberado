//! Split from `session_pack/preflight_hook.rs`: kills the baseline campaign's
//! survivors.
//!
//! Covers the required-for-ship decision table, the baseline cache location,
//! merge-base discovery, green-run event shape, failure-detail scoping, and the
//! pre-existing-failure comparison.

use super::*;
use liberado_coder_sandbox::{PreflightSpec, PreflightStep};

fn required_for(payload: serde_json::Value) -> bool {
    ship_preflight_required_for(&payload)
}

#[test]
fn explore_mode_is_not_a_ship_outcome_even_with_a_project() {
    assert!(!required_for(serde_json::json!({
        "explore_mode": true, "project": "probe"
    })));
}

#[test]
fn plan_mode_via_the_mode_string_is_not_a_ship_outcome() {
    assert!(!required_for(serde_json::json!({
        "mode": "Explore", "project": "probe"
    })));
}

#[test]
fn explicit_required_and_project_names_are_required() {
    assert!(required_for(
        serde_json::json!({ "preflight": { "required": true } })
    ));
    assert!(required_for(serde_json::json!({ "project": "probe" })));
    assert!(required_for(
        serde_json::json!({ "preflight": { "steps": [] } })
    ));
    assert!(!required_for(serde_json::json!({})));
    assert!(!required_for(
        serde_json::json!({ "skip_preflight": true, "project": "p" })
    ));
}

/// The cache lives beside the coding worktrees under the data dir — an empty
/// path would scatter baselines into the process CWD.
#[tokio::test]
async fn the_baseline_cache_is_under_the_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let _env = crate::DATA_DIR_ENV_LOCK.lock().await;
    let prior = std::env::var_os("LIBERADO_DATA_DIR");
    unsafe {
        std::env::set_var("LIBERADO_DATA_DIR", dir.path());
    }
    let resolved = baseline_cache_dir();
    unsafe {
        match prior {
            Some(v) => std::env::set_var("LIBERADO_DATA_DIR", v),
            None => std::env::remove_var("LIBERADO_DATA_DIR"),
        }
    }
    assert_eq!(
        resolved,
        dir.path().join("preflight-baselines"),
        "{resolved:?}"
    );
}

/// A repo whose history is `base <- head`, both on `main`, HEAD checked out.
async fn repo_with_base_and_head() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let out = liberado_common::process::std_command("git")
            .args(["-C", dir.path().to_str().unwrap()])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", "-q", "-b", "main"]);
    run(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "--allow-empty",
        "-qm",
        "base",
    ]);
    let base = String::from_utf8(
        liberado_common::process::std_command("git")
            .args(["-C", dir.path().to_str().unwrap(), "rev-parse", "main"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    // The work itself lives on a branch OFF main, so merge-base(HEAD, main)
    // is the base commit and not simply HEAD.
    run(&["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.path().join("tracked.txt"), "head work\n").unwrap();
    run(&["add", "-A"]);
    run(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "head",
    ]);
    (dir, base)
}

#[tokio::test]
async fn base_commit_finds_the_merge_base_with_main() {
    let (_dir, base) = repo_with_base_and_head().await;
    let found = base_commit(_dir.path()).await;
    assert_eq!(found.as_deref(), Some(base.as_str()));
}

#[tokio::test]
async fn a_repo_without_standard_branches_has_no_base() {
    let dir = tempfile::tempdir().unwrap();
    let out = liberado_common::process::std_command("git")
        .args(["init", "-q", "-b", "feature-x"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = liberado_common::process::std_command("git")
        .args([
            "-C",
            dir.path().to_str().unwrap(),
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--allow-empty",
            "-qm",
            "only",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(base_commit(dir.path()).await, None);
}

async fn drain(events: &mut tokio::sync::mpsc::Receiver<SessionEvent>) -> Vec<SessionEvent> {
    let mut out = Vec::new();
    while let Ok(e) = events.try_recv() {
        out.push(e);
    }
    out
}

/// A green preflight says its piece once: progress in, verdict out, no failure
/// detail for steps that passed.
#[tokio::test]
async fn a_green_run_sends_progress_then_verdict_only() {
    let spec = PreflightSpec::new("probe", vec![PreflightStep::new("always-green", "true")]);
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let dir = tempfile::tempdir().unwrap();
    let report = run_ship_preflight("s", dir.path(), &spec, &tx)
        .await
        .unwrap();
    assert!(report.ok, "{report:?}");
    let events = drain(&mut rx).await;
    assert_eq!(
        events.len(),
        2,
        "green runs must not emit failure detail: {events:?}"
    );
    assert!(!format!("{events:?}").contains("exit="), "{events:?}");
}

/// Failure detail names only the steps that failed; a passing optional step is
/// not accused.
#[tokio::test]
async fn failure_detail_names_only_failing_steps() {
    let spec = PreflightSpec::new(
        "probe",
        vec![
            PreflightStep::new("passes", "true"),
            PreflightStep::new("breaks", "false"),
        ],
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let dir = tempfile::tempdir().unwrap();
    let report = run_ship_preflight("s", dir.path(), &spec, &tx)
        .await
        .expect("preflight ran");
    assert!(!report.ok);
    let events = drain(&mut rx).await;
    let rendered = format!("{events:?}");
    assert!(rendered.contains("breaks"), "{rendered}");
    assert!(
        !rendered.contains("passes: exit"),
        "passing steps must stay out of the detail: {rendered}"
    );
}

/// When the base commit fails the same way, the failures are pre-existing and
/// the run is not blocked by them.
#[tokio::test]
async fn failures_that_already_exist_at_base_do_not_block() {
    let _env = crate::DATA_DIR_ENV_LOCK.lock().await;
    let data = tempfile::tempdir().unwrap();
    // SAFETY-free variant: scoped via guard below instead.
    let (dir, _base) = repo_with_base_and_head().await;
    let spec = PreflightSpec::new("probe", vec![PreflightStep::new("doomed", "false")]);
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);

    let _restore = RestoreDataDir::set_to(data.path());

    let report = run_ship_preflight("s", dir.path(), &spec, &tx)
        .await
        .expect("preflight ran");
    assert!(
        report.ok,
        "a failure that exists at base must not block: {report:?}"
    );
    assert!(
        report.summary.contains("no new failures") && report.summary.contains("pre-existing"),
        "{report:?}"
    );
    let _ = drain(&mut rx).await;
}

struct RestoreDataDir(Option<std::ffi::OsString>);

impl Drop for RestoreDataDir {
    fn drop(&mut self) {
        unsafe {
            match &self.0 {
                Some(v) => std::env::set_var("LIBERADO_DATA_DIR", v),
                None => std::env::remove_var("LIBERADO_DATA_DIR"),
            }
        }
    }
}

impl RestoreDataDir {
    fn set_to(path: &std::path::Path) -> Self {
        let prior = std::env::var_os("LIBERADO_DATA_DIR");
        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", path);
        }
        Self(prior)
    }
}
