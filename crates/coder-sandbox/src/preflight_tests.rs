//! Split from `preflight.rs` for module-health boundaries.

use super::*;

#[tokio::test]
async fn all_green_report_when_every_step_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let spec = PreflightSpec::new(
        "ship",
        vec![
            PreflightStep::new("one", "echo ok1"),
            PreflightStep::new("two", "echo ok2"),
        ],
    );
    let report = run_preflight(dir.path(), &spec).await.unwrap();
    assert!(report.ok, "{:?}", report.summary);
    assert_eq!(report.steps.len(), 2);
    assert!(report.steps.iter().all(|s| s.ok));
    assert!(report.summary.contains("ok"));
    assert_eq!(report.profile_id, "ship");
}

#[tokio::test]
async fn continues_after_required_failure_for_complete_report() {
    let dir = tempfile::tempdir().unwrap();
    let fail = if cfg!(windows) { "exit /B 1" } else { "exit 1" };
    let spec = PreflightSpec::new(
        "ship",
        vec![
            PreflightStep::new("pass", "echo hi"),
            PreflightStep::new("boom", fail),
            PreflightStep::new("later", "echo still-runs"),
        ],
    );
    let report = run_preflight(dir.path(), &spec).await.unwrap();
    assert!(
        !report.ok,
        "overall must stay fail-closed: {:?}",
        report.summary
    );
    assert_eq!(
        report.steps.len(),
        3,
        "must run later steps after a required failure"
    );
    assert!(report.steps[0].ok);
    assert!(!report.steps[1].ok);
    assert!(
        report.steps[2].ok,
        "later diagnostic/action step must still execute"
    );
    assert!(report.summary.contains("boom"));
    assert!(
        report.summary.contains("failed at step(s)"),
        "summary should list failed steps: {}",
        report.summary
    );
}

/// Staged ship bar: early format failure must not hide later test/clippy-style failures.
#[tokio::test]
async fn multi_required_failures_all_appear_in_report() {
    let dir = tempfile::tempdir().unwrap();
    let fail = if cfg!(windows) { "exit /B 1" } else { "exit 1" };
    let spec = PreflightSpec::new(
        "ship",
        vec![
            PreflightStep::new("fmt", fail),
            PreflightStep::new("compile", "echo ok"),
            PreflightStep::new("test", fail),
            PreflightStep::new("clippy", fail),
        ],
    );
    let report = run_preflight(dir.path(), &spec).await.unwrap();
    assert!(!report.ok);
    assert_eq!(report.steps.len(), 4, "all staged steps must run");
    let names: Vec<_> = report.steps.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["fmt", "compile", "test", "clippy"]);
    assert!(!report.steps[0].ok);
    assert!(report.steps[1].ok);
    assert!(!report.steps[2].ok);
    assert!(!report.steps[3].ok);
    let failed = report_failures(&report);
    assert!(failed.contains_key("fmt"), "{failed:?}");
    assert!(failed.contains_key("test"), "{failed:?}");
    assert!(failed.contains_key("clippy"), "{failed:?}");
    assert!(
        !failed.contains_key("compile"),
        "green compile must not appear in failures: {failed:?}"
    );
    assert!(
        report.summary.contains("fmt")
            && report.summary.contains("test")
            && report.summary.contains("clippy"),
        "summary should name every failed step: {}",
        report.summary
    );
}

/// Baseline differential still works when multiple staged steps fail.
#[test]
fn staged_multi_step_failures_preserve_baseline_diff() {
    let mut current = FailureSet::new();
    current.insert(
        "fmt".into(),
        [OPAQUE_FAILURE.to_string()].into_iter().collect(),
    );
    current.insert(
        "test".into(),
        ["gates::new_break".to_string()].into_iter().collect(),
    );
    current.insert(
        "clippy".into(),
        [OPAQUE_FAILURE.to_string()].into_iter().collect(),
    );

    let mut baseline = FailureSet::new();
    baseline.insert(
        "fmt".into(),
        [OPAQUE_FAILURE.to_string()].into_iter().collect(),
    );
    // clippy was already failing on base; test was green
    baseline.insert(
        "clippy".into(),
        [OPAQUE_FAILURE.to_string()].into_iter().collect(),
    );

    let new = diff_against_baseline(&current, &baseline);
    let described = describe_failures(&new);
    assert_eq!(
        described,
        vec!["test: gates::new_break".to_string()],
        "pre-existing fmt/clippy must be ignored; new test failure remains: {described:?}"
    );
}

