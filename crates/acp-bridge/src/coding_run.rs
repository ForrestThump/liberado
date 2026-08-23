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

use liberado_coder_agent::assemble_production_run;
use liberado_coder_agent::{CoderProviderFactory, LiberadoLoopBackend};
use liberado_coder_core::{CoderBackend, CoderRoleConfig, CoderTask, CoderTuning};
use liberado_coder_sandbox::{PreflightSpec, PreflightStep, ensure_session_worktree};
use liberado_coder_tools::coding_worktrees_base;
use liberado_provider::{
    CompletionRequest, CompletionResponse, CompletionStream, Provider, ProviderResult,
};
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
    let Some(project) = covering_project(config_dir, project_root) else {
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

/// Interactive ACP `done` spec for a run rooted at `project_root`.
///
/// `None` when nothing matches — no config dir, an undeclared directory, or a project with
/// no interactive profile. Unlike ship, a project named `liberado` does **not** acquire
/// built-in steps: the commands live in the project file or there is no `done` tool.
pub fn interactive_preflight_spec(
    config_dir: Option<&Path>,
    project_root: &Path,
) -> Option<PreflightSpec> {
    let project = covering_project(config_dir, project_root)?;
    spec_from_payload(project.interactive_preflight_payload()?)
}

/// The declared project whose root contains `project_root`, if this deployment has one.
///
/// Identity for preflight: the client's cwd, not a durable worktree under
/// `LIBERADO_DATA_DIR`. Those worktrees are not under the project root, so matching on
/// them would silently drop every configured bar.
fn covering_project(
    config_dir: Option<&Path>,
    project_root: &Path,
) -> Option<liberado_config::ProjectConfig> {
    let dir = config_dir?;
    let config = match liberado_config::load_config(Some(dir)) {
        Ok((config, _)) => config,
        Err(e) => {
            tracing::warn!(error = %e, "loading topology for project preflight failed; no preflight");
            return None;
        }
    };
    // Canonicalized on both sides before comparing. A declared root and a client's cwd routinely
    // name the same directory in different spellings — a trailing separator, a symlink, or on
    // Windows the 8.3 short form — and a literal comparison then finds no project and silently
    // drops the bar, which is the failure this whole change exists to end.
    let root = canonical(project_root);
    let project = config
        .enabled_projects()
        .into_iter()
        .find(|p| root.starts_with(canonical(&p.root)));
    match project {
        Some(p) => Some(p.clone()),
        None => {
            tracing::info!(
                root = %project_root.display(),
                "no declared project covers this workspace; no preflight will run"
            );
            None
        }
    }
}

fn spec_from_payload(payload: serde_json::Value) -> Option<PreflightSpec> {
    let id = payload
        .get("profile")
        .and_then(|v| v.as_str())
        .unwrap_or("interactive")
        .to_string();
    let steps_val = payload.get("steps")?.as_array()?;
    let steps: Vec<PreflightStep> = steps_val.iter().filter_map(step_from_json).collect();
    if steps.is_empty() {
        None
    } else {
        Some(PreflightSpec::new(id, steps))
    }
}

fn step_from_json(value: &serde_json::Value) -> Option<PreflightStep> {
    let name = value.get("name")?.as_str()?.to_string();
    let run = value.get("run")?.as_str()?.to_string();
    if name.is_empty() || run.is_empty() {
        return None;
    }
    let mut step = PreflightStep::new(name, run);
    if let Some(t) = value.get("timeout_secs").and_then(|v| v.as_u64()) {
        step.timeout_secs = Some(t);
    }
    if let Some(r) = value.get("required").and_then(|v| v.as_bool()) {
        step.required = r;
    }
    Some(step)
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .map(|p| liberado_coder_sandbox::strip_extended_path_prefix(&p))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Build coding pack tunables from Liberado config when available.
///
/// An absent config dir or an absent `tuning.toml` is defaults. A file that
/// exists and does not parse, or a `[coder]` section `from_value` rejects, is
/// an error — not a silent default. The warning-and-default path hid a live
/// four-tool + `reasoning = high` file and offered the full catalog instead.
pub fn load_coder_tuning(config_dir: Option<&Path>) -> Result<CoderTuning, String> {
    let Some(dir) = config_dir else {
        return Ok(CoderTuning::default());
    };
    // `load_tuning_only`, not `load_config`: tuning is per-run behaviour and has no business
    // being blocked by a missing `vault_path`. With the full loader, a directory holding one
    // `tuning.toml` failed validation and this fell back to defaults *with a warning nobody
    // reads* — so `[coder]` settings could not be changed without standing up a whole
    // deployment config, which is most of why the "tweak without recompiling" story did not
    // survive contact with an experiment.
    let tuning = liberado_config::load_tuning_only(Some(dir))
        .map_err(|e| format!("reading tuning.toml failed: {e}"))?;
    CoderTuning::from_value(tuning.coder.as_ref()).map_err(|e| {
        format!(
            "invalid [coder] in {}: {e}",
            dir.join("tuning.toml").display()
        )
    })
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

    let mut task = CoderTask::new(&state.coding_session_id, description);
    if let Some(prev) = &state.last_summary {
        task = task.with_context(format!(
            "Prior coding round summary (round {}):\n{prev}",
            state.rounds
        ));
    }

    // Shared production assembly (same path as CodingSessionPack and liberado-coder-run).
    // Critic gets the loaded reviewer prompt so an enabled gate cannot fail on an empty role.
    let assembled = assemble_production_run(
        tuning,
        liberado_coder_agent::assemble::entry::acp_surface(
            task,
            workspace.clone(),
            if model.is_empty() {
                None
            } else {
                Some(model.clone())
            },
            Some(max_turns),
            state.rounds,
            state.prior_feedback.clone(),
        ),
    );
    let request = assembled.request;
    tracing::debug!(
        ?assembled.provenance.fields,
        "coding run assembled (shared production path)"
    );

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
    warm_workspace_if_configured(tuning, &workspace).await?;

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
    let remediation = remediate_if_needed(
        &backend,
        tuning,
        &state.coding_session_id,
        &workspace,
        &base_request,
        &result.session_findings,
    )
    .await;

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

/// Optional remediation pass over the round's session findings, on its own branch. Failures are
/// logged and dropped — an optional extra that can fail the run it was meant to help is a bad
/// trade. Returns the remediation record, if one was produced.
async fn remediate_if_needed(
    backend: &LiberadoLoopBackend,
    tuning: &CoderTuning,
    coding_session_id: &str,
    workspace: &Path,
    base_request: &liberado_coder_core::CoderRunRequest,
    session_findings: &[liberado_coder_core::SessionFinding],
) -> Option<liberado_coder_core::RemediationRecord> {
    if !tuning.session_critic.remediation || session_findings.is_empty() {
        return None;
    }
    let branch = liberado_coder_agent::remediation::remediation_branch(coding_session_id);
    match commit_and_branch(workspace, &branch).await {
        Ok(()) => {
            match liberado_coder_agent::remediation::run_remediation(
                backend,
                base_request,
                session_findings,
                branch.clone(),
            )
            .await
            {
                Ok(record) => record,
                Err(e) => {
                    tracing::warn!(error = %e, "remediation run failed");
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, %branch, "cannot isolate a remediation branch");
            None
        }
    }
}

/// Warm the shared target dir before the first token goes out. A broken baseline refuses the
/// run outright — the model is not asked to fix a tree that never compiled.
async fn warm_workspace_if_configured(
    tuning: &CoderTuning,
    workspace: &Path,
) -> Result<(), String> {
    if !tuning.workspace_build.warmup {
        return Ok(());
    }
    let outcome = liberado_coder_sandbox::warmup::warm_workspace(
        workspace,
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
                "the workspace does not compile before any change was made, so no coding run was started. Fix the baseline first.

{detail}"
            ));
        }
    }
    Ok(())
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

/// A per-role view of one connection profile.
///
/// ACP selects the provider profile once for a session. The coding pack then selects the model
/// for each role. Binding the model to the request keeps the provider credentials and endpoint
/// shared while making the model sent on the wire match the role recorded in the trace. The wrapper
/// also preserves streaming; a coding turn must not degrade to a buffered response.
#[derive(Clone)]
struct RoleBoundProvider {
    inner: Arc<dyn Provider>,
    model: String,
    reasoning: Option<String>,
}

#[async_trait::async_trait]
impl Provider for RoleBoundProvider {
    fn model(&self) -> String {
        self.model.clone()
    }

    async fn complete(&self, request: CompletionRequest) -> ProviderResult<CompletionResponse> {
        self.inner
            .complete(
                request
                    .with_model(Some(self.model.clone()))
                    .with_reasoning(self.reasoning.clone()),
            )
            .await
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> ProviderResult<CompletionStream> {
        self.inner
            .complete_stream(
                request
                    .with_model(Some(self.model.clone()))
                    .with_reasoning(self.reasoning.clone()),
            )
            .await
    }

    async fn list_models(&self) -> ProviderResult<Vec<String>> {
        self.inner.list_models().await
    }
}

#[derive(Clone)]
struct RoleProviderFactory {
    provider: Arc<dyn Provider>,
}

impl CoderProviderFactory for RoleProviderFactory {
    fn provider_for(
        &self,
        _role: &str,
        config: &CoderRoleConfig,
    ) -> Result<Arc<dyn Provider>, liberado_coder_core::CoderError> {
        Ok(Arc::new(RoleBoundProvider {
            inner: Arc::clone(&self.provider),
            model: config.model.clone(),
            reasoning: config.reasoning.clone(),
        }))
    }
}

/// Build providers that share the ACP connection profile but honour each coding role's model.
pub fn role_factory(provider: Arc<dyn Provider>) -> Arc<dyn CoderProviderFactory> {
    Arc::new(RoleProviderFactory { provider })
}

/// Payload fragment for logging / diagnostics (not a GoalSpec — ACP owns the wire session).
#[allow(dead_code)] // reserved for multi-mode diagnostics / session metadata
pub fn workspace_payload(cwd: &Path) -> serde_json::Value {
    json!({ "workspace_root": cwd.to_string_lossy() })
}

#[cfg(test)]
#[path = "max_turns_tests.rs"]
mod max_turns_tests;

#[cfg(test)]
#[path = "reviewer_role_tests.rs"]
mod reviewer_role_tests;

#[cfg(test)]
#[path = "tuning_load_tests.rs"]
mod tuning_load_tests;

#[cfg(test)]
#[path = "coding_run_tests.rs"]
mod tests;

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
#[path = "grant_tests.rs"]
mod grant_tests;

#[cfg(test)]
#[path = "ship_bar_tests.rs"]
mod ship_bar_tests;

#[cfg(test)]
#[path = "interactive_spec_tests.rs"]
mod interactive_spec_tests;
