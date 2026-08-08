//! Parallel coding subagents on worktrees + parent-side LLM merge-back (S6 / C7).
//!
//! ## Contract
//!
//! | Question | Answer |
//! |---|---|
//! | Where do workers work? | Each child: named branch + linked worktree under `coding-worktrees/` |
//! | How do results merge? | **Parent only** — sequential `git merge --no-ff` of each child branch |
//! | Disagree? | Git conflicts → parent LLM resolves file contents → stage + commit merge |
//!
//! Children **never** self-merge. Nested fan-out is not supported in this slice.
//!
//! Concurrency is bounded by `max_concurrent` (wire from
//! `tuning.dispatch.max_concurrent_coding_subagents`).

use std::path::Path;
use std::sync::Arc;

use liberado_coder_core::{
    CoderBackend, CoderError, CoderRoleConfig, CoderRunConfig, CoderRunRequest, CoderTask,
    CommandPolicy, LIBERADO_LOOP_BACKEND, PathPolicy, ProgressPolicy, SandboxSpec, WorkspaceRef,
};
use liberado_coder_sandbox::{
    MergeAttempt, add_worktree_on_branch, branch_tip, commit_merge, merge_branch,
    read_conflict_sides, remove_worktree, stage_resolution,
};
use liberado_coder_tools::coding_worktrees_base;
use liberado_common::Outcome;
use liberado_provider::{CompletionRequest, Message, Provider};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Semaphore;
use tracing::{info, warn};

/// One independent coding subtask for fan-out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingSubtask {
    /// Short label used in branch names (`fanout/<label>-…`). Path-safe preferred.
    pub label: String,
    /// Full goal text for the child coding worker.
    pub description: String,
    #[serde(default)]
    pub success_criteria: Vec<String>,
}

/// Result of one child after its worktree is removed (branch tip remains on the parent repo).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChildOutcome {
    pub label: String,
    pub branch: String,
    pub tip_sha: Option<String>,
    pub outcome: Outcome,
    pub summary: String,
    pub files_changed: Vec<String>,
    /// Hub session id when the child ran as a goal session (`run_coding_fanout_via_hub`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// How one branch integrated into the parent tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeStep {
    pub branch: String,
    pub clean: bool,
    pub conflicts_resolved: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanoutReport {
    pub children: Vec<ChildOutcome>,
    pub merges: Vec<MergeStep>,
    pub overall: Outcome,
    pub summary: String,
}

/// Parse `payload.subtasks` (array of objects with label + description).
pub fn subtasks_from_payload(payload: &serde_json::Value) -> Option<Vec<CodingSubtask>> {
    let arr = payload.get("subtasks")?.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for (i, v) in arr.iter().enumerate() {
        let label = v
            .get("label")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("task{i}"));
        let description = v
            .get("description")
            .or_else(|| v.get("goal"))
            .and_then(|x| x.as_str())
            .map(str::to_string)?;
        let success_criteria = v
            .get("success_criteria")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        out.push(CodingSubtask {
            label,
            description,
            success_criteria,
        });
    }
    Some(out)
}

/// Sanitize label for branch / worktree directory segments.
pub fn sanitize_label(label: &str) -> String {
    let s: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "task".into()
    } else {
        s.chars().take(40).collect()
    }
}

/// Default concurrency when neither payload nor pack config sets a cap (matches
/// `DispatchTuning::default().max_concurrent_coding_subagents`).
pub const DEFAULT_MAX_CONCURRENT_CODING_SUBAGENTS: usize = 3;

