//! Executes one delegated task: clone → worktree → coding pack → push → PR (plan §7).
//!
//! The run assembles exactly what a local fan-out child assembles —
//! [`assemble_production_run`] with the runner surface, executor in report mode, traces
//! relative to the worktree — so a delegated task and a local one differ only in where
//! the worktree lives. On any failure the task records `Failed` with the step's reason
//! and keeps the worktree: honest infrastructure reporting, never a partial success.

use std::path::PathBuf;
use std::sync::Arc;

use liberado_coder_agent::assemble_production_run;
use liberado_coder_core::{CoderBackend, CoderTask};
use liberado_config_loader::ProviderProfile;
use liberado_delegate_contract::{TaskId, TaskRecord, TaskSpec, TaskStatus, WorkerHealth};
use liberado_forge::ForgeClient;

use crate::ask::{AnswerMailbox, AskDelegator, TaskDelegatorCtx};
use crate::config::{self, WorkerSettings};
use crate::git;
use crate::provider_factory::ProfileProviderFactory;
use crate::queue::TaskStore;

/// Everything one execution needs. Trait objects so tests inject a stub backend/forge.
#[derive(Clone)]
pub struct RunContext {
    pub settings: Arc<WorkerSettings>,
    pub store: Arc<TaskStore>,
    /// Where each task's coding backend comes from. A source rather than a fixed
    /// backend because delegated runs carry a per-task `ask_delegator` extension —
    /// the question has to know which task asked it.
    pub backends: Arc<dyn TaskBackendSource>,
    pub forge: Option<Arc<dyn ForgeClient>>,
}

/// One coding backend per delegated run.
pub trait TaskBackendSource: Send + Sync {
    fn backend_for(&self, ctx: TaskDelegatorCtx) -> Arc<dyn CoderBackend>;
}

/// Tests and no-extension uses: every task gets the same backend instance.
pub struct FixedBackend(pub Arc<dyn CoderBackend>);

impl TaskBackendSource for FixedBackend {
    fn backend_for(&self, _ctx: TaskDelegatorCtx) -> Arc<dyn CoderBackend> {
        self.0.clone()
    }
}

/// Production: clones the assembled template per task and attaches `ask_delegator`,
/// wired to this worker's store and answer mailbox.
pub struct ProductionBackends {
    pub template: liberado_coder_agent::LiberadoLoopBackend,
    pub store: Arc<TaskStore>,
    pub mailbox: Arc<AnswerMailbox>,
    pub question_timeout_secs: u64,
    pub max_open_questions: u32,
}

impl TaskBackendSource for ProductionBackends {
    fn backend_for(&self, ctx: TaskDelegatorCtx) -> Arc<dyn CoderBackend> {
        let extension = AskDelegator::new(
            self.store.clone(),
            self.mailbox.clone(),
            self.question_timeout_secs,
            self.max_open_questions,
            ctx,
        );
        Arc::new(self.template.clone().with_extension(Arc::new(extension)))
    }
}

impl RunContext {
    /// The production context: real provider factory from the worker's topology profile,
    /// Gitea forge when configured. A missing API key fails here, at assembly time.
    /// The caller owns the mailbox so one worker process shares it across every
    /// `RunContext` (restart rescans build fresh contexts; an answer must reach a
    /// re-spawned run's waiter just the same).
    pub fn production(
        settings: Arc<WorkerSettings>,
        store: Arc<TaskStore>,
        mailbox: Arc<AnswerMailbox>,
        profile: ProviderProfile,
        forge: Option<Arc<dyn ForgeClient>>,
    ) -> Result<Self, String> {
        let factory = ProfileProviderFactory::from_profile(profile)?;
        let backends = ProductionBackends {
            template: liberado_coder_agent::LiberadoLoopBackend::with_provider_factory(Arc::new(
                factory,
            )),
            question_timeout_secs: settings.question_timeout_secs,
            max_open_questions: settings.max_open_questions,
            store: store.clone(),
            mailbox,
        };
        Ok(Self {
            settings,
            store,
            backends: Arc::new(backends),
            forge,
        })
    }
}

/// How this pass over the pipeline differs from the first one. A kickback reuses the
/// branch, worktree, and PR that already exist and seeds the model's goal with the
/// delegator's instruction (plan §10; the §8 fallback shape — worse context, same
/// durability).
#[derive(Debug, Clone)]
pub struct RunShape {
    pub reuse_worktree: bool,
    pub effective_goal: String,
    /// Set on kickbacks: the PR to update instead of opening.
    pub existing_pr_url: Option<String>,
}

impl RunShape {
    fn first_pass(spec: &TaskSpec) -> Self {
        Self {
            reuse_worktree: false,
            effective_goal: spec.goal.clone(),
            existing_pr_url: None,
        }
    }
}

/// The full lifecycle of one accepted task. Runs in its own tokio task; every terminal
/// path leaves a record behind.
pub async fn execute(ctx: RunContext, spec: TaskSpec) -> TaskRecord {
    let shape = RunShape::first_pass(&spec);
    run_shaped(ctx, spec, shape).await
}

