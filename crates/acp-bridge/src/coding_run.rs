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
pub fn resolve_max_turns(configured: Option<u32>) -> u32 {
    std::env::var("LIBERADO_ACP_MAX_TURNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .or(configured.filter(|&n| n > 0))
        .unwrap_or(DEFAULT_ACP_MAX_TURNS)
}

/// Turns per prompt when neither `[acp] max_turns` nor the env override says otherwise.
pub const DEFAULT_ACP_MAX_TURNS: u32 = 50;

/// The `[acp]` section from Liberado config, or defaults when there is no config dir.
///
/// Same source as [`load_coder_tuning`]: one config dir, read once, so the prompt and the turn
/// budget cannot disagree with the coding pack's own settings about which deployment this is.
pub fn load_acp_config(config_dir: Option<&Path>) -> liberado_config::AcpConfig {
    let Some(dir) = config_dir else {
        return liberado_config::AcpConfig::default();
    };
    match liberado_config::load_config(Some(dir)) {
        Ok((config, _)) => config.topology.acp,
        Err(e) => {
            tracing::warn!(error = %e, "loading [acp] config failed; using defaults");
            liberado_config::AcpConfig::default()
        }
    }
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
            // An unconfigured deployment gets real acceptance checks rather than none. This
            // line used to pass `tuning.verifiers` straight through, which is empty by default,
            // so the only thing standing between a run and `succeeded` was the model's own say-so
            // — and a run whose `cargo check` failed three times filed success on exactly that.
            verifiers: if tuning.verifiers.is_empty() {
                liberado_coder_core::default_verifiers(&workspace)
            } else {
                tuning.verifiers.clone()
            },
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
    /// Process-global env mutations in this crate must not race other tests.
    ///
    /// `tokio::sync::Mutex`, not `std::sync::Mutex`: these tests hold the guard across an `.await`,
    /// which `clippy::await_holding_lock` rejects for a blocking lock — it parks the whole runtime
    /// thread rather than yielding. `coder-agent`'s `DATA_DIR_ENV_LOCK` is the same pattern for the
    /// same reason. (Test binaries are per-crate, so this cannot be the *same* lock, only the same
    /// shape.)
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn prepare_workspace_fails_hard_when_worktree_setup_fails() {
        let _guard = ENV_LOCK.lock().await;
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

/// The capability grant this bridge runs coding mode under (`policy.toml` `coding-local`).
///
/// The bridge runs the coding pack **in-process**, so it never passes through `goals_start`'s
/// profile resolution and never acquires a `SessionGrant` the way a daemon goal does. That is a
/// deliberate choice — a local editor session should not pay for a boundary designed to contain
/// unattended overnight runs — but "no boundary" and "an unstated boundary" are different things.
/// Resolving the grant here makes the authority a row in `policy.toml` that an operator can read
/// and tighten, instead of a property of whatever this binary's code happens to do.
///
/// Fail-closed on an empty resolution, matching `goals_start`: "a session that may do nothing is
/// safe, and never useful. Refuse it here rather than start it." An empty grant almost always means
/// the component is missing from `policy.toml`, and the useful thing is to say so by name.
pub fn resolve_local_grant(
    config_dir: Option<&Path>,
) -> Result<liberado_common::CapabilitySet, String> {
    const COMPONENT: &str = "coding-local";
    let Some(dir) = config_dir else {
        // No config dir at all: the bridge is running standalone (no Liberado deployment on this
        // machine). Nothing to enforce against, and refusing here would make the common
        // `liberado-acp` install useless. Report it rather than inventing authority.
        tracing::info!(
            "no LIBERADO_CONFIG_DIR; coding mode runs without a declared grant (standalone)"
        );
        return Ok(liberado_common::CapabilitySet::empty());
    };
    let (config, _) = liberado_config::load_config(Some(dir))
        .map_err(|e| format!("loading policy for `{COMPONENT}`: {e}"))?;
    let caps = config.policy.capabilities_for(COMPONENT);
    if caps.capabilities.is_empty() {
        return Err(format!(
            "capability grant `{COMPONENT}` resolves to nothing — add a `[[grants]] component = \
             \"{COMPONENT}\"` entry to policy.toml (see config.example/policy.toml), or unset \
             LIBERADO_CONFIG_DIR to run standalone"
        ));
    }
    tracing::info!(
        component = COMPONENT,
        capabilities = caps.capabilities.len(),
        ask_human = caps.contains(&liberado_common::Capability::AskHuman),
        "coding mode authority resolved"
    );
    Ok(caps)
}

#[cfg(test)]
mod grant_tests {
    use super::*;

    /// A configured deployment missing the grant must be refused by name, not run with implied
    /// authority. This is the fail-closed half; without it the grant is a row nothing reads.
    /// A *loadable* config dir with policy.toml but no `coding-local` grant.
    ///
    /// Writing only policy.toml made `load_config` fail on the missing topology, and that load
    /// error string happens to contain "coding-local" too — so the first version of this test
    /// passed with the emptiness check deleted. It asserted the error message, not the rule.
    fn config_dir_without_the_grant() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("topology.toml"),
            "vault_path = \"/tmp/vault\"\n",
        )
        .expect("write topology");
        std::fs::write(
            dir.path().join("policy.toml"),
            "[[grants]]\ncomponent = \"something-else\"\ncapabilities = [\"AskHuman\"]\n",
        )
        .expect("write policy");
        dir
    }

    #[tokio::test]
    async fn a_configured_deployment_without_the_grant_is_refused() {
        let dir = config_dir_without_the_grant();
        // Precondition: the config must actually LOAD, or this proves nothing about the grant.
        liberado_config::load_config(Some(dir.path()))
            .expect("fixture config must load - otherwise the refusal below is a load error");

        let err = resolve_local_grant(Some(dir.path()))
            .expect_err("a missing grant must refuse, not default to permissive");
        assert!(
            err.contains("resolves to nothing"),
            "must refuse for the empty grant specifically, not some upstream failure: {err}"
        );
    }

    /// Standalone (no config dir) is the common install and must keep working — the refusal above
    /// must not fire when there is no deployment to have a policy at all.
    #[tokio::test]
    async fn standalone_without_a_config_dir_is_allowed() {
        assert!(
            resolve_local_grant(None).is_ok(),
            "no config dir means no deployment, not a refusal"
        );
    }
}
