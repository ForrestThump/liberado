//! Full Liberado coding pack engine for ACP sessions.
//!
//! This is **not** the face-agent chat path (`ChatSessions` + `delegate`). It is the same
//! [`LiberadoLoopBackend`] the daemon coding pack and `liberado-coder-run` use: durable
//! worktrees, progress guards, optional gate/repair, coding tools, traces.
//!
//! Multi-turn Paseo chat maps to sequential coding runs: each `session/prompt` is one
//! `CoderRunRequest` (with optional prior-feedback from earlier rounds in this ACP session).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use liberado_coder_agent::{CoderProviderFactory, LiberadoLoopBackend, SingleProviderFactory};
use liberado_coder_core::{
    CoderBackend, CoderRoleConfig, CoderRunConfig, CoderRunRequest, CoderTask, CoderTuning,
    LIBERADO_LOOP_BACKEND, PathPolicy, ProgressPolicy, SandboxSpec, TraceFormat, WorkspaceRef,
};
use liberado_coder_sandbox::ensure_session_worktree;
use liberado_coder_tools::coding_worktrees_base;
use liberado_provider::Provider;
use serde_json::json;

/// Per-ACP-session memory of prior coding rounds (for repair-style feedback continuity).
#[derive(Debug, Default, Clone)]
pub struct CodingSessionState {
    pub cwd: PathBuf,
    /// Liberado session/worktree id (stable across prompts in one ACP session).
    pub coding_session_id: String,
    pub prior_feedback: Vec<String>,
    pub last_summary: Option<String>,
    pub rounds: u32,
}

/// How many model turns the **coder role** may use inside one `session/prompt`.
///
/// This is *not* the face-agent `Budget::default()` (8). It is the coding pack's `max_turns`
/// on [`CoderRoleConfig`] — the same knob as `[coder.coder] max_turns` / headless runner.
pub fn max_turns_from_env() -> u32 {
    std::env::var("LIBERADO_ACP_MAX_TURNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(50)
}

/// Build coding pack tunables from Liberado config when available.
pub fn load_coder_tuning(config_dir: Option<&Path>) -> CoderTuning {
    let Some(dir) = config_dir else {
        return CoderTuning::default();
    };
    match liberado_config::load_config(Some(dir)) {
        Ok((config, _)) => match CoderTuning::from_value(config.tuning.coder.as_ref()) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "invalid [coder] tuning; using defaults");
                CoderTuning::default()
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "config load failed for coding pack; using defaults");
            CoderTuning::default()
        }
    }
}

/// Run one full coding pack attempt for `description` in `state.cwd`.
///
/// Creates/reuses a durable worktree under `coding-worktrees/<coding_session_id>` when `cwd` is a
/// git repo (same as `CodingSessionPack` build phase).
pub async fn run_coding_round(
    provider: Arc<dyn Provider>,
    factory: Arc<dyn CoderProviderFactory>,
    tuning: &CoderTuning,
    state: &mut CodingSessionState,
    description: &str,
    model_override: Option<&str>,
    max_turns: u32,
) -> Result<CodingRoundOutcome, String> {
    let model = model_override
        .map(str::to_string)
        .unwrap_or_else(|| provider.model());

    let workspace = prepare_workspace(&state.cwd, &state.coding_session_id).await?;

    let mut coder_role = tuning.coder.clone();
    if !model.is_empty() {
        coder_role.model = model.clone();
    }
    coder_role.max_turns = Some(max_turns);
    // Repair mirrors coder when present.
    let repair = tuning.repair.clone().map(|mut r| {
        if !model.is_empty() {
            r.model = model.clone();
        }
        r.max_turns = Some(max_turns);
        r
    });

    let mut task = CoderTask::new(&state.coding_session_id, description);
    if let Some(prev) = &state.last_summary {
        task = task.with_context(format!(
            "Prior coding round summary (round {}):\n{prev}",
            state.rounds
        ));
    }

    let request = CoderRunRequest {
        task,
        workspace: WorkspaceRef::new(workspace.to_string_lossy(), "HEAD"),
        config: CoderRunConfig {
            backend: LIBERADO_LOOP_BACKEND.into(),
            trace_dir: tuning.trace_dir.clone().or_else(|| {
                Some(
                    PathBuf::from(
                        std::env::var("LIBERADO_DATA_DIR").unwrap_or_else(|_| ".liberado".into()),
                    )
                    .join("coder-traces")
                    .to_string_lossy()
                    .into_owned(),
                )
            }),
            trace_formats: if tuning.trace_formats.is_empty() {
                vec![TraceFormat::Native]
            } else {
                tuning.trace_formats.clone()
            },
            planner: disabled_role(&model),
            coder: coder_role,
            critic: disabled_role(&model),
            gate: tuning.gate.clone(),
            repair,
            // Durable worktree already materialised; HostLocal on that tree (same as pack build).
            sandbox: SandboxSpec::HostLocal,
            command_policy: tuning.command_policy.clone(),
            validation_command: tuning.validation_command.clone(),
            verifiers: tuning.verifiers.clone(),
            verify_policy: tuning.verify_policy.clone(),
            path_policy: if tuning.path_policy.allow_write_globs.is_empty() {
                PathPolicy::default()
            } else {
                tuning.path_policy.clone()
            },
            progress: ProgressPolicy {
                // Prefer pack progress knobs; never use executor DEFAULT_MAX_TURNS (8).
                ..tuning.progress.clone()
            },
            hashline: tuning.hashline.clone(),
        },
        attempt: state.rounds,
        prior_feedback: state.prior_feedback.clone(),
        strategist_directive: None,
    };

    tracing::info!(
        session = %state.coding_session_id,
        workspace = %workspace.display(),
        %model,
        max_turns,
        round = state.rounds,
        "coding pack run starting"
    );

    let backend = LiberadoLoopBackend::with_provider_factory(factory);
    let result = backend
        .run(request)
        .await
        .map_err(|e| format!("coding pack failed: {e}"))?;

    state.rounds = state.rounds.saturating_add(1);
    state.last_summary = Some(result.summary.clone());
    if !matches!(
        result.outcome,
        liberado_common::Outcome::Succeeded | liberado_common::Outcome::PartiallySucceeded
    ) {
        state
            .prior_feedback
            .push(format!("Previous attempt: {}", result.summary));
    }

    Ok(CodingRoundOutcome {
        summary: result.summary,
        outcome: format!("{:?}", result.outcome),
        files_changed: result.files_changed,
        workspace: workspace.display().to_string(),
        trace_path: result.trace_path,
        validation_notes: result.validation_notes,
    })
}