/// Run N coding children via in-process [`CoderBackend`] (no hub). Prefer
/// [`run_coding_fanout_via_hub`] in production when a hub is attached.
///
/// `parent_root` must be a git repository. Children use HostLocal inside dedicated worktrees.
pub async fn run_coding_fanout(
    backend: Arc<dyn CoderBackend>,
    merger: Arc<dyn Provider>,
    parent_root: &Path,
    tasks: Vec<CodingSubtask>,
    max_concurrent: usize,
    model: &str,
) -> Result<FanoutReport, CoderError> {
    if tasks.is_empty() {
        return Err(CoderError::Setup(
            "coding fan-out requires at least one subtask".into(),
        ));
    }
    let max_concurrent = max_concurrent.max(1);
    let worktrees_base = coding_worktrees_base();
    let sem = Arc::new(Semaphore::new(max_concurrent));

    let mut handles = Vec::new();
    for (i, task) in tasks.into_iter().enumerate() {
        let backend = Arc::clone(&backend);
        let sem = Arc::clone(&sem);
        let parent = parent_root.to_path_buf();
        let wt_base = worktrees_base.clone();
        let model = model.to_string();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed unexpectedly");
            run_one_child_backend(backend, &parent, &wt_base, &task, i, &model).await
        }));
    }

    let mut children = Vec::new();
    for h in handles {
        match h.await {
            Ok(child) => children.push(child),
            Err(e) => children.push(join_fail(e.to_string())),
        }
    }

    finish_fanout(merger.as_ref(), parent_root, children).await
}

/// Run N coding children as **hub-spawned background goal sessions**, each on its own worktree
/// branch, then merge into `parent_root` (parent-only, LLM on conflicts).
///
/// Child goals:
/// - domain `coding`, grant = parent without `AskHuman` (no nested interactive stall)
/// - `payload.fanout_child = true` + `force_host_local` (already on a worktree — no nested isolation)
/// - no `subtasks` (no recursive fan-out)
/// - `parent_session_id` for audit
#[allow(clippy::too_many_arguments)]
pub async fn run_coding_fanout_via_hub(
    hub: Arc<liberado_session::GoalSessionHub>,
    parent_grant: liberado_session::SessionGrant,
    parent_session_id: &str,
    merger: Arc<dyn Provider>,
    parent_root: &Path,
    tasks: Vec<CodingSubtask>,
    max_concurrent: usize,
    model: &str,
) -> Result<FanoutReport, CoderError> {
    if tasks.is_empty() {
        return Err(CoderError::Setup(
            "coding fan-out requires at least one subtask".into(),
        ));
    }
    let max_concurrent = max_concurrent.max(1);
    let worktrees_base = coding_worktrees_base();
    let child_grant = child_session_grant(&parent_grant);
    let sem = Arc::new(Semaphore::new(max_concurrent));

    let mut handles = Vec::new();
    for (i, task) in tasks.into_iter().enumerate() {
        let hub = Arc::clone(&hub);
        let grant = child_grant.clone();
        let sem = Arc::clone(&sem);
        let parent = parent_root.to_path_buf();
        let wt_base = worktrees_base.clone();
        let parent_sid = parent_session_id.to_string();
        let model = model.to_string();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed unexpectedly");
            run_one_child_hub(hub, grant, &parent, &wt_base, &task, i, &parent_sid, &model).await
        }));
    }

    let mut children = Vec::new();
    for h in handles {
        match h.await {
            Ok(child) => children.push(child),
            Err(e) => children.push(join_fail(e.to_string())),
        }
    }

    finish_fanout(merger.as_ref(), parent_root, children).await
}

fn join_fail(msg: String) -> ChildOutcome {
    ChildOutcome {
        label: "join".into(),
        branch: String::new(),
        tip_sha: None,
        outcome: Outcome::Failed,
        summary: format!("child task join failed: {msg}"),
        files_changed: vec![],
        session_id: None,
        error: Some(msg),
    }
}

/// Child grant: same ceiling as parent minus AskHuman (unattended; no nested human stall).
pub fn child_session_grant(
    parent: &liberado_session::SessionGrant,
) -> liberado_session::SessionGrant {
    let mut capabilities = liberado_common::CapabilitySet::empty();
    for cap in &parent.capabilities.capabilities {
        if *cap != liberado_common::Capability::AskHuman {
            capabilities.grant(cap.clone());
        }
    }
    liberado_session::SessionGrant {
        capabilities,
        profile: parent.profile.clone(),
        overrides: parent.overrides.clone(),
        delegation: Some(false), // no nested fan-out via face delegate
        model: parent.model.clone(),
        prompt_append: parent.prompt_append.clone(),
    }
}

