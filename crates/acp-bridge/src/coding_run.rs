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

/// The `payload` shape the ship bar reads, for a run rooted at `project_root`.
///
/// The HTTP API builds this from the goal's `project` name. An ACP client sends a prompt and a
/// working directory and never names a project, so the identity is recovered from the path: the
/// declared `[[projects]]` entry whose root contains this run's root. Same payload, same decision
/// function, so a run dispatched from Paseo is held to the bar its project declares.
///
/// An empty object when nothing matches — no config dir, an undeclared directory, a project with
/// no ship profile. That is the honest answer, and [`ship_preflight_required_for`] reads it as
/// "no bar" rather than inventing steps for a repo whose build nobody described.
///
/// [`ship_preflight_required_for`]: liberado_coder_agent::ship_preflight::ship_preflight_required_for
pub fn ship_preflight_payload(config_dir: Option<&Path>, project_root: &Path) -> serde_json::Value {
    let Some(dir) = config_dir else {
        return serde_json::json!({});
    };
    let config = match liberado_config::load_config(Some(dir)) {
        Ok((config, _)) => config,
        Err(e) => {
            tracing::warn!(error = %e, "loading topology for the ship bar failed; no preflight");
            return serde_json::json!({});
        }
    };
    // Canonicalized on both sides before comparing. A declared root and a client's cwd routinely
    // name the same directory in different spellings — a trailing separator, a symlink, or on
    // Windows the 8.3 short form — and a literal comparison then finds no project and silently
    // drops the bar, which is the failure this whole change exists to end.
    let root = canonical(project_root);
    let Some(project) = config
        .enabled_projects()
        .into_iter()
        .find(|p| root.starts_with(canonical(&p.root)))
    else {
        tracing::info!(
            root = %project_root.display(),
            "no declared project covers this workspace; no ship preflight will run"
        );
        return serde_json::json!({});
    };

    let mut payload = serde_json::Map::new();
    payload.insert("project".into(), serde_json::json!(project.name));
    // Absent for a project that declares no steps. `liberado` then still gets the built-in
    // defaults, because the pack resolves those from the project name.
    if let Some(preflight) = project.ship_preflight_payload() {
        payload.insert("preflight".into(), preflight);
    }
    serde_json::Value::Object(payload)
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .map(|p| liberado_coder_sandbox::strip_extended_path_prefix(&p))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Build coding pack tunables from Liberado config when available.
pub fn load_coder_tuning(config_dir: Option<&Path>) -> CoderTuning {
    let Some(dir) = config_dir else {
        return CoderTuning::default();
    };
    // `load_tuning_only`, not `load_config`: tuning is per-run behaviour and has no business
    // being blocked by a missing `vault_path`. With the full loader, a directory holding one
    // `tuning.toml` failed validation and this fell back to defaults *with a warning nobody
    // reads* — so `[coder]` settings could not be changed without standing up a whole
    // deployment config, which is most of why the "tweak without recompiling" story did not
    // survive contact with an experiment.
    match liberado_config::load_tuning_only(Some(dir)) {
        Ok(tuning) => match CoderTuning::from_value(tuning.coder.as_ref()) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "invalid [coder] tuning; using defaults");
                CoderTuning::default()
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "reading tuning.toml failed; using defaults");
            CoderTuning::default()
        }
    }
}

/// Run one full coding pack attempt for `description` in `state.cwd`.
///
/// Creates/reuses a durable worktree under `coding-worktrees/<coding_session_id>` when `cwd` is a
/// git repo (same as `CodingSessionPack` build phase).
/// Everything one coding round needs that is not the mutable session state.
///
/// A struct rather than a parameter list: this grew to nine positional arguments, four of them
/// `Option`/`String`, which is both a clippy failure and an easy place to transpose two values
/// the compiler would happily accept.
pub struct CodingRound<'a> {
    pub provider: Arc<dyn Provider>,
    pub factory: Arc<dyn CoderProviderFactory>,
    pub tuning: &'a CoderTuning,
    pub description: &'a str,
    pub model_override: Option<&'a str>,
    pub max_turns: u32,
    /// Receives tool calls, file changes, guard trips and the model's own text *as they happen*.
    /// `None` reproduces the old behaviour: the run is silent until it returns.
    pub events: Option<tokio::sync::mpsc::Sender<liberado_session::SessionEvent>>,
    /// Resolved by the caller via [`prepare_workspace`], so the path is still known if the run
    /// is cancelled and its output needs preserving.
    pub workspace: PathBuf,
    /// The client's own directory — the repo this run is *of*, not the worktree it happens in.
    ///
    /// The worktree lives under the data dir and matches no declared project root, so the ship
    /// bar has to be resolved from this. Carried separately rather than derived from `workspace`
    /// by walking git links, because the caller already has it.
    pub project_root: PathBuf,
    /// Where `topology.toml` is, for resolving that project. `None` runs without a ship bar.
    pub config_dir: Option<PathBuf>,
}

