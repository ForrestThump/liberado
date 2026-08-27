//! Split from `coding_run.rs` for module-health boundaries.

use super::*;
use liberado_common::Outcome;

/// A deployment declaring one project rooted at `root`, with `steps` as its ship bar.
fn config_dir_for(root: &Path, steps: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("topology.toml"),
        format!(
            "vault_path = \"/tmp/vault\"\n\n\
                 [[projects]]\n\
                 name = \"fixture\"\n\
                 root = {root}\n\n\
                 [projects.preflight.ship]\n\
                 steps = [{steps}]\n",
            root = toml_path(root),
        ),
    )
    .expect("write topology");
    dir
}

/// TOML basic-string escaping. Windows roots are full of backslashes, and an unescaped one
/// makes the fixture fail to parse — which reads as "no project matched" and would pass the
/// tests below for entirely the wrong reason.
fn toml_path(p: &Path) -> String {
    let escaped = p.display().to_string().replace('\\', "\\\\");
    format!("\"{escaped}\"")
}

#[test]
fn a_declared_project_supplies_the_payload_the_gate_reads() {
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = config_dir_for(root.path(), "{ name = \"ok\", run = \"exit 0\" }");

    let payload = ship_preflight_payload(Some(cfg.path()), root.path());
    assert_eq!(payload["project"], "fixture");
    assert_eq!(payload["preflight"]["steps"][0]["name"], "ok");
    assert!(
        liberado_coder_agent::ship_preflight::ship_preflight_required_for(&payload),
        "a declared project with ship steps must require the bar"
    );
}

/// A subdirectory of a declared root is still that project — the client's cwd is routinely
/// deeper than the root someone wrote in topology.toml.
#[test]
fn a_subdirectory_of_a_declared_root_resolves_to_that_project() {
    let root = tempfile::tempdir().expect("tempdir");
    let nested = root.path().join("crates").join("thing");
    std::fs::create_dir_all(&nested).expect("nested");
    let cfg = config_dir_for(root.path(), "{ name = \"ok\", run = \"exit 0\" }");

    let payload = ship_preflight_payload(Some(cfg.path()), &nested);
    assert_eq!(payload["project"], "fixture");
}

/// An undeclared directory gets no bar rather than an invented one. Running someone else's
/// repo through liberado's cargo steps would fail for reasons that say nothing about it.
#[test]
fn an_undeclared_directory_has_no_ship_bar() {
    let declared = tempfile::tempdir().expect("tempdir");
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let cfg = config_dir_for(declared.path(), "{ name = \"ok\", run = \"exit 0\" }");

    let payload = ship_preflight_payload(Some(cfg.path()), elsewhere.path());
    assert!(
        !liberado_coder_agent::ship_preflight::ship_preflight_required_for(&payload),
        "an undeclared root must not acquire a bar: {payload}"
    );
}

#[tokio::test]
async fn a_failing_ship_bar_takes_the_success_away() {
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = config_dir_for(root.path(), "{ name = \"bar\", run = \"exit 3\" }");

    let (outcome, summary) = apply_ship_bar(
        Outcome::Succeeded,
        "the model says it is done".into(),
        root.path(),
        root.path(),
        Some(cfg.path()),
        None,
    )
    .await;

    assert_eq!(
        outcome,
        Outcome::Failed,
        "a round that cannot clear the ship bar is not a success: {summary}"
    );
    assert!(
        summary.contains("ship preflight"),
        "the reason must reach the summary, which is what the next round is told: {summary}"
    );
}

#[tokio::test]
async fn a_passing_ship_bar_leaves_the_success_alone() {
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = config_dir_for(root.path(), "{ name = \"bar\", run = \"exit 0\" }");

    let (outcome, _) = apply_ship_bar(
        Outcome::Succeeded,
        "done".into(),
        root.path(),
        root.path(),
        Some(cfg.path()),
        None,
    )
    .await;

    assert_eq!(outcome, Outcome::Succeeded);
}

/// A round that already failed is returned untouched. Gating it would spend a full CI run to
/// confirm what is already known, on the path where the agent has the least budget left.
#[tokio::test]
async fn a_failed_round_is_not_put_through_the_bar() {
    let root = tempfile::tempdir().expect("tempdir");
    // A step that would fail if it ran at all.
    let cfg = config_dir_for(root.path(), "{ name = \"bar\", run = \"exit 3\" }");

    let (outcome, summary) = apply_ship_bar(
        Outcome::Failed,
        "already failed".into(),
        root.path(),
        root.path(),
        Some(cfg.path()),
        None,
    )
    .await;

    assert_eq!(outcome, Outcome::Failed);
    assert_eq!(
        summary, "already failed",
        "the bar must not rewrite a summary it never ran against"
    );
}

/// Standalone: no config dir, so no topology, so no bar — and the round stands as the pack
/// reported it rather than being failed for the absence of a deployment.
#[tokio::test]
async fn a_standalone_run_keeps_its_outcome() {
    let root = tempfile::tempdir().expect("tempdir");
    let (outcome, summary) = apply_ship_bar(
        Outcome::Succeeded,
        "done".into(),
        root.path(),
        root.path(),
        None,
        None,
    )
    .await;
    assert_eq!(outcome, Outcome::Succeeded);
    assert_eq!(summary, "done");
}
