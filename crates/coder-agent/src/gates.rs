//! Deterministic coding-domain verifiers (backend-owned, not model-owned).
//!
//! These are the coding pack's `Verifier` implementations: real git status, path-independent
//! validation re-run, and mapping progress fatals to terminal errors. They intentionally do **not**
//! go through model-facing command policy.

use chrono::Utc;
use liberado_coder_core::{CoderCommandConfig, CoderError, CoderEvent, CoderRunRequest};
use liberado_coder_sandbox::CommandRequest;
use liberado_common::Outcome;

use crate::progress::ProgressFatal;
use crate::trace::{self, EventLog};

pub fn command_request(command: &CoderCommandConfig) -> CommandRequest {
    CommandRequest {
        program: command.program.clone(),
        args: command.args.clone(),
        env: command.env.clone(),
        timeout_secs: command.timeout_secs,
        output_max_bytes: command.output_max_bytes,
        // Backend gates, not model tool results: keep head truncation.
        offload_dir: None,
    }
}

/// Maps a latched progress fatal to a terminal error. Compare 7 stopped applying this
/// after a filed report — the ship bar is the scorekeeper. Kept so a future in-loop
/// terminal has one place for the mapping.
#[allow(dead_code)]
pub async fn fail_with_progress_fatal(
    request: &CoderRunRequest,
    session_id: &str,
    events: &EventLog,
    fatal: ProgressFatal,
) -> CoderError {
    trace::push_event(
        events,
        CoderEvent::LoopGuardTriggered {
            guard: fatal.guard_name().to_string(),
            action: "fail_run".to_string(),
            at: Utc::now(),
        },
    );
    trace::push_event(
        events,
        CoderEvent::SessionFinished {
            outcome: Outcome::Failed,
            at: Utc::now(),
        },
    );
    let _ = trace::write_trace(request, session_id, trace::snapshot_events(events), None).await;
    match fatal {
        ProgressFatal::ReadOnlyStall { .. } | ProgressFatal::SameToolChurn { .. } => {
            CoderError::NoChanges
        }
        ProgressFatal::ValidationChurn { .. } => CoderError::Validation(fatal.message()),
    }
}

// (The legacy single-command `run_validation_gate`/`validation_summary` pair lived here until
// 2026-07-11 — superseded by `verify_pipeline`, which synthesizes a one-element pipeline from a
// bare `validation_command` (verifiers.md §5.3) — deleted once nothing referenced them.)