/// A kickback round: re-run the pack on the surviving branch with the instruction
/// appended to the goal, push, update the existing PR, and report the new summary as
/// a PR comment — the visible half of the audit trail (plan §10).
pub async fn execute_kickback(
    ctx: RunContext,
    spec: TaskSpec,
    round: u32,
    instruction: String,
) -> TaskRecord {
    let id = spec.id.clone();
    // The PR url must exist before anything runs; without it there is nothing to
    // kick back against.
    let existing = ctx.store.get(&id.0).ok().flatten();
    let Some(pr_url) = existing.as_ref().and_then(|record| record.pr_url.clone()) else {
        let reason = "kickback refused: task has no open PR".to_string();
        return match existing {
            Some(_) => ctx
                .store
                .finish(&id, TaskStatus::Failed { reason })
                .unwrap_or_else(|error| {
                    fallback_record(&spec, format!("record failed state: {error}"))
                }),
            None => fallback_record(&spec, reason),
        };
    };
    let shape = RunShape {
        reuse_worktree: true,
        effective_goal: format!(
            "{}\n\n## Kickback round {round}\nThe delegator reviewed the pull request and sent it back with this instruction:\n\n{}",
            spec.goal, instruction
        ),
        existing_pr_url: Some(pr_url),
    };
    run_shaped(ctx, spec, shape).await
}

async fn run_shaped(ctx: RunContext, spec: TaskSpec, shape: RunShape) -> TaskRecord {
    let id = spec.id.clone();
    if let Err(reason) = prepare_and_run(&ctx, &spec, shape).await {
        tracing::warn!(task = %id, %reason, "delegated task failed");
        return ctx
            .store
            .finish(&id, TaskStatus::Failed { reason })
            .unwrap_or_else(|error| {
                fallback_record(&spec, format!("record failed state: {error}"))
            });
    }
    // The success path records PrOpened inside `open_pull_request`; reload for the caller.
    match ctx.store.get(&id.0) {
        Ok(Some(record)) => record,
        _ => fallback_record(&spec, "record vanished after completion".into()),
    }
}

fn fallback_record(spec: &TaskSpec, reason: String) -> TaskRecord {
    TaskRecord {
        spec: spec.clone(),
        status: TaskStatus::Failed { reason },
        session_id: None,
        pr_url: None,
        updated_at: chrono::Utc::now().to_rfc3339(),
    }
}

async fn prepare_and_run(ctx: &RunContext, spec: &TaskSpec, shape: RunShape) -> Result<(), String> {
    let session_id = ulid::Ulid::new().to_string();
    ctx.store
        .mark_running(&spec.id, &session_id)
        .map_err(|error| format!("record running state: {error}"))?;

    let branch = branch_name(spec);
    let worktree = worktree_path(ctx, &spec.id);
    let clone_dir = repo_dir(ctx, &spec.repository);

    ensure_clone(ctx, &clone_dir, &spec.repository).await?;
    ensure_worktree(ctx, spec, &branch, shape.reuse_worktree).await?;

    let backend = ctx.backends.backend_for(TaskDelegatorCtx {
        task_id: spec.id.clone(),
        session_id: session_id.clone(),
    });
    let result = run_pack(
        ctx,
        backend.as_ref(),
        spec,
        &shape.effective_goal,
        &worktree,
    )
    .await?;

    if result.outcome != liberado_common::Outcome::Succeeded {
        return Err(format!(
            "coding pack reported {:?}: {}",
            result.outcome, result.summary
        ));
    }

    commit_and_push(ctx, spec, &branch, &result).await?;
    pr::open_or_update_pr(ctx, spec, &branch, shape, &result).await
}

/// First pass creates the worktree; a kickback lands on the one it left behind —
/// recreating would discard the very state the delegator reviewed.
async fn ensure_worktree(
    ctx: &RunContext,
    spec: &TaskSpec,
    branch: &str,
    reuse: bool,
) -> Result<(), String> {
    let worktree = worktree_path(ctx, &spec.id);
    if reuse && worktree.join(".git").exists() {
        return Ok(());
    }
    let clone_dir = repo_dir(ctx, &spec.repository);
    git::create_worktree(&clone_dir, branch, &worktree, &spec.base_branch)
        .await
        .map_err(|error| format!("create worktree at {branch}: {error}"))
}

async fn ensure_clone(
    ctx: &RunContext,
    clone_dir: &std::path::Path,
    repository: &str,
) -> Result<(), String> {
    if clone_dir.join(".git").exists() {
        git::fetch(clone_dir)
            .await
            .map_err(|error| format!("fetch {repository}: {error}"))?;
        return Ok(());
    }
    let url = ctx.settings.clone_url(repository);
    git::clone(&url, clone_dir)
        .await
        .map_err(|error| format!("clone {url}: {error}"))
}