pub async fn run_coding_round(
    round: CodingRound<'_>,
    state: &mut CodingSessionState,
) -> Result<CodingRoundOutcome, String> {
    let CodingRound {
        provider,
        factory,
        tuning,
        description,
        model_override,
        max_turns,
        events,
        workspace,
        project_root,
        config_dir,
    } = round;
    let model = model_override
        .map(str::to_string)
        .unwrap_or_else(|| provider.model());

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
            // A real reviewer, not `disabled_role`. `[coder.gate]` is honoured below, and the
            // gate's reviewers fall back to this role — but `role_instructions` *errors* when a
            // role carries no prompt, and `disabled_role` carries none. So `enabled = true`
            // parsed, reached the gate, and failed the entire run at the first reviewer. The
            // setting was reachable and unusable, which is worse than unreachable: it looks like
            // the feature is broken rather than unconfigured.
            //
            // The role is built whether or not the gate is on. Building it costs nothing and no
            // model is called while `gate.enabled` is false; making it conditional would put the
            // trap back one `if` away.
            critic: reviewer_role(
                &tuning.critic,
                &model,
                Some(&liberado_coder_core::prompts::dir_for(
                    tuning.prompt_dir.as_deref(),
                    &workspace.to_string_lossy(),
                )),
            ),
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
            session_critic: Default::default(),
            prompt_dir: tuning.prompt_dir.clone(),
            edit: tuning.edit.clone(),
            workspace_build: tuning.workspace_build.clone(),
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

    // Build the workspace BEFORE the first token goes out.
    //
    // Two things this buys. It proves the tree the model is about to edit compiles, so a run that
    // fails is the model's doing and not a question a trace has to answer. And it keeps the
    // provider's prompt cache warm: send the system prompt, then make the model wait through a
    // cold multi-minute build, and the cached prefix has expired by the next message — the same
    // tokens billed twice. Doing the slow part first means every request in the run lands close
    // together.
    //
    // Affordable only because of the shared target dir; see `WorkspaceBuildConfig`.
    if tuning.workspace_build.warmup {
        let outcome = liberado_coder_sandbox::warmup::warm_workspace(
            &workspace,
            &workspace_env(tuning),
            std::time::Duration::from_secs(tuning.workspace_build.warmup_timeout_secs),
        )
        .await;
        match &outcome {
            liberado_coder_sandbox::warmup::Warmup::Ready { seconds } => {
                tracing::info!(seconds, "workspace warm; starting the run")
            }
            liberado_coder_sandbox::warmup::Warmup::TimedOut { seconds } => tracing::warn!(
                seconds,
                "warm-up build did not finish in time; starting the run anyway"
            ),
            liberado_coder_sandbox::warmup::Warmup::Skipped => {}
            liberado_coder_sandbox::warmup::Warmup::BaselineBroken { detail } => {
                // Refuse before spending anything. A broken baseline is not the model's problem
                // to solve, and letting it try produces a report about errors it did not cause.
                return Err(format!(
                    "the workspace does not compile before any change was made, so no coding run                      was started. Fix the baseline first.

{detail}"
                ));
            }
        }
    }

    let backend = LiberadoLoopBackend::with_provider_factory(factory);
    // Scope the live tap around the *whole* run. The pack's emitters are task-locals several
    // layers down; anything left outside this scope emits into nothing and says so nowhere,
    // which is precisely how this path shipped completely silent.
    // Cloned before the run consumes it: the remediation path below needs the same workspace,
    // verifiers and policies, and rebuilding them by hand is how the two drift apart.
    let base_request = request.clone();
    // Cloned before the run consumes it: the ship bar below reports through the same channel, and
    // its progress and verdict are the part of a round a watching client most needs to see.
    let preflight_events = events.clone();
    let run = backend.run(request);
    let result = match events {
        Some(tx) => {
            liberado_coder_agent::with_live_events(tx, state.coding_session_id.clone(), run).await
        }
        None => run.await,
    }
    .map_err(|e| format!("coding pack failed: {e}"))?;

    // Remediation, if it is switched on and there is something a coding run could do.
    //
    // After the main run and its commit, never before: the fix belongs on a branch of its own, and
    // running it first would mix a speculative change into the implementer's work with no way to
    // tell them apart. Failures here are logged and dropped — an optional extra that can fail the
    // run it was meant to help is a bad trade.
    let mut remediation = None;
    if tuning.session_critic.remediation && !result.session_findings.is_empty() {
        let branch =
            liberado_coder_agent::remediation::remediation_branch(&state.coding_session_id);
        match commit_and_branch(&workspace, &branch).await {
            Ok(()) => {
                match liberado_coder_agent::remediation::run_remediation(
                    &backend,
                    &base_request,
                    &result.session_findings,
                    branch.clone(),
                )
                .await
                {
                    Ok(record) => remediation = record,
                    Err(e) => tracing::warn!(error = %e, "remediation run failed"),
                }
            }
            Err(e) => tracing::warn!(error = %e, %branch, "cannot isolate a remediation branch"),
        }
    }

    // The ship bar, before this round may be called a success.
    //
    // Everything above answers "did the model finish". This answers "would the project take it",
    // and the two are not the same question — the in-loop verifiers run a subset of CI against a
    // subset of the tree. Landed in PR #74 and reached only through `CodingSessionPack`, which
    // this path does not use, so every ACP-dispatched run since Paseo shipped skipped it.
    let (outcome, summary) = apply_ship_bar(
        result.outcome,
        result.summary,
        &workspace,
        &project_root,
        config_dir.as_deref(),
        preflight_events,
    )
    .await;

    state.rounds = state.rounds.saturating_add(1);
    state.last_summary = Some(summary.clone());
    if !matches!(
        outcome,
        liberado_common::Outcome::Succeeded | liberado_common::Outcome::PartiallySucceeded
    ) {
        state
            .prior_feedback
            .push(format!("Previous attempt: {summary}"));
    }

    Ok(CodingRoundOutcome {
        summary,
        outcome: format!("{outcome:?}"),
        files_changed: result.files_changed,
        workspace: workspace.display().to_string(),
        trace_path: result.trace_path,
        validation_notes: result.validation_notes,
        // Rendered once, here, so every surface that shows a round shows its findings. Building
        // the markdown at each display site is how one of them ends up not doing it.
        findings: liberado_coder_core::render_findings_markdown(
            &liberado_coder_core::CoderRunResult {
                diff_findings: result.diff_findings,
                session_findings: result.session_findings,
                remediation,
                ..empty_result_shell()
            },
        ),
    })
}