#[tokio::test]
async fn empty_spec_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let err = run_preflight(dir.path(), &PreflightSpec::new("ship", vec![]))
        .await
        .unwrap_err();
    assert!(matches!(err, PreflightError::EmptySpec));
}

#[tokio::test]
async fn missing_root_is_error() {
    let err = run_preflight(
        Path::new("/nonexistent/preflight/root-xyz"),
        &PreflightSpec::new("ship", vec![PreflightStep::new("x", "echo x")]),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PreflightError::MissingRoot(_)));
}

#[test]
fn resolve_ship_spec_uses_configured_steps() {
    let steps = vec![PreflightStep::new("t", "echo t")];
    let spec = resolve_ship_spec(Some("other"), Some(steps.clone())).unwrap();
    assert_eq!(spec.steps, steps);
}

#[test]
fn resolve_ship_spec_liberado_default_when_empty() {
    let spec = resolve_ship_spec(Some("liberado"), None).unwrap();
    assert_eq!(spec.id, "ship");
    let names: Vec<_> = spec.steps.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["fmt", "compile", "test", "clippy", "deny"]);
    assert!(
        spec.steps
            .iter()
            .any(|s| s.run.contains("cargo test --workspace"))
    );
    assert!(
        spec.steps
            .iter()
            .any(|s| s.run.contains("exclude liberado-webui") && s.run.contains("-D warnings"))
    );
    assert!(
        spec.steps
            .iter()
            .any(|s| s.name == "compile" && s.run.contains("cargo check"))
    );
}

#[test]
fn resolve_ship_spec_none_for_unknown_project_without_config() {
    assert!(resolve_ship_spec(Some("notes"), None).is_none());
    assert!(resolve_ship_spec(None, None).is_none());
}

#[test]
fn default_true_returns_true() {
    assert!(super::default_true());
}

#[test]
fn default_constants_have_expected_values() {
    assert_eq!(super::DEFAULT_STEP_TIMEOUT_SECS, 45 * 60);
    assert_eq!(super::DEFAULT_LOG_CAP_BYTES, 16 * 1024);
}

#[test]
fn cap_log_exact_boundary() {
    assert_eq!(cap_log("hello", 5), "hello");
    assert_eq!(cap_log("hello", 50), "hello");
}

#[test]
fn cap_log_truncation_keeps_last_max_bytes() {
    let s = "abcdefghijklmnop";
    let capped = cap_log(s, 8);
    assert!(capped.starts_with('…'));
    assert_eq!(capped.len(), 11); // 3-byte … + 8 content bytes
}

#[test]
fn cap_log_empty_string() {
    assert_eq!(cap_log("", 8), "");
}

#[test]
fn cap_log_zero_max_returns_ellipsis_only() {
    assert_eq!(cap_log("anything", 0), "…");
}

#[test]
fn cap_log_multibyte_utf8_truncation_safe() {
    // 字 is 3 bytes; truncation near a multi-byte boundary must not panic.
    let s = "ab字defg";
    let capped = cap_log(s, 5);
    assert!(capped.starts_with('…'));
    // 3-byte … + up to 5 bytes content, may be shorter due to char boundary.
    assert!(capped.len() >= 4 && capped.len() <= 8);
}

#[test]
fn cap_log_truncation_at_multibyte_boundary_exercises_scan_loop() {
    // Two 3-byte chars = 6 bytes. Truncating to 5 bytes means start=1,
    // which falls inside the first multi-byte char. The loop must scan
    // forward to the next char boundary at byte 3.
    let s = "字字";
    let capped = cap_log(s, 5);
    assert!(capped.starts_with('…'));
    assert_eq!(capped.len(), 6); // 3-byte … + second 字 (3 bytes)
}