#[derive(Debug, Clone)]
pub struct CodingRoundOutcome {
    pub summary: String,
    pub outcome: String,
    pub files_changed: Vec<String>,
    pub workspace: String,
    pub trace_path: Option<String>,
    pub validation_notes: Option<String>,
}

impl CodingRoundOutcome {
    /// Human-readable report for ACP `agent_message_chunk` stream.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("## Coding pack result: {}\n\n", self.outcome));
        out.push_str(&self.summary);
        out.push_str("\n\n");
        out.push_str(&format!("**Workspace:** `{}`\n", self.workspace));
        if !self.files_changed.is_empty() {
            out.push_str("\n**Files changed:**\n");
            for f in &self.files_changed {
                out.push_str(&format!("- `{f}`\n"));
            }
        }
        if let Some(v) = &self.validation_notes {
            out.push_str(&format!("\n**Validation:** {v}\n"));
        }
        if let Some(t) = &self.trace_path {
            out.push_str(&format!("\n**Trace:** `{t}`\n"));
        }
        out
    }
}

async fn prepare_workspace(cwd: &Path, session_id: &str) -> Result<PathBuf, String> {
    if !cwd.is_dir() {
        return Err(format!("workspace is not a directory: {}", cwd.display()));
    }
    // Durable coding-worktrees/<id> when git; else host cwd.
    // Fail hard on worktree setup — never silently demote to the live tree
    // (matches CodingSessionPack::build; silent HostLocal would edit the user's branch).
    if is_git_repo(cwd) {
        let base = coding_worktrees_base();
        match ensure_session_worktree(cwd, session_id, &base).await {
            Ok(path) => {
                tracing::info!(
                    session = %session_id,
                    worktree = %path.display(),
                    "durable coding worktree ready"
                );
                Ok(path)
            }
            Err(e) => Err(format!(
                "durable session worktree setup failed (no live-tree fallback): {e}"
            )),
        }
    } else {
        Ok(cwd.to_path_buf())
    }
}

fn is_git_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

fn disabled_role(model: &str) -> CoderRoleConfig {
    CoderRoleConfig {
        model: model.to_string(),
        prompt_path: None,
        prompt: None,
        temperature: None,
        max_tokens: None,
        max_turns: Some(0),
    }
}

/// Factory that honours per-role model ids via `set_model` on a clone... actually
/// [`SingleProviderFactory`] shares one provider. Prefer it when one model is enough;
/// for multi-model, the bridge should install a factory that builds providers per model.
pub fn single_factory(provider: Arc<dyn Provider>) -> Arc<dyn CoderProviderFactory> {
    Arc::new(SingleProviderFactory::new(provider))
}

/// Payload fragment for logging / diagnostics (not a GoalSpec — ACP owns the wire session).
#[allow(dead_code)] // reserved for multi-mode diagnostics / session metadata
pub fn workspace_payload(cwd: &Path) -> serde_json::Value {
    json!({ "workspace_root": cwd.to_string_lossy() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// Process-global env mutations in this crate must not race other tests.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|e| e.into_inner())
    }

    #[tokio::test]
    async fn prepare_workspace_fails_hard_when_worktree_setup_fails() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        // Looks like a git repo to is_git_repo, but is not a real repo → worktree create fails.
        std::fs::create_dir(dir.path().join(".git")).expect(".git");

        let data = tempfile::tempdir().expect("data dir");
        // SAFETY: single-threaded under env_lock; restored below.
        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", data.path());
        }

        let err = prepare_workspace(dir.path(), "sess-hard-fail")
            .await
            .expect_err("must not fall back to host cwd");
        assert!(
            err.contains("durable session worktree"),
            "error should name worktree setup, got: {err}"
        );
        assert!(
            err.contains("no live-tree fallback"),
            "error should refuse live-tree demotion, got: {err}"
        );

        unsafe {
            std::env::remove_var("LIBERADO_DATA_DIR");
        }
    }

    #[tokio::test]
    async fn prepare_workspace_non_git_uses_cwd() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = prepare_workspace(dir.path(), "sess-nongit")
            .await
            .expect("non-git host cwd is ok");
        assert_eq!(path, dir.path());
    }
}