/// Run the ship bar over a finished round and return the outcome it earns.
///
/// Only a round that already claims success is gated. A failed round has nothing to demote, and
/// running a full CI-equivalent suite to confirm a failure spends minutes to learn nothing.
///
/// A preflight that cannot run is **not** a pass. It downgrades with the error in the summary, on
/// the same reasoning as the baseline logic it wraps: the bar exists to stop unverified work being
/// called finished, and "the check broke" is not evidence the work is sound. The failure is loud
/// in the summary rather than silent in a log, because a silent skip is the exact defect here.
async fn apply_ship_bar(
    outcome: liberado_common::Outcome,
    summary: String,
    workspace: &Path,
    project_root: &Path,
    config_dir: Option<&Path>,
    events: Option<tokio::sync::mpsc::Sender<liberado_session::SessionEvent>>,
) -> (liberado_common::Outcome, String) {
    use liberado_coder_agent::ship_preflight as bar;

    if !matches!(
        outcome,
        liberado_common::Outcome::Succeeded | liberado_common::Outcome::PartiallySucceeded
    ) {
        return (outcome, summary);
    }
    let payload = ship_preflight_payload(config_dir, project_root);
    if !bar::ship_preflight_required_for(&payload) {
        return (outcome, summary);
    }
    let Some(spec) = bar::ship_spec_for(&payload) else {
        tracing::info!("ship preflight required but no spec resolved; round not gated");
        return (outcome, summary);
    };

    // A live channel when the client is watching, a closed one when it is not. `run_ship_preflight`
    // ignores send failures, so a closed channel costs one allocation and keeps the gate from
    // having two shapes.
    //
    // The receiver is dropped, deliberately and explicitly. Held instead — which `let (tx, _rx)`
    // does, since a leading underscore names a binding rather than discarding it — the first send
    // fills the buffer and the second blocks forever on a channel nobody will ever read.
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);
    let sink = events.unwrap_or(tx);

    let session = workspace
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "coding".to_string());
    match bar::run_ship_preflight(&session, workspace, &spec, &sink).await {
        Ok(report) if report.ok => (outcome, format!("{summary}\n\n{}", report.summary)),
        Ok(report) => (
            liberado_common::Outcome::Failed,
            format!("{summary}\n\nship preflight failed: {}", report.summary),
        ),
        Err(e) => (
            liberado_common::Outcome::Failed,
            format!("{summary}\n\nship preflight could not run: {e}"),
        ),
    }
}