#[test]
fn cap_log_one_byte_shorter_than_input_truncates() {
    // Input is 6 bytes, max is 5 → must truncate (not return as-is).
    assert_ne!(cap_log("abcdef", 5), "abcdef");
    assert!(cap_log("abcdef", 5).starts_with('…'));
}

#[test]
fn resolve_ship_spec_empty_configured_for_non_liberado_is_none() {
    assert!(resolve_ship_spec(Some("notes"), Some(vec![])).is_none());
}

#[test]
fn resolve_ship_spec_empty_configured_for_liberado_falls_back() {
    let spec = resolve_ship_spec(Some("liberado"), Some(vec![])).unwrap();
    assert!(!spec.steps.is_empty());
    assert_eq!(spec.id, "ship");
}

#[test]
fn preflight_step_builder() {
    let s = PreflightStep::new("lint", "cargo clippy").with_timeout_secs(120);
    assert_eq!(s.name, "lint");
    assert_eq!(s.run, "cargo clippy");
    assert_eq!(s.timeout_secs, Some(120));
    assert!(s.required);
}

#[test]
fn preflight_spec_is_empty() {
    assert!(PreflightSpec::new("x", vec![]).is_empty());
    assert!(!PreflightSpec::new("x", vec![PreflightStep::new("y", "z")]).is_empty());
}

#[test]
fn liberado_ship_preflight_steps_structure() {
    let steps = liberado_ship_preflight_steps();
    let names: Vec<_> = steps.iter().map(|s| s.name.as_str()).collect();
    // Stage order: format action → compile → test+clippy diagnostics → deny.
    assert_eq!(names, ["fmt", "compile", "test", "clippy", "deny"]);
    for s in &steps {
        assert!(s.required, "{} must be required", s.name);
        assert!(!s.run.is_empty(), "{} must have a run command", s.name);
    }
    assert!(steps[1].run.contains("cargo check"));
    assert!(steps[2].run.contains("--no-fail-fast"));
}

#[tokio::test]
async fn required_failure_still_runs_following_optional() {
    let dir = tempfile::tempdir().unwrap();
    let fail = if cfg!(windows) { "exit /B 1" } else { "exit 1" };
    let mut optional = PreflightStep::new("soft", fail);
    optional.required = false;
    let spec = PreflightSpec::new(
        "ship",
        vec![PreflightStep::new("hard-fail", fail), optional],
    );
    let report = run_preflight(dir.path(), &spec).await.unwrap();
    assert!(!report.ok, "required step failed, overall must be false");
    assert_eq!(
        report.steps.len(),
        2,
        "optional after a required failure must still run for a complete report"
    );
    assert!(!report.steps[0].ok);
    assert!(!report.steps[1].ok);
}

#[tokio::test]
async fn optional_failure_does_not_fail_profile() {
    let dir = tempfile::tempdir().unwrap();
    let fail = if cfg!(windows) { "exit /B 1" } else { "exit 1" };
    let mut optional = PreflightStep::new("soft", fail);
    optional.required = false;
    let spec = PreflightSpec::new(
        "ship",
        vec![optional, PreflightStep::new("hard", "echo ok")],
    );
    let report = run_preflight(dir.path(), &spec).await.unwrap();
    assert!(report.ok, "{:?}", report);
    assert_eq!(report.steps.len(), 2);
}

#[tokio::test]
async fn step_with_stdout_and_stderr_captures_both() {
    let dir = tempfile::tempdir().unwrap();
    // Produce both stdout and stderr so the combining branches are exercised.
    let cmd = if cfg!(windows) {
        "echo out && echo err 1>&2"
    } else {
        "echo out && echo err >&2"
    };
    let spec = PreflightSpec::new("ship", vec![PreflightStep::new("both", cmd)]);
    let report = run_preflight(dir.path(), &spec).await.unwrap();
    assert!(report.ok);
    let log = &report.steps[0].log_excerpt;
    assert!(log.contains("out"));
    assert!(log.contains("err"));
}
