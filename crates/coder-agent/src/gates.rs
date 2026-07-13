//! Deterministic coding-domain verifiers (backend-owned, not model-owned).
//!
//! These are the coding pack's `Verifier` implementations: real git status, path-independent
//! validation re-run, and mapping progress fatals to terminal errors. They intentionally do **not**
//! go through model-facing command policy.

use chrono::Utc;
use liberado_coder_core::{CoderCommandConfig, CoderError, CoderEvent, CoderRunRequest};
use liberado_coder_sandbox::CommandRequest;
use liberado_common::Outcome;
use tokio::process::Command;

use crate::progress::ProgressFatal;
use crate::trace::{self, EventLog};

pub fn command_request(command: &CoderCommandConfig) -> CommandRequest {
    CommandRequest {
        program: command.program.clone(),
        args: command.args.clone(),
        env: command.env.clone(),
        timeout_secs: command.timeout_secs,
        output_max_bytes: command.output_max_bytes,
    }
}

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
    let output = Command::new("git")
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
}