/// A `CoderRunResult` with nothing in it, for reusing `render_findings_markdown` on the three
/// fields that matter. Cheaper and less brittle than a second renderer that would drift.
fn empty_result_shell() -> liberado_coder_core::CoderRunResult {
    liberado_coder_core::CoderRunResult {
        backend: String::new(),
        outcome: liberado_common::Outcome::Succeeded,
        summary: String::new(),
        files_changed: Vec::new(),
        file_changes: Vec::new(),
        validation_notes: None,
        critic_verdict: None,
        gate_votes: Vec::new(),
        trace_path: None,
        diff_findings: Vec::new(),
        session_findings: Vec::new(),
        remediation: None,
        diagnostics: serde_json::Value::Null,
    }
}

#[derive(Debug, Clone)]
pub struct CodingRoundOutcome {
    pub summary: String,
    pub outcome: String,
    pub files_changed: Vec<String>,
    pub workspace: String,
    pub trace_path: Option<String>,
    pub validation_notes: Option<String>,
    /// Review findings, already rendered. Empty when there are none.
    pub findings: String,
}

impl CodingRoundOutcome {
    /// Human-readable report for ACP `agent_message_chunk` stream.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("## Coding pack result: {}\n\n", self.outcome));
        out.push_str(&self.summary);
        out.push_str("\n\n");
        // Above the workspace path and the file list, deliberately. An open finding is the reason
        // someone is reading this; below a file list is where things go to be skimmed past.
        if !self.findings.is_empty() {
            out.push_str(&self.findings);
            out.push('\n');
        }
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

/// Commit the implementer's work, then move onto a fresh branch for a speculative fix.
///
/// Both halves matter. Without the commit, the remediation agent edits on top of uncommitted work
/// and the two become one indistinguishable diff. Without the branch, a fix for an *unverified*
/// finding lands on the branch a human is about to review as the implementer's own.
async fn commit_and_branch(workspace: &Path, branch: &str) -> Result<(), String> {
    preserve_worktree(workspace, "pre-remediation").await?;
    let out = liberado_common::process::command("git")
        .arg("-C")
        .arg(workspace)
        .args(["checkout", "-b", branch])
        .output()
        .await
        .map_err(|e| format!("git checkout -b {branch}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git checkout -b {branch} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Commit whatever the run left in `workspace`, if anything.
///
/// **Why this is code and not a prompt instruction.** The ACP path leaves its output as dirty
/// files in a scratch worktree; nothing commits them. That is the same defect as F6 — where the
/// headless runner's `preserve_work` ran only on a normal return, so a killed run lost seven
/// modified files — reproduced on a second path. Asking the model to "remember to commit" would
/// make preservation depend on the least reliable component in the system. A run either ends
/// with its work committed or it does not, and that should not be a matter of persuasion.
///
/// Called on **every** exit, including failure and cancel. A failed run's diff is evidence, and
/// a cancelled run is exactly the case F6 was written for.
///
/// Identity is passed with `-c` rather than assumed. `user.email` / `user.name` exist on every
/// dev machine and on no CI runner, so a commit that relies on global config is a commit that
/// works here and fails there.
///
/// Returns the new commit's short SHA, or `None` when the tree was already clean.
pub async fn preserve_worktree(workspace: &Path, label: &str) -> Result<Option<String>, String> {
    let cli = workspace.to_string_lossy().to_string();

    let mut status = liberado_common::process::command("git");
    status.args(["-C", &cli, "status", "--porcelain"]);
    let out = liberado_common::process::output_within(&mut status, "git status", GIT_TIMEOUT)
        .await
        .map_err(|e| format!("git status in worktree: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    if String::from_utf8_lossy(&out.stdout).trim().is_empty() {
        return Ok(None);
    }

    let mut add = liberado_common::process::command("git");
    add.args(["-C", &cli, "add", "-A"]);
    let out = liberado_common::process::output_within(&mut add, "git add", GIT_TIMEOUT)
        .await
        .map_err(|e| format!("git add in worktree: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git add failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    let message = format!("wip({label}): liberado coding session output");
    let mut commit = liberado_common::process::command("git");
    commit.args([
        "-C",
        &cli,
        "-c",
        "user.name=Liberado Coding Pack",
        "-c",
        "user.email=coding-pack@liberado.local",
        "commit",
        "-m",
        &message,
    ]);
    let out = liberado_common::process::output_within(&mut commit, "git commit", GIT_TIMEOUT)
        .await
        .map_err(|e| format!("git commit in worktree: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git commit failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    let mut rev = liberado_common::process::command("git");
    rev.args(["-C", &cli, "rev-parse", "--short", "HEAD"]);
    let out = liberado_common::process::output_within(&mut rev, "git rev-parse", GIT_TIMEOUT)
        .await
        .map_err(|e| format!("git rev-parse in worktree: {e}"))?;
    Ok(Some(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// Ceiling for the git plumbing around a session worktree — local operations, milliseconds.
const GIT_TIMEOUT: std::time::Duration = liberado_common::process::DEFAULT_COMMAND_TIMEOUT;

pub async fn prepare_workspace(cwd: &Path, session_id: &str) -> Result<PathBuf, String> {
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

/// Environment every command in a coding worktree runs with.
///
/// One function so the warm-up build and the run's own commands cannot end up pointed at
/// different caches — which would make the warm-up warm a directory nobody then uses.
fn workspace_env(tuning: &CoderTuning) -> std::collections::BTreeMap<String, String> {
    let mut env = std::collections::BTreeMap::new();
    if let Some(dir) = &tuning.workspace_build.shared_target_dir
        && !dir.trim().is_empty()
    {
        env.insert("CARGO_TARGET_DIR".to_string(), dir.clone());
    }
    env
}

/// The cold diff reviewer the completion gate consults.
///
/// Its prompt is [`liberado_coder_agent::COLD_DIFF_REVIEWER_PROMPT`], shared rather than written
/// here so the reviewer that is measured offline and the reviewer that runs in a session are the
/// same text.
fn reviewer_role(
    configured: &CoderRoleConfig,
    fallback_model: &str,
    prompt_dir: Option<&Path>,
) -> CoderRoleConfig {
    // The *configured* critic model, not the coder's.
    //
    // This took the session's model, so both reviewers ran on whatever the coding run was using
    // — `deepseek-v4-pro` by default — while `[coder.critic] model` said `deepseek-v4-flash` and
    // reached nobody. Reviewing is a cheaper job than coding: the diff reviewer answers one
    // question about a diff, the session critic reads a transcript. Paying pro rates for both,
    // silently, is the kind of thing you only notice on a bill.
    let model = if configured.model.trim().is_empty() {
        fallback_model.to_string()
    } else {
        configured.model.clone()
    };
    CoderRoleConfig {
        model,
        prompt_path: None,
        prompt: Some(liberado_coder_core::prompts::load(
            prompt_dir,
            liberado_coder_core::prompts::DIFF_REVIEWER_FILE,
            liberado_coder_core::prompts::DIFF_REVIEWER,
        )),
        // Deterministic on purpose: a reviewer that returns a different verdict on a re-run
        // cannot be argued with, and a gate you cannot argue with gets switched off.
        temperature: configured.temperature.or(Some(0.0)),
        max_tokens: configured.max_tokens.or(Some(4000)),
        max_turns: Some(1),
    }
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
mod reviewer_role_tests {
    use super::*;

    fn critic_config(model: &str) -> CoderRoleConfig {
        CoderRoleConfig {
            model: model.to_string(),
            prompt_path: None,
            prompt: None,
            temperature: None,
            max_tokens: None,
            max_turns: None,
        }
    }

    /// The reviewer must run on the model `[coder.critic]` names, not the coder's.
    ///
    /// It took the session model, so both reviewers ran on `deepseek-v4-pro` while the config
    /// said `deepseek-v4-flash`. Reviewing a diff is a cheaper job than writing one, and paying
    /// the difference silently is exactly the shape of the other shadowed settings.
    #[test]
    fn the_reviewer_uses_the_configured_model_not_the_coders() {
        let role = reviewer_role(&critic_config("deepseek-v4-flash"), "deepseek-v4-pro", None);
        assert_eq!(
            role.model, "deepseek-v4-flash",
            "the configured critic model must win over the session's"
        );
    }

    /// With no critic model configured, fall back to the session's rather than dispatching to an
    /// empty model id, which fails at the provider with a worse message.
    #[test]
    fn an_unset_critic_model_falls_back_to_the_session_model() {
        let role = reviewer_role(&critic_config("  "), "deepseek-v4-pro", None);
        assert_eq!(role.model, "deepseek-v4-pro");
    }

    /// An open finding must sit above the workspace path and the file list.
    ///
    /// This is the entire "do not bury the finding" mechanism, and it is one `push_str` away from
    /// silently reverting to a footnote under a trace path nobody scrolls to.
    #[test]
    fn findings_are_rendered_above_the_housekeeping() {
        let outcome = CodingRoundOutcome {
            summary: "did the thing".into(),
            outcome: "Succeeded".into(),
            files_changed: vec!["src/main.rs".into()],
            workspace: "/tmp/ws".into(),
            trace_path: Some("/tmp/trace.json".into()),
            validation_notes: None,
            findings: "## Review findings

- the test does not bind
"
            .into(),
        };
        let rendered = outcome.render();
        let finding_at = rendered
            .find("the test does not bind")
            .expect("finding shown");
        let workspace_at = rendered.find("**Workspace:**").expect("workspace shown");
        assert!(
            finding_at < workspace_at,
            "an open finding must not sit below the housekeeping:
{rendered}"
        );
    }

    /// No findings must render exactly as before — no stray heading, no blank section.
    #[test]
    fn a_clean_round_renders_no_findings_section() {
        let outcome = CodingRoundOutcome {
            summary: "did the thing".into(),
            outcome: "Succeeded".into(),
            files_changed: Vec::new(),
            workspace: "/tmp/ws".into(),
            trace_path: None,
            validation_notes: None,
            findings: String::new(),
        };
        assert!(!outcome.render().contains("Review findings"));
    }

    /// The completion gate must be *usable* when it is switched on, not merely reachable.
    ///
    /// `[coder.gate] enabled = true` parses, reaches `run_gate`, and asks `role_instructions` for
    /// the reviewer's prompt. That call returns `Err` for a role with neither `prompt` nor
    /// `prompt_path`, and `run_attempt` propagates it — so with `disabled_role` here, turning the
    /// gate on failed the whole run at the first reviewer. Reverting `critic` to a promptless role
    /// must fail this test rather than wait to be discovered by a user who enabled a setting.
    #[test]
    fn the_gate_reviewer_role_can_actually_be_instructed() {
        let role = reviewer_role(&critic_config("cfg/model"), "session/model", None);
        let prompt = role
            .prompt
            .as_deref()
            .or(role.prompt_path.as_deref())
            .unwrap_or("");
        assert!(
            !prompt.trim().is_empty(),
            "a reviewer role with no prompt makes `[coder.gate] enabled = true` fail the run"
        );
        assert!(
            !role.model.trim().is_empty(),
            "a reviewer with no model cannot be dispatched to a provider"
        );
    }

    /// The prompt must be the shared one, not a copy. A copy drifts from whatever gets measured
    /// offline, and then the score describes a reviewer that never runs.
    #[test]
    fn the_reviewer_uses_the_shared_prompt() {
        assert_eq!(
            reviewer_role(&critic_config("m"), "m", None)
                .prompt
                .as_deref(),
            Some(liberado_coder_agent::COLD_DIFF_REVIEWER_PROMPT),
        );
    }
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

    /// A git repo with one commit, built without relying on any global git identity.
    ///
    /// The identity is passed with `-c` here for the same reason `preserve_worktree` does it:
    /// `user.email` / `user.name` exist on every dev machine and on no CI runner, so a fixture
    /// that leans on global config passes locally and fails in CI.
    fn temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().to_string_lossy().to_string();
        let run = |args: &[&str]| {
            let out = liberado_common::process::std_command("git")
                .args(args)
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["-C", &p, "init", "-q"]);
        std::fs::write(dir.path().join("seed.txt"), "seed\n").expect("seed");
        run(&["-C", &p, "add", "-A"]);
        run(&[
            "-C",
            &p,
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "seed",
        ]);
        dir
    }

    fn is_dirty(repo: &std::path::Path) -> bool {
        let out = liberado_common::process::std_command("git")
            .args(["-C", &repo.to_string_lossy(), "status", "--porcelain"])
            .output()
            .expect("git status");
        !String::from_utf8_lossy(&out.stdout).trim().is_empty()
    }

    /// The whole point: a run's output must survive without anyone remembering to commit it.
    ///
    /// Runs with `GIT_CONFIG_GLOBAL` pointed at an empty file, which is the CI condition — no
    /// `user.name`, no `user.email`. Without the `-c` flags in `preserve_worktree` this fails
    /// with "Please tell me who you are", which is precisely the failure that passes on a
    /// developer box and breaks on a runner.
    #[tokio::test]
    async fn a_dirty_worktree_is_committed_even_with_no_global_git_identity() {
        let _guard = ENV_LOCK.lock().await;
        let repo = temp_repo();
        std::fs::write(repo.path().join("work.txt"), "agent output\n").expect("write");
        assert!(is_dirty(repo.path()), "precondition: tree must be dirty");

        let empty_cfg = tempfile::NamedTempFile::new().expect("cfg");
        // SAFETY: single-threaded under ENV_LOCK; removed below.
        unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", empty_cfg.path()) };

        let result = preserve_worktree(repo.path(), "done").await;

        unsafe { std::env::remove_var("GIT_CONFIG_GLOBAL") };

        let sha = result
            .expect("preserving a dirty worktree must succeed without global identity")
            .expect("a dirty tree must produce a commit");
        assert!(!sha.is_empty(), "commit sha must be reported");
        assert!(
            !is_dirty(repo.path()),
            "the tree must be clean after preservation - nothing left to lose"
        );
    }

    /// A clean tree must not manufacture an empty commit, or every prompt adds noise to history.
    #[tokio::test]
    async fn a_clean_worktree_produces_no_commit() {
        let _guard = ENV_LOCK.lock().await;
        let repo = temp_repo();
        assert!(!is_dirty(repo.path()), "precondition: tree must be clean");

        let preserved = preserve_worktree(repo.path(), "done")
            .await
            .expect("a clean tree is not an error");
        assert!(
            preserved.is_none(),
            "a clean tree must report nothing preserved, got {preserved:?}"
        );
    }

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
    // A config dir with no `policy.toml` is not a broken deployment — it is someone overriding
    // one setting. Treating it as fatal meant `LIBERADO_CONFIG_DIR` was all-or-nothing: point it
    // at a directory holding a single `tuning.toml` and the bridge exited before answering
    // `initialize`, which an ACP client shows as an agent that never responds.
    //
    // A policy file that *exists* and is wrong is still fatal. That is a real misconfiguration
    // and silently granting nothing would hide it.
    if !dir.join("policy.toml").exists() {
        tracing::info!(
            "no policy.toml in LIBERADO_CONFIG_DIR; coding mode runs without a declared grant              (standalone)"
        );
        return Ok(liberado_common::CapabilitySet::empty());
    }
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

#[cfg(test)]
mod ship_bar_tests {
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
}