async fn run_pack(
    ctx: &RunContext,
    backend: &dyn CoderBackend,
    spec: &TaskSpec,
    effective_goal: &str,
    worktree: &std::path::Path,
) -> Result<liberado_coder_core::CoderRunResult, String> {
    let tuning = config::read_tuning(ctx.settings.config_dir.as_deref())?;
    let mut task = CoderTask::new(format!("delegate-{}", spec.id.short()), effective_goal);
    task.success_criteria = spec.success_criteria.clone();

    let surface = liberado_coder_agent::assemble::entry::runner_surface(
        task,
        worktree.to_path_buf(),
        ctx.settings.model.clone(),
        spec.budget.max_turns,
    );
    let mut assembled = assemble_production_run(&tuning, surface);
    assembled.request.config.coder.prompt = Some(liberado_coder_core::prompts::load(
        Some(&liberado_coder_core::prompts::dir_for(
            tuning.prompt_dir.as_deref(),
            &worktree.to_string_lossy(),
        )),
        liberado_coder_core::prompts::CODER_FILE,
        liberado_coder_core::prompts::CODER,
    ));
    assembled.request.config.coder.prompt_path = None;

    // Wall clock is enforced here rather than inside the executor budget plumbing; the
    // wire field exists from day one so delegators can rely on the ceiling.
    let run = backend.run(assembled.request);
    let timed = match spec.budget.wall_clock_secs {
        Some(secs) => tokio::time::timeout(std::time::Duration::from_secs(secs), run)
            .await
            .map_err(|_| "wall clock budget exceeded".to_string())?,
        None => run.await,
    };
    let result = timed.map_err(|error| format!("coding backend failed: {error}"))?;
    Ok(result)
}

async fn commit_and_push(
    ctx: &RunContext,
    spec: &TaskSpec,
    branch: &str,
    result: &liberado_coder_core::CoderRunResult,
) -> Result<(), String> {
    let worktree = worktree_path(ctx, &spec.id);
    let clone_dir = repo_dir(ctx, &spec.repository);
    if git::is_dirty(&worktree) {
        let message = format!(
            "delegate({}): {}\n\n{}",
            spec.id.short(),
            first_line(&spec.goal),
            result.summary
        );
        git::commit_all(&worktree, &message)
            .await
            .map_err(|error| format!("commit delegated work: {error}"))?;
    }
    git::push(&clone_dir, branch)
        .await
        .map_err(|error| format!("push {branch}: {error}"))
}

fn repo_dir(ctx: &RunContext, repository: &str) -> PathBuf {
    let slug = repository
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    ctx.settings.repos_dir().join(slug.trim_matches('-'))
}

fn worktree_path(ctx: &RunContext, id: &TaskId) -> PathBuf {
    ctx.settings.worktrees_dir().join(&id.0)
}

/// Branch shape per plan §7.4, honoring the grant's namespace override.
pub fn branch_name(spec: &TaskSpec) -> String {
    let namespace = spec
        .grant
        .branch_namespace
        .clone()
        .unwrap_or_else(|| spec.id.short());
    format!("delegate/{}/{}", namespace, slugify(&spec.goal))
}

/// Git-ref-safe kebab of the goal's leading words, capped at 40 characters.
pub fn slugify(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if slug.ends_with('-') || slug.is_empty() {
            continue;
        } else {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    slug.trim_matches('-').to_string()
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}

/// PR body carrying the task identity, criteria as checkboxes, and the run summary —
/// the audit trail the delegator reviews before merging (plan §7.4). Worker-local paths
/// (`coder-traces`, `.liberado`) are filtered from the file list so the body describes
/// what the PR actually contains.
pub fn pr_body(spec: &TaskSpec, result: &liberado_coder_core::CoderRunResult) -> String {
    let deliverable_files: Vec<&String> = result
        .files_changed
        .iter()
        .filter(|file| !is_worker_local(file))
        .collect();
    let mut body = format!(
        "Delegated task `{}` from project `{}`.\n\n## Goal\n\n{}\n",
        spec.id, spec.project, spec.goal
    );
    if !spec.success_criteria.is_empty() {
        body.push_str("\n## Success criteria\n\n");
        for criterion in &spec.success_criteria {
            body.push_str(&format!("- [ ] {criterion}\n"));
        }
    }
    body.push_str("\n## Outcome\n\n");
    body.push_str(&format!("- Outcome: {:?}\n", result.outcome));
    body.push_str(&format!("- Summary: {}\n", result.summary));
    if !deliverable_files.is_empty() {
        body.push_str(&format!("- Files changed: {}\n", deliverable_files.len()));
        for file in deliverable_files.iter().take(20) {
            body.push_str(&format!("  - `{file}`\n"));
        }
    }
    body
}

/// Whether a reported path is worker bookkeeping rather than task output; mirrors
/// [`crate::git::WORKER_LOCAL_PATHS`].
fn is_worker_local(path: &str) -> bool {
    crate::git::WORKER_LOCAL_PATHS
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
}

/// Health payload builder kept off the HTTP layer so tests pin it directly.
pub fn health_payload() -> WorkerHealth {
    WorkerHealth {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        fingerprint: crate::build_fingerprint(),
    }
}

#[cfg(test)]
mod tests;

#[path = "runner/pr.rs"]
mod pr;