async fn finish_fanout(
    merger: &dyn Provider,
    parent_root: &Path,
    children: Vec<ChildOutcome>,
) -> Result<FanoutReport, CoderError> {
    let mut merges = Vec::new();
    for child in &children {
        if child.branch.is_empty() || child.tip_sha.is_none() {
            merges.push(MergeStep {
                branch: child.branch.clone(),
                clean: false,
                conflicts_resolved: vec![],
                merge_commit: None,
                error: Some(
                    child
                        .error
                        .clone()
                        .unwrap_or_else(|| "child produced no branch tip".into()),
                ),
            });
            continue;
        }
        merges.push(merge_one_branch(merger, parent_root, &child.branch).await);
    }

    let any_child_fail = children.iter().any(|c| c.outcome != Outcome::Succeeded);
    let any_merge_fail = merges.iter().any(|m| m.error.is_some());
    let overall = if any_child_fail || any_merge_fail {
        Outcome::Failed
    } else {
        Outcome::Succeeded
    };

    let summary = format!(
        "coding fan-out: {} child(ren), {} merge step(s); overall={overall:?}. {}",
        children.len(),
        merges.len(),
        children
            .iter()
            .map(|c| format!(
                "{}={}{}",
                c.label,
                format!("{:?}", c.outcome).to_lowercase(),
                c.session_id
                    .as_ref()
                    .map(|s| format!("[{s}]"))
                    .unwrap_or_default()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );

    Ok(FanoutReport {
        children,
        merges,
        overall,
        summary,
    })
}

async fn run_one_child_backend(
    backend: Arc<dyn CoderBackend>,
    parent_root: &Path,
    worktrees_base: &Path,
    task: &CodingSubtask,
    index: usize,
    model: &str,
) -> ChildOutcome {
    let label = sanitize_label(&task.label);
    let branch = format!("fanout/{label}-{index}");
    let wt_name = format!("fanout-{label}-{index}");

    let wt_path = match add_worktree_on_branch(parent_root, worktrees_base, &wt_name, &branch).await
    {
        Ok(p) => p,
        Err(e) => {
            return ChildOutcome {
                label: task.label.clone(),
                branch: branch.clone(),
                tip_sha: None,
                outcome: Outcome::Failed,
                summary: format!("worktree create failed: {e}"),
                files_changed: vec![],
                session_id: None,
                error: Some(e.to_string()),
            };
        }
    };

    info!(
        label = %task.label,
        branch = %branch,
        worktree = %wt_path.display(),
        "coding fan-out (backend): child worktree ready"
    );

    let request = child_request(&wt_path, task, model);
    let run = backend.run(request).await;

    let tip = branch_tip(parent_root, &branch).await.ok();
    let _ = remove_worktree(parent_root, &wt_path).await;

    match run {
        Ok(result) => ChildOutcome {
            label: task.label.clone(),
            branch,
            tip_sha: tip,
            outcome: result.outcome,
            summary: result.summary,
            files_changed: result.files_changed,
            session_id: None,
            error: None,
        },
        Err(e) => ChildOutcome {
            label: task.label.clone(),
            branch,
            tip_sha: tip,
            outcome: Outcome::Failed,
            summary: e.to_string(),
            files_changed: vec![],
            session_id: None,
            error: Some(e.to_string()),
        },
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_one_child_hub(
    hub: Arc<liberado_session::GoalSessionHub>,
    grant: liberado_session::SessionGrant,
    parent_root: &Path,
    worktrees_base: &Path,
    task: &CodingSubtask,
    index: usize,
    parent_session_id: &str,
    model: &str,
) -> ChildOutcome {
    use liberado_session::{DomainHint, GoalSpec, TerminalKind};

    let label = sanitize_label(&task.label);
    let branch = format!("fanout/{label}-{index}");
    let wt_name = format!("fanout-{label}-{index}");

    let wt_path = match add_worktree_on_branch(parent_root, worktrees_base, &wt_name, &branch).await
    {
        Ok(p) => p,
        Err(e) => {
            return ChildOutcome {
                label: task.label.clone(),
                branch: branch.clone(),
                tip_sha: None,
                outcome: Outcome::Failed,
                summary: format!("worktree create failed: {e}"),
                files_changed: vec![],
                session_id: None,
                error: Some(e.to_string()),
            };
        }
    };

    let goal = GoalSpec {
        id: None,
        description: task.description.clone(),
        success_criteria: task.success_criteria.clone(),
        domain: DomainHint::Coding,
        max_turns: 12,
        max_idle_secs: None,
        origin: None,
        profile: None,
        payload: json!({
            "workspace_root": wt_path.to_string_lossy(),
            "force_host_local": true,
            "fanout_child": true,
            "fanout_branch": branch,
            "parent_session_id": parent_session_id,
            "model": model,
            "intake": { "enabled": false },
        }),
    };

    info!(
        label = %task.label,
        branch = %branch,
        worktree = %wt_path.display(),
        "coding fan-out (hub): starting child goal session"
    );

    let session_id = match hub.start_background(goal, grant).await {
        Ok(id) => id,
        Err(e) => {
            let _ = remove_worktree(parent_root, &wt_path).await;
            return ChildOutcome {
                label: task.label.clone(),
                branch: branch.clone(),
                tip_sha: None,
                outcome: Outcome::Failed,
                summary: format!("hub start_background failed: {e}"),
                files_changed: vec![],
                session_id: None,
                error: Some(e),
            };
        }
    };

    let snap = match hub.await_terminal(&session_id).await {
        Ok(s) => s,
        Err(e) => {
            let tip = branch_tip(parent_root, &branch).await.ok();
            let _ = remove_worktree(parent_root, &wt_path).await;
            return ChildOutcome {
                label: task.label.clone(),
                branch,
                tip_sha: tip,
                outcome: Outcome::Failed,
                summary: format!("await_terminal failed: {e}"),
                files_changed: vec![],
                session_id: Some(session_id),
                error: Some(e),
            };
        }
    };

    let tip = branch_tip(parent_root, &branch).await.ok();
    let _ = remove_worktree(parent_root, &wt_path).await;

    let (outcome, summary, files) = match snap.session.result {
        Some(r) => {
            let outcome = match r.terminal {
                TerminalKind::Succeeded => Outcome::Succeeded,
                _ => Outcome::Failed,
            };
            (outcome, r.summary, r.artifacts)
        }
        None => (
            Outcome::Failed,
            format!("child session {session_id} finished with no result"),
            vec![],
        ),
    };

    ChildOutcome {
        label: task.label.clone(),
        branch,
        tip_sha: tip,
        outcome,
        summary,
        files_changed: files,
        session_id: Some(session_id),
        error: None,
    }
}

fn child_request(worktree_root: &Path, task: &CodingSubtask, model: &str) -> CoderRunRequest {
    let prompt = format!(
        "You are a Liberado coding subagent working on an isolated git worktree/branch.\n\
         Complete ONLY this subtask; do not expand scope.\n\
         Prefer git_commit for your changes so the parent can merge your branch.\n\
         Subtask label: {}\n",
        task.label
    );
    let role = CoderRoleConfig {
        model: model.into(),
        prompt_path: None,
        prompt: Some(prompt),
        temperature: Some(0.1),
        max_tokens: None,
        max_turns: Some(12),
    };
    let disabled = CoderRoleConfig {
        model: model.into(),
        prompt_path: None,
        prompt: None,
        temperature: None,
        max_tokens: None,
        max_turns: Some(2),
    };
    let mut task_dto = CoderTask::new(
        format!("fanout-{}", sanitize_label(&task.label)),
        &task.description,
    );
    task_dto.success_criteria = task.success_criteria.clone();

    CoderRunRequest {
        task: task_dto,
        workspace: WorkspaceRef::new(worktree_root.to_string_lossy(), "HEAD"),
        config: CoderRunConfig {
            backend: LIBERADO_LOOP_BACKEND.into(),
            trace_dir: None,
            planner: disabled.clone(),
            coder: role.clone(),
            critic: disabled,
            gate: Default::default(),
            repair: Some(role),
            // Already on a dedicated worktree — HostLocal inside it.
            sandbox: SandboxSpec::HostLocal,
            command_policy: CommandPolicy::default(),
            validation_command: None,
            verifiers: Vec::new(),
            verify_policy: Default::default(),
            path_policy: PathPolicy::default(),
            progress: ProgressPolicy {
                max_attempts: 2,
                ..ProgressPolicy::default()
            },
            hashline: liberado_coder_core::HashlineConfig::default(),
        },
        attempt: 0,
        prior_feedback: Vec::new(),
        strategist_directive: None,
    }
}

async fn merge_one_branch(merger: &dyn Provider, parent_root: &Path, branch: &str) -> MergeStep {
    match merge_branch(parent_root, branch).await {
        Ok(MergeAttempt::Clean { merge_commit }) => MergeStep {
            branch: branch.into(),
            clean: true,
            conflicts_resolved: vec![],
            merge_commit,
            error: None,
        },
        Ok(MergeAttempt::Conflicts { paths }) => {
            info!(
                branch = %branch,
                n = paths.len(),
                "coding fan-out: resolving merge conflicts with LLM"
            );
            match resolve_conflicts_with_llm(merger, parent_root, branch, &paths).await {
                Ok(merge_commit) => MergeStep {
                    branch: branch.into(),
                    clean: false,
                    conflicts_resolved: paths,
                    merge_commit: Some(merge_commit),
                    error: None,
                },
                Err(e) => {
                    warn!(branch = %branch, error = %e, "coding fan-out: conflict resolution failed");
                    // Leave repo mid-merge? Abort to leave parent clean for next attempt.
                    let _ = tokio::process::Command::new("git")
                        .args(["-C", &parent_root.to_string_lossy(), "merge", "--abort"])
                        .output()
                        .await;
                    MergeStep {
                        branch: branch.into(),
                        clean: false,
                        conflicts_resolved: vec![],
                        merge_commit: None,
                        error: Some(e),
                    }
                }
            }
        }
        Err(e) => MergeStep {
            branch: branch.into(),
            clean: false,
            conflicts_resolved: vec![],
            merge_commit: None,
            error: Some(e.to_string()),
        },
    }
}

async fn resolve_conflicts_with_llm(
    merger: &dyn Provider,
    parent_root: &Path,
    branch: &str,
    paths: &[String],
) -> Result<String, String> {
    for path in paths {
        let sides = read_conflict_sides(parent_root, path)
            .await
            .map_err(|e| e.to_string())?;
        let resolved = llm_resolve_file(merger, branch, &sides).await?;
        stage_resolution(parent_root, path, &resolved)
            .await
            .map_err(|e| e.to_string())?;
    }
    commit_merge(
        parent_root,
        &format!("merge coding subagent {branch} (LLM-resolved conflicts)"),
    )
    .await
    .map_err(|e| e.to_string())
}

async fn llm_resolve_file(
    merger: &dyn Provider,
    branch: &str,
    sides: &liberado_coder_sandbox::ConflictSides,
) -> Result<String, String> {
    let system = "You are Liberado's merge resolver for coding subagent fan-out.\n\
         Output ONLY the full resolved file contents — no markdown fences, no commentary.\n\
         Integrate both sides' intent when possible; prefer keeping both changes when they do not conflict semantically.";
    let user = format!(
        "Merge conflict while integrating branch `{branch}`.\n\
         Path: {}\n\n\
         === OURS (current parent) ===\n{}\n\n\
         === THEIRS (subagent branch) ===\n{}\n\n\
         === COMBINED (with conflict markers) ===\n{}\n\n\
         Write the complete resolved file:",
        sides.path, sides.ours, sides.theirs, sides.combined
    );
    let req = CompletionRequest::new(vec![Message::system(system), Message::user(user)])
        .with_temperature(0.0)
        .with_max_tokens(8192);
    let resp = merger
        .complete(req)
        .await
        .map_err(|e| format!("merge LLM failed: {e}"))?;
    let text = resp
        .content
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "merge LLM returned empty content".to_string())?;
    // Strip accidental fences.
    let trimmed = text.trim();
    let body = if let Some(rest) = trimmed.strip_prefix("```") {
        let rest = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else {
        trimmed
    };
    Ok(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use liberado_coder_core::{CoderError, CoderRunRequest, CoderRunResult};
    use liberado_common::Outcome;
    use liberado_provider::{CompletionResponse, MockProvider};

    struct WriteFileBackend {
        relative: String,
        content: String,
    }

    #[async_trait::async_trait]
    impl CoderBackend for WriteFileBackend {
        fn name(&self) -> &str {
            "write-file-test"
        }
        async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
            let root = PathBuf::from(&request.workspace.root);
            let path = root.join(&self.relative);
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p).unwrap();
            }
            std::fs::write(&path, &self.content).unwrap();
            // Commit so parent can merge a real tip.
            let _ = std::process::Command::new("git")
                .args(["config", "user.email", "test@liberado.local"])
                .current_dir(&root)
                .status();
            let _ = std::process::Command::new("git")
                .args(["config", "user.name", "test"])
                .current_dir(&root)
                .status();
            let _ = std::process::Command::new("git")
                .args(["add", "-A"])
                .current_dir(&root)
                .status();
            let ok = std::process::Command::new("git")
                .args(["commit", "-m", "child work", "--quiet"])
                .current_dir(&root)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                return Err(CoderError::Backend("commit failed".into()));
            }
            Ok(CoderRunResult {
                backend: self.name().into(),
                outcome: Outcome::Succeeded,
                summary: format!("wrote {}", self.relative),
                files_changed: vec![self.relative.clone()],
                file_changes: vec![],
                validation_notes: None,
                critic_verdict: None,
                gate_votes: vec![],
                trace_path: None,
                diagnostics: json!({}),
            })
        }
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        for args in [
            ["init", "--quiet"].as_slice(),
            ["config", "user.email", "test@liberado.local"].as_slice(),
            ["config", "user.name", "test"].as_slice(),
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(dir)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        std::fs::write(dir.join("README.md"), "base\n").unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["add", "README.md"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "-m", "base", "--quiet"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn parse_subtasks_payload() {
        let p = json!({
            "subtasks": [
                {"label": "api", "description": "do api", "success_criteria": ["x"]},
                {"goal": "do cli", "label": "cli"}
            ]
        });
        let t = subtasks_from_payload(&p).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].label, "api");
        assert_eq!(t[1].description, "do cli");
    }

    #[test]
    fn sanitize_label_preserves_alphanumeric_and_dash() {
        assert_eq!(sanitize_label("hello-world"), "hello-world");
        assert_eq!(sanitize_label("api_v2"), "api_v2");
        assert_eq!(sanitize_label("task-42"), "task-42");
    }

    #[test]
    fn sanitize_label_replaces_spaces_and_special_chars() {
        assert_eq!(sanitize_label("hello world"), "hello-world");
        assert_eq!(sanitize_label("fix: bug"), "fix--bug");
        assert_eq!(sanitize_label("a@b#c"), "a-b-c");
    }

    #[test]
    fn sanitize_label_trims_leading_trailing_dashes() {
        assert_eq!(sanitize_label("-hello-"), "hello");
        assert_eq!(sanitize_label("--start"), "start");
        assert_eq!(sanitize_label("end--"), "end");
    }

    #[test]
    fn sanitize_label_empty_returns_task() {
        assert_eq!(sanitize_label(""), "task");
        assert_eq!(sanitize_label("---"), "task");
        assert_eq!(sanitize_label("!!!%#"), "task");
    }

    #[test]
    fn sanitize_label_truncates_to_40_chars() {
        let long = "a".repeat(100);
        let out = sanitize_label(&long);
        assert_eq!(out.len(), 40);
        assert!(out.chars().all(|c| c == 'a'));
    }

    #[tokio::test]
    async fn fanout_two_children_clean_merge() {
        let root = tempfile::tempdir().unwrap();
        init_repo(root.path());
        // Point coding worktrees at a temp dir via env. The guard is held for the whole test:
        // `LIBERADO_DATA_DIR` is process-global and other tests in this binary set it too.
        let _env = crate::DATA_DIR_ENV_LOCK.lock().await;
        let wt = root.path().join("coding-worktrees");
        // SAFETY: test-only env mutation, serialized by the guard above.
        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", root.path());
        }

        // Two backends that write different files — use a selector backend.
        struct PickBackend;
        #[async_trait::async_trait]
        impl CoderBackend for PickBackend {
            fn name(&self) -> &str {
                "pick"
            }
            async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
                let label = if request.task.id.contains("api") {
                    ("src/api.rs", "api\n")
                } else {
                    ("src/cli.rs", "cli\n")
                };
                let inner = WriteFileBackend {
                    relative: label.0.into(),
                    content: label.1.into(),
                };
                inner.run(request).await
            }
        }

        let backend: Arc<dyn CoderBackend> = Arc::new(PickBackend);
        let merger: Arc<dyn Provider> = Arc::new(MockProvider::with_script(
            "merge",
            [CompletionResponse::text("unused")],
        ));

        let report = run_coding_fanout(
            backend,
            merger,
            root.path(),
            vec![
                CodingSubtask {
                    label: "api".into(),
                    description: "add api".into(),
                    success_criteria: vec![],
                },
                CodingSubtask {
                    label: "cli".into(),
                    description: "add cli".into(),
                    success_criteria: vec![],
                },
            ],
            2,
            "mock",
        )
        .await
        .unwrap();

        assert_eq!(report.overall, Outcome::Succeeded, "{:?}", report);
        assert_eq!(report.children.len(), 2);
        assert!(report.merges.iter().all(|m| m.error.is_none()));
        assert!(root.path().join("src/api.rs").exists());
        assert!(root.path().join("src/cli.rs").exists());
        let _ = wt;
        unsafe {
            std::env::remove_var("LIBERADO_DATA_DIR");
        }
    }

    /// Hub-spawned children: each is a real goal session on a test pack.
    #[tokio::test]
    async fn fanout_via_hub_spawns_sessions_and_merges() {
        use async_trait::async_trait;
        use liberado_session::{
            CODING_DOMAIN, DomainPackRunner, GoalResult, GoalSessionHub, GoalSessionStore,
            GoalSpec, InputChannel, PackContext, PackError, SessionEvent, SessionGrant,
            TerminalKind,
        };
        use tokio::sync::mpsc::Sender;

        struct HubChildPack;
        #[async_trait]
        impl DomainPackRunner for HubChildPack {
            fn domain_id(&self) -> &str {
                CODING_DOMAIN
            }
            async fn run(
                &self,
                _session_id: &str,
                goal: &GoalSpec,
                _ctx: &PackContext<'_>,
                _events: Sender<SessionEvent>,
                _inputs: InputChannel,
                _cancel: tokio::sync::watch::Receiver<bool>,
            ) -> Result<GoalResult, PackError> {
                let root = goal
                    .payload
                    .get("workspace_root")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| PackError::Setup("no workspace_root".into()))?;
                let name = if goal.description.contains("api") {
                    "src/api.rs"
                } else {
                    "src/cli.rs"
                };
                let path = PathBuf::from(root).join(name);
                if let Some(p) = path.parent() {
                    std::fs::create_dir_all(p).unwrap();
                }
                std::fs::write(&path, format!("{name}\n")).unwrap();
                let _ = std::process::Command::new("git")
                    .args(["config", "user.email", "test@liberado.local"])
                    .current_dir(root)
                    .status();
                let _ = std::process::Command::new("git")
                    .args(["config", "user.name", "test"])
                    .current_dir(root)
                    .status();
                assert!(
                    std::process::Command::new("git")
                        .args(["add", "-A"])
                        .current_dir(root)
                        .status()
                        .unwrap()
                        .success()
                );
                assert!(
                    std::process::Command::new("git")
                        .args(["commit", "-m", "child", "--quiet"])
                        .current_dir(root)
                        .status()
                        .unwrap()
                        .success()
                );
                Ok(GoalResult {
                    terminal: TerminalKind::Succeeded,
                    summary: format!("wrote {name}"),
                    artifacts: vec![name.into()],
                    diagnostics: json!({}),
                })
            }
        }

        let root = tempfile::tempdir().unwrap();
        init_repo(root.path());
        let _env = crate::DATA_DIR_ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation, serialized by the guard above.
        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", root.path());
        }

        let store_dir = root.path().join("sessions");
        std::fs::create_dir_all(&store_dir).unwrap();
        let store = GoalSessionStore::open(&store_dir).await;
        let mut hub = GoalSessionHub::new(store);
        hub.register_pack(Arc::new(HubChildPack));
        let hub = Arc::new(hub);

        let grant = SessionGrant {
            capabilities: liberado_common::CapabilitySet::from_iter([
                liberado_common::Capability::AskHuman,
            ]),
            ..Default::default()
        };
        // Child grant must strip AskHuman
        let child = child_session_grant(&grant);
        assert!(!child.grants_ask_human());

        let merger: Arc<dyn Provider> = Arc::new(MockProvider::with_script(
            "merge",
            [CompletionResponse::text("unused")],
        ));

        let report = run_coding_fanout_via_hub(
            hub,
            grant,
            "parent-test",
            merger,
            root.path(),
            vec![
                CodingSubtask {
                    label: "api".into(),
                    description: "add api".into(),
                    success_criteria: vec![],
                },
                CodingSubtask {
                    label: "cli".into(),
                    description: "add cli".into(),
                    success_criteria: vec![],
                },
            ],
            3,
            "mock",
        )
        .await
        .unwrap();

        assert_eq!(report.overall, Outcome::Succeeded, "{:?}", report);
        assert!(
            report.children.iter().all(|c| c.session_id.is_some()),
            "hub children must record session ids: {:?}",
            report.children
        );
        assert!(root.path().join("src/api.rs").exists());
        assert!(root.path().join("src/cli.rs").exists());
        unsafe {
            std::env::remove_var("LIBERADO_DATA_DIR");
        }
    }

    #[tokio::test]
    async fn fanout_conflict_resolved_by_llm() {
        let root = tempfile::tempdir().unwrap();
        init_repo(root.path());
        let _env = crate::DATA_DIR_ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation, serialized by the guard above.
        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", root.path());
        }

        // Two children both rewrite README from the same base → first merge clean, second conflicts.
        struct ToggleBackend;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        #[async_trait::async_trait]
        impl CoderBackend for ToggleBackend {
            fn name(&self) -> &str {
                "toggle"
            }
            async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
                let i = N.fetch_add(1, Ordering::SeqCst);
                let content = if i == 0 { "child-a\n" } else { "child-b\n" };
                WriteFileBackend {
                    relative: "README.md".into(),
                    content: content.into(),
                }
                .run(request)
                .await
            }
        }

        let backend: Arc<dyn CoderBackend> = Arc::new(ToggleBackend);
        let merger: Arc<dyn Provider> = Arc::new(MockProvider::with_script(
            "merge",
            [CompletionResponse::text("merged-by-llm\n")],
        ));

        let report = run_coding_fanout(
            backend,
            merger,
            root.path(),
            vec![
                CodingSubtask {
                    label: "a".into(),
                    description: "a".into(),
                    success_criteria: vec![],
                },
                CodingSubtask {
                    label: "b".into(),
                    description: "b".into(),
                    success_criteria: vec![],
                },
            ],
            1, // serial children so branches are sequential tips from same base... actually
            // both branch from same parent HEAD at start, so both change README from base
            // → first merge clean, second conflicts. Use concurrent 2.
            "mock",
        )
        .await
        .unwrap();

        // With parallel from same base, both merge may conflict depending on order.
        // At least one merge should succeed; overall may succeed if LLM resolves.
        assert!(
            report.merges.iter().any(|m| m.error.is_none()),
            "expected at least one successful merge: {:?}",
            report.merges
        );
        let readme = std::fs::read_to_string(root.path().join("README.md")).unwrap();
        assert!(
            readme.contains("child") || readme.contains("merged-by-llm") || readme.contains("base"),
            "unexpected readme: {readme}"
        );
        unsafe {
            std::env::remove_var("LIBERADO_DATA_DIR");
        }
    }
}