pub async fn changed_files(workspace_root: &str) -> Result<Vec<String>, CoderError> {
    // `-uall` lists files inside new untracked dirs (`src/main.rs`) instead of only `src/`.
    //
    // The `-- .` pathspec is load-bearing, not decoration. If `workspace_root` is not itself a git
    // repo, git walks *up* to the nearest enclosing `.git` and happily reports that repo's dirty
    // files — so a sandboxed session created under, say, `<repo>/.liberado/goal-workspaces/…` would
    // claim it changed files elsewhere in the user's checkout that it never touched. Scoping to the
    // current directory subtree means the answer can never name a file outside the workspace,
    // whatever repo happens to enclose it.
    let output = liberado_common::process::command("git")
        .args(["status", "--porcelain", "-uall", "--", "."])
        .current_dir(workspace_root)
        .output()
        .await
        .map_err(|e| CoderError::Backend(format!("git status: {e}")))?;
    if !output.status.success() {
        return Err(CoderError::Backend(format!(
            "git status exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().filter_map(parse_status_path).collect())
}

/// Like [`changed_files`], but each entry carries how the file changed
/// (`added` | `modified` | `deleted`) for the `file_changed` wire event.
pub async fn changed_files_detailed(
    workspace_root: &str,
) -> Result<Vec<(String, &'static str)>, CoderError> {
    let output = liberado_common::process::command("git")
        .args(["status", "--porcelain", "-uall", "--", "."])
        .current_dir(workspace_root)
        .output()
        .await
        .map_err(|e| CoderError::Backend(format!("git status: {e}")))?;
    if !output.status.success() {
        return Err(CoderError::Backend(format!(
            "git status exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter_map(|line| parse_status_path(line).map(|p| (p, parse_status_change(line))))
        .collect())
}

/// Classify a porcelain status line's XY code.
///
/// Deliberately coarse — three buckets, because that is all a surface renders. A rename reports as
/// `added`: `parse_status_path` already resolves `old -> new` to the new path, so from the reader's
/// point of view that path is new. Anything unrecognized falls back to `modified`, the safest
/// wrong answer: it says "this file was touched" without claiming it appeared or vanished.
pub fn parse_status_change(line: &str) -> &'static str {
    let code = line.get(..2).unwrap_or("");
    match code {
        "??" => "added",
        c if c.starts_with('D') || c.ends_with('D') => "deleted",
        c if c.starts_with('A') || c.starts_with('R') || c.starts_with('C') => "added",
        _ => "modified",
    }
}

pub fn parse_status_path(line: &str) -> Option<String> {
    if line.len() < 4 {
        return None;
    }
    let path = line.get(3..)?.trim();
    if path.is_empty() {
        return None;
    }
    let path = path
        .rsplit_once(" -> ")
        .map(|(_, new_path)| new_path)
        .unwrap_or(path);
    Some(path.trim_matches('"').to_string())
}

/// Resolve `rev` to a full SHA in `workspace_root` (e.g. `"HEAD"`).
pub async fn rev_parse(workspace_root: &str, rev: &str) -> Result<String, CoderError> {
    let output = liberado_common::process::command("git")
        .args(["rev-parse", rev])
        .current_dir(workspace_root)
        .output()
        .await
        .map_err(|e| CoderError::Backend(format!("git rev-parse: {e}")))?;
    if !output.status.success() {
        return Err(CoderError::Backend(format!(
            "git rev-parse {rev} exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Files changed by commits after `baseline_sha` (exclusive) through `HEAD`.
///
/// Used when the working tree is clean after `git_commit`: porcelain status is empty, but the
/// attempt still produced real work. The baseline comparison preserves that progress signal.
pub async fn committed_files_since(
    workspace_root: &str,
    baseline_sha: &str,
) -> Result<Vec<(String, &'static str)>, CoderError> {
    let head = rev_parse(workspace_root, "HEAD").await?;
    if head == baseline_sha {
        return Ok(Vec::new());
    }
    // Two-arg form: tree of baseline vs tree of HEAD (works when baseline is an ancestor).
    let output = liberado_common::process::command("git")
        .args(["diff", "--name-status", baseline_sha, &head])
        .current_dir(workspace_root)
        .output()
        .await
        .map_err(|e| CoderError::Backend(format!("git diff name-status: {e}")))?;
    if !output.status.success() {
        return Err(CoderError::Backend(format!(
            "git diff --name-status exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().filter_map(parse_name_status_line).collect())
}

fn parse_name_status_line(line: &str) -> Option<(String, &'static str)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let mut parts = line.split('\t');
    let code = parts.next()?.trim();
    let path = parts.next()?.trim();
    if path.is_empty() {
        return None;
    }
    // Renames: R100\told\tnew — take the new path.
    let path = if code.starts_with('R') || code.starts_with('C') {
        parts.next().unwrap_or(path).trim()
    } else {
        path
    };
    let change = match code.chars().next().unwrap_or('M') {
        'A' => "added",
        'D' => "deleted",
        'R' | 'C' => "added",
        _ => "modified",
    };
    Some((path.to_string(), change))
}

/// Uncommitted porcelain changes, or — if the tree is clean — files introduced by commits after
/// `baseline_sha` (the SHA recorded at attempt start). Either counts as real workspace progress.
pub async fn resolve_attempt_changes(
    workspace_root: &str,
    baseline_sha: Option<&str>,
) -> Result<Vec<(String, &'static str)>, CoderError> {
    let uncommitted = changed_files_detailed(workspace_root).await?;
    if !uncommitted.is_empty() {
        return Ok(uncommitted);
    }
    let Some(baseline) = baseline_sha.filter(|s| !s.is_empty()) else {
        return Ok(Vec::new());
    };
    committed_files_since(workspace_root, baseline).await
}

/// Failing test identities at `baseline_sha`.
///
/// Empty set means "unknown" and every current failure is treated as new — never as green.
/// A compute error (missing git, worktree add failed) takes that same path. Softening only
/// happens when we have named identities that match.
///
/// On a cache miss this runs the ship-bar **test** step in a throwaway worktree at the base
/// commit (`compute_baseline`). Path-deps are copied in, not junctioned. The agent's tree is
/// not stashed or checked out. Compare 5's `stdio_smoke` was already red on the base; without
/// this compute the headless bar treated it as new and burned two repair attempts.
pub async fn baseline_test_failures(
    workspace_root: &str,
    baseline_sha: &str,
) -> Result<std::collections::BTreeSet<String>, CoderError> {
    if baseline_sha.is_empty() {
        return Ok(std::collections::BTreeSet::new());
    }
    let workspace = std::path::Path::new(workspace_root);
    let cache_dir = workspace.join(".liberado/preflight-baselines");
    let spec = liberado_coder_sandbox::liberado_ship_preflight_spec();
    let mut steps = std::collections::BTreeSet::new();
    steps.insert("test".to_string());
    let target_owned = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"));
    let opts = liberado_coder_sandbox::BaselineOptions {
        project_root: workspace,
        base_sha: baseline_sha,
        cache_dir: &cache_dir,
        target_dir: Some(target_owned.as_path()),
    };
    let set = match liberado_coder_sandbox::compute_baseline(&opts, &spec, &steps).await {
        Ok(set) => set,
        Err(e) => {
            tracing::warn!(
                error = %e,
                baseline = %baseline_sha,
                "baseline compute failed; treating every test failure as new"
            );
            return Ok(std::collections::BTreeSet::new());
        }
    };
    Ok(set
        .values()
        .flatten()
        .filter(|f| *f != liberado_coder_sandbox::OPAQUE_FAILURE)
        .cloned()
        .collect())
}

#[cfg(test)]
mod changed_files_tests {
    use super::changed_files;

    fn unique() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?} failed in {}", dir.display());
    }

    /// `git init` plus a commit identity.
    ///
    /// A bare `git init` is not enough to commit in: a clean CI runner has no global `user.email`
    /// or `user.name`, so `git commit` exits non-zero there while passing on any dev machine that
    /// has them set. Every test in this module that commits must go through here — two of them
    /// were written without the identity lines and only failed once CI could actually run.
    fn init_repo_with_identity(dir: &std::path::Path) {
        git(dir, &["init", "--quiet"]);
        git(dir, &["config", "user.email", "test@liberado.local"]);
        git(dir, &["config", "user.name", "liberado-test"]);
    }

    /// The escape this guards against: a session workspace that is **not itself a repo** but sits
    /// inside one (exactly what `<repo>/.liberado/goal-workspaces/…` is, since `data_dir()` is a
    /// relative path). `git status` there walks *up* to the enclosing repo, so without the `-- .`
    /// pathspec the session reports files from the user's checkout that it never touched — and
    /// those bogus artifacts get folded into the parent conversation by the return handoff.
    #[tokio::test]
    async fn a_workspace_nested_in_another_repo_never_reports_that_repo_s_files() {
        let root = std::env::temp_dir().join(format!("lib-gates-{}", unique()));
        let nested = root.join("nested-workspace");
        std::fs::create_dir_all(&nested).unwrap();

        // An enclosing repo with a dirty file of its own.
        git(&root, &["init", "--quiet"]);
        std::fs::write(root.join("outer-file.txt"), "the user's own work").unwrap();

        // The nested workspace is NOT a repo; it has its own file.
        std::fs::write(nested.join("inner.rs"), "fn main() {}").unwrap();

        let changed = changed_files(nested.to_str().unwrap()).await.unwrap();
        assert!(
            !changed.iter().any(|f| f.contains("outer-file")),
            "a session must never claim it changed a file outside its workspace: {changed:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The paired positive case: once the workspace is its own repo (what `init_git_repo` ensures
    /// for session workspaces), its own new files *are* reported.
    #[tokio::test]
    async fn a_workspace_that_is_its_own_repo_reports_its_own_files() {
        let ws = std::env::temp_dir().join(format!("lib-gates-own-{}", unique()));
        std::fs::create_dir_all(&ws).unwrap();
        git(&ws, &["init", "--quiet"]);
        std::fs::write(ws.join("main.rs"), "fn main() {}").unwrap();

        let changed = changed_files(ws.to_str().unwrap()).await.unwrap();
        assert!(
            changed.iter().any(|f| f.contains("main.rs")),
            "the workspace's own new file should be reported: {changed:?}"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    /// Dogfood finding #3: after `git commit`, porcelain is empty but commits since baseline count.
    #[tokio::test]
    async fn committed_work_since_baseline_counts_as_attempt_changes() {
        use super::{resolve_attempt_changes, rev_parse};

        let ws = std::env::temp_dir().join(format!("lib-gates-commit-{}", unique()));
        std::fs::create_dir_all(&ws).unwrap();
        init_repo_with_identity(&ws);
        std::fs::write(ws.join("seed.txt"), "seed").unwrap();
        git(&ws, &["add", "seed.txt"]);
        git(&ws, &["commit", "-m", "seed", "--quiet"]);
        let baseline = rev_parse(ws.to_str().unwrap(), "HEAD").await.unwrap();

        std::fs::write(ws.join("feature.txt"), "dogfood").unwrap();
        git(&ws, &["add", "feature.txt"]);
        git(&ws, &["commit", "-m", "feature", "--quiet"]);

        // Working tree is clean — porcelain would be empty.
        let porcelain = changed_files(ws.to_str().unwrap()).await.unwrap();
        assert!(
            porcelain.is_empty(),
            "expected clean tree after commit, got {porcelain:?}"
        );

        let resolved = resolve_attempt_changes(ws.to_str().unwrap(), Some(&baseline))
            .await
            .unwrap();
        assert!(
            resolved.iter().any(|(p, _)| p.contains("feature.txt")),
            "committed feature.txt must count as attempt progress: {resolved:?}"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    /// No baseline means nothing to diff against — return empty even if there are uncommitted changes.
    #[tokio::test]
    async fn resolve_attempt_changes_with_none_baseline_returns_empty_when_clean() {
        use super::resolve_attempt_changes;

        let ws = std::env::temp_dir().join(format!("lib-gates-none-base-{}", unique()));
        std::fs::create_dir_all(&ws).unwrap();
        init_repo_with_identity(&ws);
        std::fs::write(ws.join("readme.md"), "base\n").unwrap();
        git(&ws, &["add", "readme.md"]);
        git(&ws, &["commit", "-m", "base", "--quiet"]);
        // Tree is clean, no baseline — should get empty.
        let resolved = resolve_attempt_changes(ws.to_str().unwrap(), None)
            .await
            .unwrap();
        assert!(resolved.is_empty());
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// Empty string baseline is treated like None — returns empty.
    #[tokio::test]
    async fn resolve_attempt_changes_with_empty_string_baseline_returns_empty() {
        use super::resolve_attempt_changes;

        let ws = std::env::temp_dir().join(format!("lib-gates-empty-base-{}", unique()));
        std::fs::create_dir_all(&ws).unwrap();
        init_repo_with_identity(&ws);
        std::fs::write(ws.join("readme.md"), "base\n").unwrap();
        git(&ws, &["add", "readme.md"]);
        git(&ws, &["commit", "-m", "base", "--quiet"]);
        let resolved = resolve_attempt_changes(ws.to_str().unwrap(), Some(""))
            .await
            .unwrap();
        assert!(resolved.is_empty());
        let _ = std::fs::remove_dir_all(&ws);
    }
}

#[cfg(test)]
mod baseline_test_failures_tests {
    use super::baseline_test_failures;

    fn unique() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[tokio::test]
    async fn an_empty_sha_returns_no_identities() {
        let got = baseline_test_failures("/tmp/unused", "")
            .await
            .expect("empty sha is not an error");
        assert!(got.is_empty());
    }

    /// Cache hit: do not need git or cargo. Compare 5 would have used this after the first
    /// compute paid for `stdio_smoke`.
    #[tokio::test]
    async fn a_cached_baseline_yields_named_failures() {
        let ws = std::env::temp_dir().join(format!("lib-gates-bl-cache-{}", unique()));
        std::fs::create_dir_all(ws.join(".liberado/preflight-baselines")).unwrap();
        let mut set = liberado_coder_sandbox::FailureSet::new();
        set.insert(
            "test".into(),
            ["initialize_and_session_new_over_stdio".to_string()]
                .into_iter()
                .collect(),
        );
        liberado_coder_sandbox::store_baseline(
            &ws.join(".liberado/preflight-baselines"),
            "deadbeefcafe00",
            &set,
        );

        let got = baseline_test_failures(ws.to_str().unwrap(), "deadbeefcafe00")
            .await
            .expect("cache hit");
        assert!(
            got.contains("initialize_and_session_new_over_stdio"),
            "got {got:?}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// Opaque identities are not softening keys — a red baseline that did not name a test
    /// must not forgive every cargo-test 101.
    #[tokio::test]
    async fn an_opaque_cached_failure_is_not_a_named_identity() {
        let ws = std::env::temp_dir().join(format!("lib-gates-bl-opaque-{}", unique()));
        std::fs::create_dir_all(ws.join(".liberado/preflight-baselines")).unwrap();
        let mut set = liberado_coder_sandbox::FailureSet::new();
        set.insert(
            "test".into(),
            [liberado_coder_sandbox::OPAQUE_FAILURE.to_string()]
                .into_iter()
                .collect(),
        );
        liberado_coder_sandbox::store_baseline(
            &ws.join(".liberado/preflight-baselines"),
            "cafebabedead00",
            &set,
        );

        let got = baseline_test_failures(ws.to_str().unwrap(), "cafebabedead00")
            .await
            .expect("opaque cache");
        assert!(
            got.is_empty(),
            "opaque must not soften named tests, got {got:?}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// Compute cannot run here (not a git repo). Fail closed: empty set, not an error that
    /// a caller could mistake for "base is green".
    #[tokio::test]
    async fn a_failed_compute_treats_every_failure_as_new() {
        let ws = std::env::temp_dir().join(format!("lib-gates-bl-nocompute-{}", unique()));
        std::fs::create_dir_all(&ws).unwrap();
        let got = baseline_test_failures(ws.to_str().unwrap(), "abc123abc123")
            .await
            .expect("compute error is not a CoderError");
        assert!(got.is_empty(), "fail-closed, got {got:?}");
        let _ = std::fs::remove_dir_all(&ws);
    }
}

#[cfg(test)]
mod status_change_tests {
    use super::parse_status_change;

    #[test]
    fn classifies_porcelain_codes() {
        assert_eq!(parse_status_change("?? src/new.rs"), "added");
        assert_eq!(parse_status_change("A  src/new.rs"), "added");
        assert_eq!(parse_status_change(" M src/lib.rs"), "modified");
        assert_eq!(parse_status_change("M  src/lib.rs"), "modified");
        assert_eq!(parse_status_change("MM src/lib.rs"), "modified");
        assert_eq!(parse_status_change(" D src/gone.rs"), "deleted");
        assert_eq!(parse_status_change("D  src/gone.rs"), "deleted");
        // A rename resolves to its new path, so it reads as an addition to a surface.
        assert_eq!(parse_status_change("R  old.rs -> new.rs"), "added");
    }

    #[test]
    fn unknown_codes_fall_back_to_modified() {
        // "touched" is the safe wrong answer — it never claims a file appeared or vanished.
        assert_eq!(parse_status_change("XY weird.rs"), "modified");
        assert_eq!(parse_status_change(""), "modified");
    }
}

#[cfg(test)]
mod name_status_line_tests {
    use super::parse_name_status_line;

    #[test]
    fn parses_added_file() {
        let (path, change) = parse_name_status_line("A\tsrc/new.rs").unwrap();
        assert_eq!(path, "src/new.rs");
        assert_eq!(change, "added");
    }

    #[test]
    fn parses_deleted_file() {
        let (path, change) = parse_name_status_line("D\told.rs").unwrap();
        assert_eq!(path, "old.rs");
        assert_eq!(change, "deleted");
    }

    #[test]
    fn parses_modified_file() {
        let (path, change) = parse_name_status_line("M\tlib.rs").unwrap();
        assert_eq!(path, "lib.rs");
        assert_eq!(change, "modified");
    }

    #[test]
    fn parses_rename_takes_new_path() {
        let (path, change) = parse_name_status_line("R100\told.rs\tnew.rs").unwrap();
        assert_eq!(path, "new.rs");
        assert_eq!(change, "added");
    }

    #[test]
    fn parses_copy_takes_destination() {
        let (path, change) = parse_name_status_line("C80\torig.rs\tcopy.rs").unwrap();
        assert_eq!(path, "copy.rs");
        assert_eq!(change, "added");
    }

    #[test]
    fn empty_line_is_none() {
        assert!(parse_name_status_line("").is_none());
        assert!(parse_name_status_line("  ").is_none());
    }

    #[test]
    fn unknown_code_defaults_to_modified() {
        let (path, change) = parse_name_status_line("XY\tweird.rs").unwrap();
        assert_eq!(path, "weird.rs");
        assert_eq!(change, "modified");
    }

    #[test]
    fn malformed_line_without_tab_is_none() {
        assert!(parse_name_status_line("M").is_none());
    }

    #[test]
    fn trailing_empty_segment_is_none() {
        // code + tab + empty path
        assert!(parse_name_status_line("M\t").is_none());
    }
}

#[cfg(test)]
mod command_request_tests {
    use super::command_request;
    use liberado_coder_core::CoderCommandConfig;

    #[test]
    fn builds_from_command_config() {
        let mut env = std::collections::BTreeMap::new();
        env.insert("RUST_LOG".into(), "debug".into());
        let config = CoderCommandConfig {
            program: "cargo".into(),
            args: vec!["test".into(), "--lib".into()],
            env,
            timeout_secs: Some(300),
            output_max_bytes: Some(65536),
        };
        let req = command_request(&config);
        assert_eq!(req.program, "cargo");
        assert_eq!(req.args, vec!["test", "--lib"]);
        assert_eq!(req.env.get("RUST_LOG").unwrap(), "debug");
        assert_eq!(req.timeout_secs, Some(300));
        assert_eq!(req.output_max_bytes, Some(65536));
    }

    #[test]
    fn command_request_without_timeout_or_cap() {
        let config = CoderCommandConfig::new("echo");
        let req = command_request(&config);
        assert_eq!(req.program, "echo");
        assert!(req.args.is_empty());
        assert!(req.timeout_secs.is_none());
        assert!(req.output_max_bytes.is_none());
    }
}

#[cfg(test)]
#[path = "gates_survivor_tests.rs"]
mod survivor_tests;
