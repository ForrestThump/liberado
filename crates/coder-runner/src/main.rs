//! Process boundary for the Rust-native coding backend.
//!
//! Two modes:
//!
//! 1. **JSON bridge** (PR-dispatch path):
//!    `liberado-coder-run --request <path|-> [--config-dir <dir>]`
//!
//! 2. **Headless task runner** (harness-bench / one-shot coding):
//!    `liberado-coder-run task run --prompt "..." --workspace <path> [--model ...] [--max-turns N]`

use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
};

use liberado_coder_agent::{CoderProviderFactory, LiberadoLoopBackend, assemble_production_run};
use liberado_coder_core::{
    CoderBackend, CoderError, CoderRoleConfig, CoderRunRequest, CoderTask, CoderTuning,
};
use liberado_coder_tools::repo_map::{self, RepoMapOptions};
use liberado_common::Outcome;
use liberado_config_loader::{ProviderProfile, Topology};
use liberado_provider::Provider;
use liberado_provider_openai_compat::OpenAiCompatibleProvider;
use serde::{Deserialize, Serialize};

const PROVIDER_ENV: &str = "LIBERADO_CODER_PROVIDER";
const DEFAULT_MODEL: &str = "deepseek-chat";
const DEFAULT_MAX_TURNS: u32 = 50;
const DEFAULT_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
const DEFAULT_BASE_URL: &str = "https://api.deepseek.com/v1";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "liberado_coder_runner=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let args = Args::parse(env::args().skip(1))?;
    match args.command {
        CliCommand::Request { path, config_dir } => run_request(path, config_dir).await,
        CliCommand::TaskRun {
            prompt,
            workspace,
            model,
            max_turns,
            config_dir,
            api_key_env,
            base_url,
            session_id,
        } => {
            run_headless(HeadlessArgs {
                prompt,
                workspace,
                model,
                max_turns,
                config_dir,
                api_key_env,
                base_url,
                session_id,
            })
            .await
        }
    }
}

async fn run_request(path: Option<PathBuf>, config_dir: Option<PathBuf>) -> Result<(), String> {
    let request = read_request(path.as_deref()).await?;
    let profile = provider_profile(config_dir.as_deref())?;
    let providers = Arc::new(OpenAiProfileProviderFactory::from_profile(profile)?);
    let backend = LiberadoLoopBackend::with_provider_factory(providers);
    let result = backend
        .run(request)
        .await
        .map_err(|error| format!("coder backend failed: {error}"))?;
    let json = serde_json::to_string_pretty(&result)
        .map_err(|error| format!("serialize coder result: {error}"))?;
    println!("{json}");
    Ok(())
}

/// Everything `task run` needs. Grouped rather than passed as eight positional parameters —
/// they are one request, and at that width the call site stops being readable.
struct HeadlessArgs {
    prompt: String,
    workspace: PathBuf,
    model: Option<String>,
    max_turns: Option<u32>,
    config_dir: Option<PathBuf>,
    api_key_env: Option<String>,
    base_url: Option<String>,
    session_id: Option<String>,
}

/// Build the task's context: the repo map (when enabled), the workspace summary, and any
/// prior-round session history — plus the task id the run will be preserved under.
async fn build_task_context(
    prompt: &str,
    workspace: &Path,
    tuning: &CoderTuning,
    session_id: Option<&str>,
    workspace_summary: &str,
) -> Result<(Option<String>, String), String> {
    let repo_map = if tuning.repo_map.enabled {
        let mentioned_terms = if tuning.repo_map.task_aware {
            repo_map::extract_task_terms(prompt)
        } else {
            Vec::new()
        };
        repo_map::generate_repo_map(
            workspace,
            &RepoMapOptions {
                max_map_tokens: tuning.repo_map.max_map_tokens,
                min_source_files: tuning.repo_map.min_source_files,
                mentioned_terms,
                ..Default::default()
            },
        )
        .await
    } else {
        None
    };

    let session_context = if let Some(sid) = session_id {
        let prior = load_prior_rounds(workspace, sid)?;
        if !prior.is_empty() {
            Some(build_session_context(&prior))
        } else {
            None
        }
    } else {
        None
    };

    let mut task_context = String::new();
    if let Some(rm) = repo_map {
        task_context.push_str(&rm);
        task_context.push_str("\n\n");
    }
    task_context.push_str(workspace_summary);
    if let Some(sc) = session_context {
        if !task_context.is_empty() {
            task_context.push_str("\n\n");
        }
        task_context.push_str(&sc);
    }
    let task_context = if task_context.is_empty() {
        None
    } else {
        Some(task_context)
    };

    // Derive a task id once so the preservation branch carries information. Both consumers
    // below take this value; see `derive_task_id` for why neither may hold a literal.
    let task_id = derive_task_id(session_id, prompt);

    Ok((task_context, task_id))
}

/// The provider factory behind a headless run: a config-dir profile when given, else the direct
/// api-key/base-url path.
fn build_providers(
    config_dir: &Option<PathBuf>,
    api_key: String,
    base_url: String,
) -> Result<Arc<dyn CoderProviderFactory>, String> {
    match config_dir {
        Some(dir) => {
            let profile = provider_profile(Some(dir))?;
            Ok(Arc::new(OpenAiProfileProviderFactory::from_profile(
                profile,
            )?))
        }
        None => Ok(Arc::new(DirectProviderFactory { api_key, base_url })),
    }
}

/// The run finished: report the outcome, preserve the work durably, and record the round when a
/// session id is in play.
async fn finish_run(
    result: liberado_coder_core::CoderRunResult,
    workspace: PathBuf,
    task_id: String,
    session_id: Option<&str>,
    prompt: &str,
) -> Result<(), String> {
    let json = serde_json::to_string_pretty(&result)
        .map_err(|error| format!("serialize coder result: {error}"))?;
    println!("{json}");

    // Preserve the work before anything else can lose it. A run's output lived only as dirty
    // files in a scratch directory, so deleting that directory destroyed it — which is exactly
    // what happened to one completed run. A commit is durable even when the workspace is a git
    // worktree that later disappears, because the commit and the branch ref go to the *shared*
    // object store.
    if let Err(error) = preserve_work(&workspace, &task_id, push_enabled()).await {
        tracing::warn!(%error, "preserving the run's work failed; the workspace is still on disk");
    }

    if let Some(sid) = session_id
        && result.outcome == Outcome::Succeeded
    {
        save_round_state(&workspace, sid, prompt, &result)?;
    }

    match result.outcome {
        Outcome::Succeeded => Ok(()),
        _ => Err(format!("task completed with outcome: {:?}", result.outcome)),
    }
}

async fn run_headless(args: HeadlessArgs) -> Result<(), String> {
    let HeadlessArgs {
        prompt,
        workspace,
        model,
        max_turns,
        config_dir,
        api_key_env,
        base_url,
        session_id,
    } = args;
    let model = model.unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let max_turns = max_turns.unwrap_or(DEFAULT_MAX_TURNS);
    let api_key_env = api_key_env.unwrap_or_else(|| DEFAULT_API_KEY_ENV.to_string());
    let base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

    let api_key = env::var(&api_key_env)
        .map_err(|_| format!("{api_key_env} is required for headless task runner"))?;

    ensure_git_repo(&workspace).await?;

    let workspace_summary = build_workspace_summary(&workspace).unwrap_or_default();
    let tuning = read_tuning(config_dir.as_deref())?;
    let (task_context, task_id) = build_task_context(
        &prompt,
        &workspace,
        &tuning,
        session_id.as_deref(),
        &workspace_summary,
    )
    .await?;

    let task = CoderTask {
        id: task_id.clone(),
        description: prompt.clone(),
        context: task_context,
        success_criteria: Vec::new(),
    };

    // Shared production assembly (same path as CodingSessionPack and ACP). Gate, progress,
    // command/path policy, edit, workspace_build, etc. come from tuning — not surface hardcodes.
    // Was: gate.enabled hardcoded false (Band F residue); command_policy/path_policy Default.
    let surface = liberado_coder_agent::assemble::entry::runner_surface(
        task,
        workspace.clone(),
        Some(model),
        Some(max_turns),
    );
    // Headless still pre-loads the coder prompt so a bare checkout without prompt_path files
    // works the same way as before; the assembler would leave prompt_path from tuning alone.
    let mut assembled = assemble_production_run(&tuning, surface);
    assembled.request.config.coder.prompt = Some(liberado_coder_core::prompts::load(
        Some(&liberado_coder_core::prompts::dir_for(
            tuning.prompt_dir.as_deref(),
            &workspace.to_string_lossy(),
        )),
        liberado_coder_core::prompts::CODER_FILE,
        liberado_coder_core::prompts::CODER,
    ));
    assembled.request.config.coder.prompt_path = None;
    let request = assembled.request;

    let providers = build_providers(&config_dir, api_key, base_url)?;
    let backend = LiberadoLoopBackend::with_provider_factory(providers);

    // Race the backend against a termination signal so an interrupted run still commits what it
    // produced. The race lives in `run_or_preserve` rather than inline, so it can be tested with
    // a signal the test controls; see that function for what this does and does not catch.
    let result = match run_or_preserve(
        backend.run(request),
        wait_for_termination_signal(),
        &workspace,
        &task_id,
        push_enabled(),
    )
    .await
    {
        RunEnd::Finished(result) => result,
        RunEnd::Terminated => return Err("task terminated by signal".to_string()),
    };

    let result = result.map_err(|error| {
        eprintln!("CoderError: {error:?}");
        format!("coder backend failed: {error}")
    })?;

    finish_run(
        result,
        workspace.clone(),
        task_id.clone(),
        session_id.as_deref(),
        &prompt,
    )
    .await
}

// --- workspace summary (cold-start context injection) -------------------

fn build_workspace_summary(workspace: &Path) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    lines.push("Workspace contents:".to_string());

    let mut files: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(workspace) {
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(workspace).unwrap_or(&path);
            let rel_str = rel.display().to_string();
            if rel_str.starts_with(".git") || rel_str.starts_with(".liberado") {
                continue;
            }
            if path.is_dir() {
                let count = std::fs::read_dir(&path)
                    .map(|d| d.flatten().count())
                    .unwrap_or(0);
                files.push(format!("  {}/  ({} files)", rel_str, count));
            } else {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                files.push(format!("  {}  ({} bytes)", rel_str, size));
            }
        }
    }

    if files.is_empty() {
        lines.push("  (empty workspace)".to_string());
    } else {
        // Cap at 40 entries
        let total = files.len();
        files.truncate(40);
        lines.extend(files);
        if total > 40 {
            lines.push(format!("  ... and {} more entries", total - 40));
        }
    }

    Some(lines.join("\n"))
}

// --- session state (multi-round task memory) ----------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionRound {
    pub session_id: String,
    pub round: u32,
    pub prompt: String,
    pub summary: String,
    pub files_changed: Vec<String>,
}

fn session_state_dir(workspace: &Path, session_id: &str) -> PathBuf {
    workspace
        .join(".liberado")
        .join("task-sessions")
        .join(session_id)
}

fn load_prior_rounds(workspace: &Path, session_id: &str) -> Result<Vec<SessionRound>, String> {
    let dir = session_state_dir(workspace, session_id);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut rounds: Vec<SessionRound> = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .map_err(|e| format!("read session dir {}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            let round: SessionRound =
                serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
            rounds.push(round);
        }
    }
    Ok(rounds)
}

fn build_session_context(prior_rounds: &[SessionRound]) -> String {
    let mut ctx = String::from("[Session history — prior rounds]\n");
    for round in prior_rounds {
        ctx.push_str(&format!("Round {}: {}\n", round.round + 1, round.prompt));
        ctx.push_str(&format!("  Outcome: {}\n", round.summary));
        if !round.files_changed.is_empty() {
            ctx.push_str(&format!(
                "  Files changed: {}\n",
                round.files_changed.join(", ")
            ));
        }
    }
    ctx.push_str("\n[End session history]\n");
    ctx
}

fn save_round_state(
    workspace: &Path,
    session_id: &str,
    prompt: &str,
    result: &liberado_coder_core::CoderRunResult,
) -> Result<(), String> {
    let dir = session_state_dir(workspace, session_id);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create session dir {}: {e}", dir.display()))?;

    let round_num = std::fs::read_dir(&dir)
        .map_err(|e| format!("read session dir: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .count() as u32;

    let round = SessionRound {
        session_id: session_id.to_string(),
        round: round_num,
        prompt: prompt.to_string(),
        summary: result.summary.clone(),
        files_changed: result.files_changed.clone(),
    };

    let path = dir.join(format!("round-{:02}.json", round_num));
    let json =
        serde_json::to_string_pretty(&round).map_err(|e| format!("serialize round state: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))?;

    tracing::info!("saved session round {round_num} to {}", path.display());
    Ok(())
}

async fn ensure_git_repo(workspace: &Path) -> Result<(), String> {
    let git_dir = workspace.join(".git");
    if git_dir.exists() {
        tracing::info!("workspace already a git repo: {}", workspace.display());
        configure_git_safe_directory(workspace)?;
        return Ok(());
    }
    std::fs::create_dir_all(workspace)
        .map_err(|e| format!("create workspace dir {}: {e}", workspace.display()))?;
    tracing::info!(
        "initialising git repo in bare workspace: {}",
        workspace.display()
    );

    let run_git = |args: &[&str]| -> Result<(), String> {
        let output = liberado_common::process::std_command("git")
            .args(args)
            .current_dir(workspace)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
        if !output.status.success() {
            return Err(format!(
                "git {} exited {:?}",
                args.join(" "),
                output.status.code()
            ));
        }
        Ok(())
    };

    run_git(&["init"])?;
    run_git(&["add", "-A"])?;
    run_git(&["commit", "-m", "harness-bench baseline", "--allow-empty"])?;
    configure_git_safe_directory(workspace)?;
    Ok(())
}

/// Let git operate in `workspace` when the checkout is not owned by the running user.
///
/// Scoped to this one absolute path, and only added when it is not already listed. The earlier
/// form — `--global --add safe.directory "*"` on every run — had two problems: `*` turns git's
/// ownership check off for *every* repository on the machine, and `--add` with no membership test
/// appended a duplicate line to `~/.gitconfig` on each invocation, so the file grew without bound.
fn configure_git_safe_directory(workspace: &Path) -> Result<(), String> {
    // Canonical form so the membership test matches what git itself would store.
    let path = std::fs::canonicalize(workspace)
        .unwrap_or_else(|_| workspace.to_path_buf())
        .to_string_lossy()
        .to_string();

    let existing = liberado_common::process::std_command("git")
        .args(["config", "--global", "--get-all", "safe.directory"])
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("git config --get-all safe.directory: {e}"))?;
    // Exit 1 just means the key is unset — that is not a failure here.
    let already = String::from_utf8_lossy(&existing.stdout)
        .lines()
        .any(|line| line.trim() == path || line.trim() == "*");
    if already {
        return Ok(());
    }

    let output = liberado_common::process::std_command("git")
        .args(["config", "--global", "--add", "safe.directory", &path])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("git config safe.directory: {e}"))?;
    if !output.status.success() {
        tracing::warn!(
            "git config safe.directory failed (non-fatal): {:?}",
            output.status.code()
        );
    }
    Ok(())
}

// --- args ------------------------------------------------------------------

#[derive(Debug)]
enum CliCommand {
    Request {
        path: Option<PathBuf>,
        config_dir: Option<PathBuf>,
    },
    TaskRun {
        prompt: String,
        workspace: PathBuf,
        model: Option<String>,
        max_turns: Option<u32>,
        config_dir: Option<PathBuf>,
        api_key_env: Option<String>,
        base_url: Option<String>,
        session_id: Option<String>,
    },
}

impl Default for CliCommand {
    fn default() -> Self {
        CliCommand::Request {
            path: None,
            config_dir: None,
        }
    }
}

#[derive(Debug, Default)]
struct Args {
    command: CliCommand,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let first = args.next();
        match first.as_deref() {
            Some("task") => Self::parse_task_run(args),
            Some("--help") | Some("-h") => Err(usage()),
            _ => {
                let mut parsed = Args::default();
                let mut path = None;
                let mut config_dir = None;

                let iter = first.into_iter().chain(args);
                let mut iter = iter.peekable();
                while let Some(arg) = iter.next() {
                    match arg.as_str() {
                        "--request" => {
                            path =
                                Some(PathBuf::from(iter.next().ok_or_else(|| {
                                    "--request requires a path or '-'".to_string()
                                })?));
                        }
                        "--config-dir" => {
                            config_dir = Some(PathBuf::from(
                                iter.next()
                                    .ok_or_else(|| "--config-dir requires a path".to_string())?,
                            ));
                        }
                        "--help" | "-h" => return Err(usage()),
                        other => return Err(format!("unknown argument '{other}'\n{}", usage())),
                    }
                }

                parsed.command = CliCommand::Request { path, config_dir };
                Ok(parsed)
            }
        }
    }

    fn parse_task_run(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let sub = args.next();
        if sub.as_deref() != Some("run") {
            return Err(format!("expected 'run', got {:?}\n{}", sub, task_usage()));
        }

        let mut prompt = None;
        let mut workspace = None;
        let mut model = None;
        let mut max_turns = None;
        let mut config_dir = None;
        let mut api_key_env = None;
        let mut base_url = None;
        let mut session_id = None;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--prompt" => {
                    prompt = Some(
                        args.next()
                            .ok_or_else(|| "--prompt requires a value".to_string())?,
                    );
                }
                "--workspace" => {
                    workspace = Some(PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--workspace requires a path".to_string())?,
                    ));
                }
                "--model" => {
                    model = Some(
                        args.next()
                            .ok_or_else(|| "--model requires a value".to_string())?,
                    );
                }
                "--max-turns" => {
                    let val = args
                        .next()
                        .ok_or_else(|| "--max-turns requires a number".to_string())?;
                    max_turns = Some(
                        val.parse::<u32>()
                            .map_err(|_| format!("--max-turns must be a number, got '{val}'"))?,
                    );
                }
                "--config-dir" => {
                    config_dir = Some(PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--config-dir requires a path".to_string())?,
                    ));
                }
                "--api-key-env" => {
                    api_key_env = Some(
                        args.next()
                            .ok_or_else(|| "--api-key-env requires a value".to_string())?,
                    );
                }
                "--base-url" => {
                    base_url = Some(
                        args.next()
                            .ok_or_else(|| "--base-url requires a value".to_string())?,
                    );
                }
                "--session-id" => {
                    session_id = Some(
                        args.next()
                            .ok_or_else(|| "--session-id requires a value".to_string())?,
                    );
                }
                "--help" | "-h" => return Err(task_usage()),
                other => return Err(format!("unknown argument '{other}'\n{}", task_usage())),
            }
        }

        let prompt = prompt.ok_or_else(|| format!("--prompt is required\n{}", task_usage()))?;
        let workspace =
            workspace.ok_or_else(|| format!("--workspace is required\n{}", task_usage()))?;

        Ok(Args {
            command: CliCommand::TaskRun {
                prompt,
                workspace,
                model,
                max_turns,
                config_dir,
                api_key_env,
                base_url,
                session_id,
            },
        })
    }
}

// --- provider factory (direct mode, no topology.toml) ----------------------

#[derive(Debug, Clone)]
struct DirectProviderFactory {
    api_key: String,
    base_url: String,
}

impl CoderProviderFactory for DirectProviderFactory {
    fn provider_for(
        &self,
        _role: &str,
        config: &CoderRoleConfig,
    ) -> Result<Arc<dyn Provider>, CoderError> {
        let provider = OpenAiCompatibleProvider::new(&self.api_key, &config.model, &self.base_url)
            .with_extra_client_error_status(vec![429])
            .with_reasoning_effort(config.reasoning.clone());
        Ok(Arc::new(provider))
    }
}

// --- request reading (unchanged from original) -----------------------------

async fn read_request(path: Option<&Path>) -> Result<CoderRunRequest, String> {
    let bytes = match path {
        Some(path) if path.as_os_str() != "-" => tokio::fs::read(path)
            .await
            .map_err(|error| format!("read request {}: {error}", path.display()))?,
        _ => {
            use tokio::io::AsyncReadExt;
            let mut bytes = Vec::new();
            tokio::io::stdin()
                .read_to_end(&mut bytes)
                .await
                .map_err(|error| format!("read request from stdin: {error}"))?;
            bytes
        }
    };
    serde_json::from_slice(&bytes).map_err(|error| format!("parse CoderRunRequest JSON: {error}"))
}

// --- provider profile (unchanged from original) ----------------------------

fn provider_profile(config_dir: Option<&Path>) -> Result<ProviderProfile, String> {
    let topology = match config_dir {
        Some(dir) => read_topology(dir)?,
        None => Topology::default(),
    };
    let provider_name = env::var(PROVIDER_ENV).unwrap_or_else(|_| topology.provider.clone());
    provider_profile_named(topology, &provider_name)
}

/// The pure provider-lookup half of [`provider_profile`], with the provider name supplied as an
/// argument instead of read from the environment — so tests can pin each branch without mutating
/// process-global env vars (the repo's inject-not-mutate convention).
fn provider_profile_named(
    topology: Topology,
    provider_name: &str,
) -> Result<ProviderProfile, String> {
    topology
        .providers
        .into_iter()
        .find(|profile| profile.name == provider_name)
        .ok_or_else(|| format!("provider '{provider_name}' is not declared in topology.providers"))
}

fn read_topology(config_dir: &Path) -> Result<Topology, String> {
    let path = config_dir.join("topology.toml");
    if !path.exists() {
        return Ok(Topology::default());
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| format!("read topology {}: {error}", path.display()))?;
    toml::from_str(&raw).map_err(|error| format!("parse topology {}: {error}", path.display()))
}

fn read_tuning(config_dir: Option<&Path>) -> Result<CoderTuning, String> {
    let Some(dir) = config_dir else {
        return Ok(CoderTuning::default());
    };
    let path = dir.join("tuning.toml");
    if !path.exists() {
        return Ok(CoderTuning::default());
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    let value: toml::Value =
        toml::from_str(&raw).map_err(|error| format!("parse {}: {error}", path.display()))?;
    // Invalid `[coder]` must fail the run. `unwrap_or_default()` here dropped
    // `offered_tools` and `reasoning` on a live compare and offered 21 tools.
    CoderTuning::from_value(value.get("coder"))
        .map_err(|error| format!("invalid [coder] in {}: {error}", path.display()))
}

// --- provider factory from profile (unchanged from original) ---------------

#[derive(Debug, Clone)]
struct OpenAiProfileProviderFactory {
    profile: ProviderProfile,
    api_key: String,
}

impl OpenAiProfileProviderFactory {
    fn from_profile(profile: ProviderProfile) -> Result<Self, String> {
        let api_key = env::var(&profile.api_key_env).map_err(|_| {
            format!(
                "{} is required for provider '{}'",
                profile.api_key_env, profile.name
            )
        })?;
        Ok(Self { profile, api_key })
    }
}

impl CoderProviderFactory for OpenAiProfileProviderFactory {
    fn provider_for(
        &self,
        _role: &str,
        config: &CoderRoleConfig,
    ) -> Result<Arc<dyn Provider>, CoderError> {
        let provider =
            OpenAiCompatibleProvider::new(&self.api_key, &config.model, &self.profile.base_url)
                .with_extra_client_error_status(self.profile.extra_client_error_status.clone())
                .with_reasoning_effort(config.reasoning.clone());
        Ok(Arc::new(provider))
    }
}

// --- usage -----------------------------------------------------------------

fn usage() -> String {
    concat!(
        "liberado-coder-run\n\n",
        "  JSON bridge mode:\n",
        "    liberado-coder-run --request <path|-> [--config-dir <dir>]\n\n",
        "  Headless task mode:\n",
        "    liberado-coder-run task run --prompt <text> --workspace <path> \\\n",
        "      [--model <name>] [--max-turns <n>] [--api-key-env <env>] [--base-url <url>]\n",
    )
    .to_string()
}

fn task_usage() -> String {
    concat!(
        "liberado-coder-run task run --prompt <text> --workspace <path> \\\n",
        "    [--model <name>] [--max-turns <n>] [--config-dir <dir>] \\\n",
        "    [--api-key-env <env>] [--base-url <url>] [--session-id <id>]\n",
        "\n",
        "  --prompt       Task description (required)\n",
        "  --workspace    Working directory path (required)\n",
        "  --model        Model name (default: deepseek-chat)\n",
        "  --max-turns    Max tool turns (default: 30)\n",
        "  --config-dir   Config directory for topology.toml provider lookup\n",
        "  --api-key-env  Env var for API key (default: DEEPSEEK_API_KEY)\n",
        "  --base-url     API base URL (default: https://api.deepseek.com/v1)\n",
        "  --session-id   Session ID for multi-round tasks (resumes prior state)\n",
    )
    .to_string()
}

// --- tests -----------------------------------------------------------------

/// Slugify a prompt into a short label for the preservation branch.
///
/// `preserve_work` does its own alphanumeric sanitization; this only produces a
/// readable prefix so the branch name carries a hint of what the run was about.
fn slugify_prompt(prompt: &str) -> String {
    let slug: String = prompt
        .chars()
        .take(50)
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    slug.trim_matches('-').to_string()
}

/// The label a run's preservation branch is built from.
///
/// A caller-supplied `--session-id` is already a meaningful name and is used verbatim; otherwise
/// the prompt is slugified, so `agent/Fix-the-parser-1786332331` says what the run was about.
/// Both call sites previously held the same three-character literal, which made every branch in
/// the repo's history indistinguishable from every other.
///
/// A prompt with no ASCII alphanumerics slugifies to the empty string. That is left as-is rather
/// than substituted: `preserve_work` still produces a valid timestamped branch, and a silent
/// fallback label would recreate the "this branch says nothing" problem in a new disguise.
///
/// Extracted rather than written inline because a derivation buried in a 200-line `async fn` can
/// only be tested by copying it into the test — and a test that re-implements its subject passes
/// whatever the subject does. `no_task_id_literal_survives_in_production_code` is what holds the
/// *call sites*; this function is what makes the rule testable at all.
fn derive_task_id(session_id: Option<&str>, prompt: &str) -> String {
    match session_id {
        Some(id) => id.to_string(),
        None => slugify_prompt(prompt),
    }
}

/// Which of the two racers in [`run_or_preserve`] finished first.
enum RunEnd<T> {
    /// The run completed on its own; `preserve_work` has *not* been called.
    Finished(T),
    /// A termination signal arrived first and the work was preserved.
    Terminated,
}

/// Run `run` to completion, but if `signal` fires first, commit the workspace and give up.
///
/// The point is the ordering: an interrupted run's output lives only as dirty files in a
/// directory that is about to be deleted, so the commit has to happen before the process exits.
///
/// `signal` is a parameter rather than a direct call to [`wait_for_termination_signal`] purely so
/// this can be tested — passing a future the test fires itself is the only way to observe the
/// preserve-then-give-up branch without sending a real signal to the test runner.
///
/// **What this catches, and what it does not.** It catches a signal the process can actually
/// receive: Ctrl+C at a terminal, or `SIGTERM` from a supervisor on Unix. It does **not** catch
/// `SIGKILL`, and on Windows it does not catch `TerminateProcess` — which is what `taskkill /F`,
/// Python's `subprocess` timeout handling, and our own `Child::start_kill` all use. Nothing in
/// this repo currently sends the runner a catchable signal, so on Windows this is insurance
/// rather than an active code path. Surviving a hard kill needs the workspace committed *during*
/// the run rather than at the end of it, which is a different fix.
async fn run_or_preserve<T, R, S>(
    run: R,
    signal: S,
    workspace: &Path,
    task_id: &str,
    push: bool,
) -> RunEnd<T>
where
    R: std::future::Future<Output = T>,
    S: std::future::Future<Output = ()>,
{
    tokio::select! {
        value = run => RunEnd::Finished(value),
        _ = signal => {
            tracing::info!("termination signal received; preserving work before exit");
            if let Err(error) = preserve_work(workspace, task_id, push).await {
                tracing::warn!(
                    %error,
                    "preserving the run's work failed; the workspace is still on disk"
                );
            }
            RunEnd::Terminated
        }
    }
}

/// Await OS termination signals (Ctrl+C; SIGTERM on Unix).
///
/// Follows the cross-platform pattern in `crates/server/src/shutdown.rs`:
/// `tokio::signal::unix::SignalKind::terminate` under `#[cfg(unix)]`,
/// `tokio::signal::ctrl_c` otherwise. Windows is a first-class target here —
/// a unix-only handler will not compile on CI.
async fn wait_for_termination_signal() {
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received Ctrl+C");
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM");
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("received Ctrl+C");
    }
}

/// Commit whatever the run produced to a branch, and optionally push it.
///
/// Committing is unconditional and local. A worktree's commits and its branch ref are written to
/// the shared `.git`, not to the worktree directory, so the work survives that directory being
/// deleted — which is how a finished run was lost, its output existing only as uncommitted files
/// in a scratch dir that got swept.
///
/// Pushing is opt-in (`--push`) because it is outward-facing: it publishes to a shared remote,
/// where a half-finished agent branch is visible to everyone and cannot be quietly un-published.
/// Local commit alone already removes the data-loss risk, so the default does the safe thing and
/// the network action stays a deliberate choice.
///
/// Never fatal. The run's result is what the caller came for, and failing it over a git problem
/// would discard a successful run to report a bookkeeping error.
async fn preserve_work(workspace: &Path, task_id: &str, push: bool) -> Result<(), String> {
    let dirty = git_output(workspace, &["status", "--porcelain"]).await?;
    if dirty.trim().is_empty() {
        tracing::info!("no changes to preserve");
        return Ok(());
    }

    let slug = task_slug(task_id);
    let branch = format!("agent/{}-{}", slug.trim_matches('-'), now_unix_seconds());

    git_output(workspace, &["checkout", "-b", &branch]).await?;
    commit_work(workspace, &slug).await?;
    tracing::info!(%branch, "committed the run's work");

    if push {
        push_work(workspace, &branch).await;
    }
    Ok(())
}

/// The task id reduced to a git-ref-safe component: alphanumerics kept, everything else a dash.
/// Untrimmed — the caller trims for the branch name while the commit message keeps the raw form.
fn task_slug(task_id: &str) -> String {
    task_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Seconds since the epoch (0 on a clock failure — the branch still uniques per task id).
fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Stage everything and commit on the preservation branch.
///
/// Identity is set explicitly: `user.email`/`user.name` exist on every dev machine and on no
/// CI runner, so relying on ambient config passes locally and fails in the unattended case
/// this whole path exists to protect.
async fn commit_work(workspace: &Path, slug: &str) -> Result<(), String> {
    git_output(workspace, &["add", "-A"]).await?;
    git_output(
        workspace,
        &[
            "-c",
            "user.name=liberado-coder",
            "-c",
            "user.email=coder@liberado.local",
            "commit",
            "-m",
            &format!(
                "wip({slug}): agent run output

Uncommitted output of an unattended coding run, committed so it survives the workspace."
            ),
        ],
    )
    .await?;
    Ok(())
}

/// Best-effort push of the preservation branch: a failure is logged, never fatal — the commit
/// is already safe locally.
async fn push_work(workspace: &Path, branch: &str) {
    match git_output(workspace, &["push", "-u", "origin", branch]).await {
        Ok(_) => tracing::info!(%branch, "pushed"),
        Err(error) => {
            tracing::warn!(%branch, %error, "push failed; the commit is safe locally")
        }
    }
}

/// Whether to push the preservation branch. Opt-in via `LIBERADO_CODER_PUSH=1`.
///
/// An env var rather than a flag so the unattended callers (shepherd, cron, CI) can turn it on for
/// a whole environment without every call site growing an argument, and so the default stays local.
fn push_enabled() -> bool {
    matches!(
        env::var("LIBERADO_CODER_PUSH").as_deref(),
        Ok("1") | Ok("true")
    )
}

async fn git_output(workspace: &Path, args: &[&str]) -> Result<String, String> {
    let out = liberado_common::process::command("git")
        .current_dir(workspace)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The acceptance gates for a headless run.
///
/// A non-empty diff proves the agent did *something*; it says nothing about whether the something
/// Default verifiers for the headless path (same as the shared assembler / ACP).
///
/// Kept as a thin alias so the runner's own tests keep naming the production function, not a
/// reimplementation. Production assembly goes through `assemble_production_run`, which calls
/// `default_verifiers` when tuning leaves the list empty.
#[cfg(test)]
fn verifiers_for(workspace: &Path) -> Vec<liberado_coder_core::VerifierSpec> {
    liberado_coder_core::default_verifiers(workspace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_args() {
        let args = Args::parse(
            ["--request", "request.json", "--config-dir", "config"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();
        match args.command {
            CliCommand::Request { path, config_dir } => {
                assert_eq!(path, Some(PathBuf::from("request.json")));
                assert_eq!(config_dir, Some(PathBuf::from("config")));
            }
            _ => panic!("expected Request command"),
        }
    }

    #[test]
    fn unknown_arg_is_an_error() {
        let err = Args::parse(["--wat"].into_iter().map(str::to_string)).unwrap_err();
        assert!(err.contains("unknown argument"));
        // The error carries the usage text — a usage() that returns empty would pass the
        // unknown-argument check alone.
        assert!(err.contains("JSON bridge mode"), "{err}");
    }

    #[test]
    fn help_as_first_arg_is_an_error() {
        for flag in ["--help", "-h"] {
            let err = Args::parse([flag].into_iter().map(str::to_string)).unwrap_err();
            assert!(err.contains("JSON bridge mode"), "flag {flag}: {err}");
            // The dedicated help arm must win over the unknown-argument arm.
            assert!(!err.contains("unknown argument"), "flag {flag}: {err}");
        }
    }

    #[test]
    fn parses_task_run_minimal() {
        let args = Args::parse(
            [
                "task",
                "run",
                "--prompt",
                "write hello.txt",
                "--workspace",
                "/tmp/ws",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();
        match args.command {
            CliCommand::TaskRun {
                prompt,
                workspace,
                model,
                max_turns,
                ..
            } => {
                assert_eq!(prompt, "write hello.txt");
                assert_eq!(workspace, PathBuf::from("/tmp/ws"));
                assert!(model.is_none());
                assert!(max_turns.is_none());
            }
            _ => panic!("expected TaskRun command"),
        }
    }

    #[test]
    fn parses_task_run_full() {
        let args = Args::parse(
            [
                "task",
                "run",
                "--prompt",
                "do thing",
                "--workspace",
                "/tmp/ws",
                "--model",
                "deepseek-v4-pro",
                "--max-turns",
                "15",
                "--api-key-env",
                "OPENROUTER_API_KEY",
                "--base-url",
                "https://openrouter.ai/api/v1",
                "--config-dir",
                "/srv/config",
                "--session-id",
                "recurring-task-7",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();
        match args.command {
            CliCommand::TaskRun {
                prompt,
                workspace,
                model,
                max_turns,
                api_key_env,
                base_url,
                config_dir,
                session_id,
            } => {
                assert_eq!(prompt, "do thing");
                assert_eq!(workspace, PathBuf::from("/tmp/ws"));
                assert_eq!(model, Some("deepseek-v4-pro".to_string()));
                assert_eq!(max_turns, Some(15));
                assert_eq!(api_key_env, Some("OPENROUTER_API_KEY".to_string()));
                assert_eq!(base_url, Some("https://openrouter.ai/api/v1".to_string()));
                assert_eq!(config_dir, Some(PathBuf::from("/srv/config")));
                assert_eq!(session_id, Some("recurring-task-7".to_string()));
            }
            _ => panic!("expected TaskRun command"),
        }
    }

    #[test]
    fn task_run_missing_prompt_errors() {
        let err = Args::parse(
            ["task", "run", "--workspace", "/tmp"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap_err();
        assert!(err.contains("--prompt is required"));
    }

    #[test]
    fn task_run_missing_workspace_errors() {
        let err = Args::parse(
            ["task", "run", "--prompt", "hi"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap_err();
        assert!(err.contains("--workspace is required"));
    }

    #[test]
    fn task_run_bad_subcommand_errors() {
        let err = Args::parse(
            ["task", "wat", "--prompt", "hi"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap_err();
        assert!(err.contains("expected 'run'"));
    }

    #[test]
    fn task_run_bad_max_turns_errors() {
        let err = Args::parse(
            [
                "task",
                "run",
                "--prompt",
                "hi",
                "--workspace",
                "/tmp",
                "--max-turns",
                "abc",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap_err();
        assert!(err.contains("--max-turns must be a number"));
    }

    #[test]
    fn task_run_help_is_an_error() {
        let err =
            Args::parse(["task", "run", "--help"].into_iter().map(str::to_string)).unwrap_err();
        assert!(err.contains("--prompt"));
    }

    const FOUR_TOOL_THINKING: &str = r#"
[coder]
offered_tools = ["read_file", "write_file", "edit_file", "run_command"]

[coder.coder]
model = "deepseek/deepseek-v4-flash"
temperature = 0.1
max_turns = 30
reasoning = "high"
"#;

    #[test]
    fn read_tuning_loads_offered_tools_and_reasoning_from_a_partial_role() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tuning.toml"), FOUR_TOOL_THINKING).unwrap();

        let tuning = read_tuning(Some(dir.path())).expect("compare-2-shaped tuning must load");
        assert_eq!(
            tuning.offered_tools.as_deref(),
            Some(
                [
                    "read_file".to_string(),
                    "write_file".to_string(),
                    "edit_file".to_string(),
                    "run_command".to_string()
                ]
                .as_slice()
            )
        );
        assert_eq!(tuning.coder.reasoning.as_deref(), Some("high"));

        let assembled = assemble_production_run(
            &tuning,
            liberado_coder_agent::assemble::entry::runner_surface(
                CoderTask::new("d2", "price the models"),
                dir.path().to_path_buf(),
                None,
                Some(30),
            ),
        );
        assert_eq!(
            assembled.request.config.offered_tools, tuning.offered_tools,
            "headless assembly must keep the configured catalog"
        );
        assert_eq!(
            assembled.request.config.coder.reasoning.as_deref(),
            Some("high"),
            "headless assembly must keep the configured reasoning effort"
        );
    }

    #[test]
    fn read_tuning_rejects_an_invalid_coder_section_instead_of_defaulting() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tuning.toml"),
            "[coder.coder]\nmodel = \"x\"\nprompt = \"p\"\nmax_turns = 0\n",
        )
        .unwrap();

        let err =
            read_tuning(Some(dir.path())).expect_err("max_turns = 0 must not become defaults");
        assert!(
            err.contains("invalid [coder]"),
            "the operator must see a load error, got: {err}"
        );
        assert!(
            err.contains("max_turns"),
            "the error must name the bad field, got: {err}"
        );
    }

    #[test]
    fn read_tuning_absent_file_is_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let tuning = read_tuning(Some(dir.path())).expect("missing tuning.toml is defaults");
        assert!(tuning.offered_tools.is_none());
        assert!(tuning.coder.reasoning.is_none());
    }

    /// build_task_context with the repo map disabled: the workspace summary (and prior-round
    /// history when a session id is given) is all that feeds the task context; no shell-out.
    #[tokio::test]
    async fn build_task_context_uses_workspace_summary_and_session_history() {
        let dir = tempfile::tempdir().unwrap();
        let tuning = CoderTuning::default();
        let (ctx, task_id) = build_task_context(
            "do the thing",
            dir.path(),
            &tuning,
            None,
            "Workspace contents:\n  (empty workspace)",
        )
        .await
        .unwrap();
        assert_eq!(task_id, derive_task_id(None, "do the thing"));
        let ctx = ctx.expect("the workspace summary must make the context non-empty");
        assert!(ctx.contains("Workspace contents:"), "{ctx}");
        assert!(!ctx.contains("Session history"), "{ctx}");
    }

    /// With a session id and a prior round on disk, the task context carries the history.
    #[tokio::test]
    async fn build_task_context_includes_prior_rounds() {
        let dir = tempfile::tempdir().unwrap();
        let state = session_state_dir(dir.path(), "sess-1");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            state.join("01.json"),
            serde_json::to_vec(&SessionRound {
                session_id: "sess-1".into(),
                round: 0,
                prompt: "first ask".into(),
                summary: "did it".into(),
                files_changed: vec!["a.txt".into()],
            })
            .unwrap(),
        )
        .unwrap();
        let tuning = CoderTuning::default();
        let (ctx, _) = build_task_context("second ask", dir.path(), &tuning, Some("sess-1"), "")
            .await
            .unwrap();
        let ctx = ctx.expect("the session history must make the context non-empty");
        assert!(ctx.contains("Session history"), "{ctx}");
        assert!(ctx.contains("first ask"), "{ctx}");
        assert!(ctx.contains("a.txt"), "{ctx}");
    }

    /// Without a config dir the direct api-key/base-url factory is used.
    #[test]
    fn build_providers_without_config_dir_is_direct() {
        assert!(
            build_providers(&None, "k".into(), "http://x".into()).is_ok(),
            "the direct factory must construct without any config"
        );
    }
}

#[cfg(test)]
mod verifier_tests {
    use super::*;
    use liberado_coder_core::VerifierSpec;

    fn ids(specs: &[VerifierSpec]) -> Vec<String> {
        specs
            .iter()
            .map(|s| match s {
                VerifierSpec::GitNonemptyDiff { id } => id.clone(),
                VerifierSpec::Command { id, .. } => id.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// A run that produces code which does not compile must not be able to report success.
    ///
    /// Before this, the only acceptance test on the headless path was "the diff is non-empty", so
    /// `outcome: succeeded` meant the model said so and touched a file. PR #92 was filed that way:
    /// `cargo fmt --check` red on both platforms and a test module that would not build.
    #[test]
    fn a_rust_workspace_gets_a_build_check_not_just_a_diff_check() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();

        let specs = verifiers_for(dir.path());
        assert!(
            ids(&specs).iter().any(|id| id == "cargo-check"),
            "a Rust workspace must be compile-checked, got {:?}",
            ids(&specs)
        );
        // The diff check still has to be there — "it compiles" is satisfied by changing nothing.
        assert!(ids(&specs).iter().any(|id| id == "nonempty-diff"));

        let uses_check = specs.iter().any(|s| {
            matches!(s, VerifierSpec::Command { program, args, .. }
                if program == "cargo" && args.first().map(String::as_str) == Some("check"))
        });
        assert!(
            uses_check,
            "prefer `cargo check` over a full build — it catches these errors far cheaper, and \
             disk is finite"
        );
    }

    /// A verifier that always fails is indistinguishable from a broken one, so a non-Rust
    /// workspace must not be handed a cargo command it can never satisfy.
    #[test]
    fn a_workspace_without_cargo_gets_no_cargo_check() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();

        let specs = verifiers_for(dir.path());
        assert!(
            !ids(&specs).iter().any(|id| id == "cargo-check"),
            "must not require cargo where there is no Cargo.toml: {:?}",
            ids(&specs)
        );
        assert!(ids(&specs).iter().any(|id| id == "nonempty-diff"));
    }
}

/// `GIT_CONFIG_GLOBAL` is process-global and `cargo test` runs this binary's tests concurrently,
/// so a test that sets it must exclude every other test that shells out to git. One lock for the
/// whole binary: a per-module duplicate would let two modules race the same variable, which
/// flakes exactly when the suite is busiest. Same purpose as `ENV_LOCK` in
/// `coder-sandbox/src/checkpoint.rs`, but `tokio::sync` rather than `std::sync`: the guard has to
/// be held across an `await`, and a blocking guard there stalls the whole runtime thread —
/// `clippy::await_holding_lock` rejects it outright. `coder-agent`'s `DATA_DIR_ENV_LOCK` is the
/// same choice for the same reason.
#[cfg(test)]
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
mod preserve_work_tests {
    use super::*;
    use std::time::Duration;
    /// Minimal git repo with one committed file. Identity is passed with `-c` rather than
    /// configured — `user.email`/`user.name` exist on every dev machine and on no CI runner.
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

    fn git(repo: &Path, args: &[&str]) -> String {
        let repo_s = repo.to_string_lossy().to_string();
        let mut argv: Vec<&str> = vec!["-C", &repo_s];
        argv.extend_from_slice(args);
        let out = liberado_common::process::std_command("git")
            .args(&argv)
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn is_dirty(repo: &Path) -> bool {
        !git(repo, &["status", "--porcelain"]).is_empty()
    }

    // ── derive_task_id ────────────────────────────────────────────────────────────────────

    #[test]
    fn a_session_id_is_used_verbatim() {
        assert_eq!(
            derive_task_id(Some("my-recurring-task"), "this prompt is ignored"),
            "my-recurring-task"
        );
    }

    #[test]
    fn without_a_session_id_the_prompt_becomes_the_label() {
        let id = derive_task_id(None, "Fix: compile error in main.rs");
        assert_eq!(
            id, "Fix--compile-error-in-main-rs",
            "the label must be a slug of the prompt, not a constant"
        );
    }

    #[test]
    fn a_long_prompt_is_capped_and_never_ends_in_a_separator() {
        let id = derive_task_id(None, &"word ".repeat(40));
        assert!(id.len() <= 50, "must be capped, got {} chars", id.len());
        assert!(
            !id.ends_with('-') && !id.starts_with('-'),
            "a trailing separator produces branches like `agent/word--1786332331`: {id:?}"
        );
    }

    /// Two different prompts must not collide on one label — the whole point of the change.
    #[test]
    fn different_prompts_produce_different_labels() {
        assert_ne!(
            derive_task_id(None, "add a --verbose flag"),
            derive_task_id(None, "remove the --verbose flag"),
        );
    }

    /// The literal this change exists to delete must not come back at either call site.
    ///
    /// A unit test on `derive_task_id` cannot catch that: reverting `CoderTask { id }` to
    /// `"task-1"` leaves the helper correct and every other test in this file green. That exact
    /// mutation was run against the first version of this module and survived it. Scanning the
    /// source is crude, and it is the only check here that binds the call sites.
    ///
    /// Comment lines are exempt so the surrounding prose can name the literal it forbids.
    #[test]
    fn no_task_id_literal_survives_in_production_code() {
        let source = include_str!("main.rs");
        let cut = source
            .lines()
            .position(|l| l.contains("#[cfg(test)]"))
            .unwrap_or(usize::MAX);
        let offenders: Vec<String> = source
            .lines()
            .take(cut)
            .enumerate()
            .filter(|(_, l)| !l.trim_start().starts_with("//"))
            .filter(|(_, l)| l.contains("\"task-1\""))
            .map(|(i, l)| format!("main.rs:{}: {}", i + 1, l.trim()))
            .collect();
        assert!(
            offenders.is_empty(),
            "every run's branch was `agent/task-1-<epoch>` because of this literal; \
             derive the id instead:\n{}",
            offenders.join("\n")
        );
    }

    /// F6 production wiring: the headless path must race the backend against a termination
    /// signal and pass the derived task id into both the race and the post-run preserve.
    ///
    /// Removing `run_or_preserve` or `wait_for_termination_signal` from production leaves every
    /// helper test green and loses SIGTERM/Ctrl+C preservation again — the original F6 failure.
    #[test]
    fn production_headless_path_races_run_against_termination() {
        let source = include_str!("main.rs");
        let cut = source
            .lines()
            .position(|l| l.contains("#[cfg(test)]"))
            .unwrap_or(usize::MAX);
        let production_lines: Vec<&str> = source
            .lines()
            .take(cut)
            .filter(|l| !l.trim_start().starts_with("//") && !l.trim_start().starts_with("///"))
            .collect();
        let production = production_lines.join("\n");
        assert!(
            production.contains("run_or_preserve("),
            "headless path must call run_or_preserve so a catchable signal preserves work"
        );
        let run_call = production
            .split_once("let result = match run_or_preserve(")
            .and_then(|(_, tail)| tail.split_once(")\n    .await"))
            .map(|(call, _)| call)
            .expect("production headless path must await run_or_preserve");
        assert!(
            run_call.contains("wait_for_termination_signal()"),
            "headless path must pass wait_for_termination_signal() into run_or_preserve"
        );
        assert!(
            production.contains("derive_task_id("),
            "task id for preserve branches must be derived, not a constant"
        );
        assert!(
            run_call.contains("&task_id"),
            "run_or_preserve must receive the derived task_id"
        );
        assert!(
            production.contains("preserve_work(&workspace, &task_id, push_enabled())"),
            "post-run preserve_work must receive the derived task_id"
        );
    }

    // ── run_or_preserve ───────────────────────────────────────────────────────────────────

    /// A signal mid-run must commit the workspace before giving up.
    ///
    /// The run future never completes, so a version that simply awaits the backend hangs here;
    /// the outer timeout turns that into a named failure instead of a stalled suite.
    #[tokio::test]
    async fn a_signal_commits_the_work_and_ends_the_run() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("work.txt"), "saved-on-term\n").expect("write");

        let end = tokio::time::timeout(
            Duration::from_secs(30),
            run_or_preserve(
                std::future::pending::<u8>(),
                std::future::ready(()),
                repo.path(),
                "signal-test",
                false,
            ),
        )
        .await
        .expect("the signal must end the run; it waited for a run that never finishes");

        assert!(
            matches!(end, RunEnd::Terminated),
            "a signal must report termination, not a completed run"
        );
        assert!(
            !is_dirty(repo.path()),
            "a killed run's work must be committed, not left dirty in a doomed directory"
        );
        assert!(
            git(repo.path(), &["branch", "--show-current"]).contains("signal-test"),
            "the preserved branch must carry the task id"
        );
    }

    /// The ordinary path must not preserve anything here — the caller does that afterwards, and
    /// committing twice would strand the run's output on a branch it never returns to.
    #[tokio::test]
    async fn a_run_that_finishes_first_is_left_alone() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("work.txt"), "still working\n").expect("write");

        let end = run_or_preserve(
            std::future::ready(7u8),
            std::future::pending::<()>(),
            repo.path(),
            "finished-test",
            false,
        )
        .await;

        assert!(
            matches!(end, RunEnd::Finished(7)),
            "the run's own value must be returned untouched"
        );
        assert!(
            is_dirty(repo.path()),
            "the signal branch must not run when the backend wins the race"
        );
        assert!(
            !git(repo.path(), &["branch", "--show-current"]).starts_with("agent/"),
            "no preservation branch may be created on the ordinary path"
        );
    }

    // ── preserve_work ─────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn the_branch_name_carries_the_supplied_id() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("work.txt"), "output\n").expect("write");

        preserve_work(repo.path(), "fix-compile-error", false)
            .await
            .expect("preserve_work must succeed");

        let current = git(repo.path(), &["branch", "--show-current"]);
        assert!(
            current.contains("fix-compile-error"),
            "branch '{current}' must name the task, not a constant"
        );
        assert!(!is_dirty(repo.path()), "the tree must be clean afterwards");
    }

    #[tokio::test]
    async fn a_clean_worktree_produces_no_commit() {
        let repo = temp_repo();
        assert!(!is_dirty(repo.path()), "precondition: tree must be clean");

        preserve_work(repo.path(), "clean-test", false)
            .await
            .expect("a clean tree is not an error");

        assert!(
            !git(repo.path(), &["branch", "--show-current"]).starts_with("agent/"),
            "an empty commit on a throwaway branch is noise, not preservation"
        );
    }

    /// The unattended case: no global git identity, which is every CI runner.
    #[tokio::test]
    async fn a_dirty_worktree_is_committed_with_no_global_git_identity() {
        let _guard = ENV_LOCK.lock().await;
        let repo = temp_repo();
        std::fs::write(repo.path().join("work.txt"), "agent output\n").expect("write");

        let empty_cfg = tempfile::NamedTempFile::new().expect("cfg");
        let prior = std::env::var_os("GIT_CONFIG_GLOBAL");
        // SAFETY: ENV_LOCK excludes every other git-touching test in this binary, and the
        // previous value is restored below rather than blindly removed.
        unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", empty_cfg.path()) };

        let result = preserve_work(repo.path(), "no-identity", false).await;

        // SAFETY: same lock; restores exactly what was there before.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("GIT_CONFIG_GLOBAL", v),
                None => std::env::remove_var("GIT_CONFIG_GLOBAL"),
            }
        }

        result.expect("an unattended run must commit without ambient git identity");
        assert!(!is_dirty(repo.path()), "the tree must be clean afterwards");
    }

    /// finish_run: a succeeded run with a session id records the round and returns Ok.
    #[tokio::test]
    async fn finish_run_success_records_the_round() {
        let dir = temp_repo();
        let result = liberado_coder_core::CoderRunResult {
            backend: "test".into(),
            outcome: Outcome::Succeeded,
            summary: "did it".into(),
            files_changed: vec![],
            file_changes: vec![],
            validation_notes: None,
            critic_verdict: None,
            gate_votes: vec![],
            trace_path: None,
            diff_findings: vec![],
            session_findings: vec![],
            remediation: None,
            diagnostics: serde_json::json!({}),
        };
        finish_run(
            result,
            dir.path().to_path_buf(),
            "tid".into(),
            Some("sess-9"),
            "the prompt",
        )
        .await
        .unwrap();
        let state = session_state_dir(dir.path(), "sess-9");
        let entries: Vec<_> = std::fs::read_dir(&state).expect("round dir").collect();
        assert_eq!(
            entries.len(),
            1,
            "a succeeded run with a session id must record the round"
        );
    }

    /// finish_run: a failed outcome is an error naming the outcome.
    #[tokio::test]
    async fn finish_run_failed_outcome_is_an_error() {
        let dir = temp_repo();
        let result = liberado_coder_core::CoderRunResult {
            backend: "test".into(),
            outcome: Outcome::Failed,
            summary: "no".into(),
            files_changed: vec![],
            file_changes: vec![],
            validation_notes: None,
            critic_verdict: None,
            gate_votes: vec![],
            trace_path: None,
            diff_findings: vec![],
            session_findings: vec![],
            remediation: None,
            diagnostics: serde_json::json!({}),
        };
        let err = finish_run(result, dir.path().to_path_buf(), "tid".into(), None, "p")
            .await
            .unwrap_err();
        assert!(err.contains("task completed with outcome"), "{err}");
    }
}

#[cfg(test)]
mod impl_tests {
    use super::*;
    use liberado_coder_core::CoderRunResult;

    // ── build_workspace_summary ────────────────────────────────────────────────

    #[test]
    fn workspace_summary_empty_dir_is_marked_empty() {
        let dir = tempfile::tempdir().unwrap();
        let summary = build_workspace_summary(dir.path()).unwrap();
        assert!(summary.contains("(empty workspace)"), "{summary}");
    }

    #[test]
    fn workspace_summary_lists_files_and_dir_counts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("b.rs"), "fn b() {}").unwrap();
        std::fs::write(dir.path().join("sub").join("c.rs"), "fn c() {}").unwrap();

        let summary = build_workspace_summary(dir.path()).unwrap();
        assert!(summary.contains("a.txt  (5 bytes)"), "{summary}");
        assert!(summary.contains("sub/  (2 files)"), "{summary}");
    }

    #[test]
    fn workspace_summary_skips_git_and_liberado_state() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join(".liberado")).unwrap();
        std::fs::write(dir.path().join(".git").join("HEAD"), "ref: x").unwrap();
        std::fs::write(dir.path().join("real.txt"), "x").unwrap();

        let summary = build_workspace_summary(dir.path()).unwrap();
        assert!(!summary.contains(".git"), "{summary}");
        assert!(!summary.contains(".liberado"), "{summary}");
        assert!(summary.contains("real.txt"), "{summary}");
    }

    /// Exactly 40 entries must NOT trip the "and N more" cap — only a 41st entry should.
    #[test]
    fn workspace_summary_exactly_forty_entries_has_no_cap_line() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..40 {
            std::fs::write(dir.path().join(format!("f{i:02}.txt")), "x").unwrap();
        }
        let summary = build_workspace_summary(dir.path()).unwrap();
        assert!(!summary.contains("more entries"), "{summary}");
    }

    #[test]
    fn workspace_summary_caps_at_forty_entries() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..45 {
            std::fs::write(dir.path().join(format!("f{i:02}.txt")), "x").unwrap();
        }
        let summary = build_workspace_summary(dir.path()).unwrap();
        assert!(summary.contains("... and 5 more entries"), "{summary}");
    }

    // ── session state ──────────────────────────────────────────────────────────

    fn sample_round(session: &str, round: u32, prompt: &str) -> SessionRound {
        SessionRound {
            session_id: session.into(),
            round,
            prompt: prompt.into(),
            summary: "done".into(),
            files_changed: vec!["a.rs".into()],
        }
    }

    #[test]
    fn load_prior_rounds_absent_dir_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_prior_rounds(dir.path(), "sess-1").unwrap().is_empty());
    }

    #[test]
    fn load_prior_rounds_parses_and_sorts() {
        let dir = tempfile::tempdir().unwrap();
        let state = session_state_dir(dir.path(), "sess-1");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            state.join("round-01.json"),
            serde_json::to_string(&sample_round("sess-1", 1, "second")).unwrap(),
        )
        .unwrap();
        std::fs::write(
            state.join("round-00.json"),
            serde_json::to_string(&sample_round("sess-1", 0, "first")).unwrap(),
        )
        .unwrap();

        let rounds = load_prior_rounds(dir.path(), "sess-1").unwrap();
        assert_eq!(rounds.len(), 2);
        assert_eq!(
            rounds[0].prompt, "first",
            "must sort by filename, not insertion"
        );
        assert_eq!(rounds[1].prompt, "second");
    }

    #[test]
    fn load_prior_rounds_unreadable_dir_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the session dir should be: read_dir fails, not silently empty.
        let state = session_state_dir(dir.path(), "sess-1");
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        std::fs::write(&state, "not a dir").unwrap();
        let err = load_prior_rounds(dir.path(), "sess-1").unwrap_err();
        assert!(err.contains("read session dir"), "{err}");
    }

    #[test]
    fn load_prior_rounds_corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let state = session_state_dir(dir.path(), "sess-1");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("round-00.json"), "not json").unwrap();
        let err = load_prior_rounds(dir.path(), "sess-1").unwrap_err();
        assert!(err.contains("parse"), "{err}");
    }

    #[test]
    fn build_session_context_renders_rounds() {
        let ctx = build_session_context(&[
            sample_round("sess-1", 0, "first task"),
            sample_round("sess-1", 1, "second task"),
        ]);
        assert!(ctx.contains("[Session history — prior rounds]"), "{ctx}");
        assert!(ctx.contains("Round 1: first task"), "{ctx}");
        assert!(ctx.contains("Round 2: second task"), "{ctx}");
        assert!(ctx.contains("Files changed: a.rs"), "{ctx}");
        assert!(ctx.contains("[End session history]"), "{ctx}");
    }

    #[tokio::test]
    async fn save_round_state_numbers_files_sequentially() {
        let dir = tempfile::tempdir().unwrap();
        // Partial JSON is enough: every other field has a serde default.
        let result: CoderRunResult = serde_json::from_value(serde_json::json!({
            "backend": "test",
            "outcome": "succeeded",
            "summary": "wrote a.rs",
            "files_changed": ["a.rs"]
        }))
        .unwrap();
        save_round_state(dir.path(), "sess-1", "prompt one", &result).unwrap();
        save_round_state(dir.path(), "sess-1", "prompt two", &result).unwrap();

        let state = session_state_dir(dir.path(), "sess-1");
        let round0: SessionRound =
            serde_json::from_str(&std::fs::read_to_string(state.join("round-00.json")).unwrap())
                .unwrap();
        assert_eq!(round0.prompt, "prompt one");
        assert_eq!(round0.round, 0);
        let round1: SessionRound =
            serde_json::from_str(&std::fs::read_to_string(state.join("round-01.json")).unwrap())
                .unwrap();
        assert_eq!(round1.prompt, "prompt two");
        assert_eq!(round1.round, 1);
    }

    // ── read_request ───────────────────────────────────────────────────────────

    fn valid_request_json() -> String {
        let tuning = CoderTuning::default();
        let assembled = assemble_production_run(
            &tuning,
            liberado_coder_agent::assemble::entry::runner_surface(
                CoderTask::new("d1", "do the thing"),
                PathBuf::from("/tmp/ws"),
                None,
                Some(30),
            ),
        );
        serde_json::to_string(&assembled.request).unwrap()
    }

    #[tokio::test]
    async fn read_request_parses_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("request.json");
        std::fs::write(&path, valid_request_json()).unwrap();

        let req = read_request(Some(&path)).await.unwrap();
        assert_eq!(req.task.description, "do the thing");
        assert!(req.task.id.ends_with("d1") || req.task.id == "d1");
    }

    #[tokio::test]
    async fn read_request_rejects_bad_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("request.json");
        std::fs::write(&path, "{{{not json").unwrap();
        let err = read_request(Some(&path)).await.unwrap_err();
        assert!(err.contains("parse CoderRunRequest"), "{err}");
    }

    #[tokio::test]
    async fn read_request_missing_file_is_an_error() {
        let err = read_request(Some(Path::new("/nonexistent/request.json")))
            .await
            .unwrap_err();
        assert!(err.contains("read request"), "{err}");
    }

    // ── provider_profile / read_topology ───────────────────────────────────────

    #[test]
    fn provider_profile_named_finds_deepseek_in_defaults() {
        let profile = provider_profile_named(Topology::default(), "deepseek").unwrap();
        assert_eq!(profile.api_key_env, "DEEPSEEK_API_KEY");
    }

    #[test]
    fn provider_profile_named_finds_openrouter_in_defaults() {
        let profile = provider_profile_named(Topology::default(), "openrouter").unwrap();
        assert_eq!(profile.name, "openrouter");
    }

    #[test]
    fn provider_profile_reads_topology_provider() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("topology.toml"),
            r#"
provider = "custom"
[[providers]]
name = "custom"
base_url = "https://custom.example/v1"
default_model = "m"
api_key_env = "CUSTOM_API_KEY"
"#,
        )
        .unwrap();
        let topo = read_topology(dir.path()).unwrap();
        let profile = provider_profile_named(topo, "custom").unwrap();
        assert_eq!(profile.name, "custom");
        assert_eq!(profile.api_key_env, "CUSTOM_API_KEY");
    }

    #[test]
    fn provider_profile_unknown_provider_is_an_error() {
        let err = provider_profile_named(Topology::default(), "nope").unwrap_err();
        assert!(err.contains("is not declared"), "{err}");
    }

    #[test]
    fn read_topology_absent_file_is_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let topo = read_topology(dir.path()).unwrap();
        assert_eq!(topo.provider, "deepseek");
    }

    #[test]
    fn read_topology_malformed_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("topology.toml"), "provider = [not valid").unwrap();
        let err = read_topology(dir.path()).unwrap_err();
        assert!(err.contains("parse topology"), "{err}");
    }

    // ── push_enabled ───────────────────────────────────────────────────────────

    #[test]
    fn push_enabled_reads_the_opt_in_env() {
        for (val, want) in [("1", true), ("true", true), ("0", false), ("", false)] {
            unsafe { std::env::set_var("LIBERADO_CODER_PUSH", val) };
            assert_eq!(push_enabled(), want, "LIBERADO_CODER_PUSH={val:?}");
        }
        unsafe { std::env::remove_var("LIBERADO_CODER_PUSH") };
        assert!(!push_enabled(), "unset must be false");
    }

    // ── ensure_git_repo / configure_git_safe_directory ─────────────────────────

    /// A gitconfig with an identity, so `git commit` works without ambient config (CI-style).
    fn identity_gitconfig() -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            f.path(),
            "[user]\n\tname = test-runner\n\temail = runner@test.local\n",
        )
        .unwrap();
        f
    }

    fn temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().to_string_lossy().to_string();
        let out = liberado_common::process::std_command("git")
            .args(["-C", &p, "init", "-q"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git init: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::write(dir.path().join("seed.txt"), "seed\n").unwrap();
        let out = liberado_common::process::std_command("git")
            .args([
                "-C",
                &p,
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "add",
                "-A",
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = liberado_common::process::std_command("git")
            .args([
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
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        dir
    }

    #[tokio::test]
    async fn ensure_git_repo_accepts_an_existing_repo() {
        let repo = temp_repo();
        ensure_git_repo(repo.path())
            .await
            .expect("existing repo is fine");
        assert!(repo.path().join(".git").exists());
    }

    #[tokio::test]
    async fn ensure_git_repo_initialises_a_bare_workspace() {
        let _guard = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let cfg = identity_gitconfig();
        let prior = std::env::var_os("GIT_CONFIG_GLOBAL");
        unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", cfg.path()) };

        let result = ensure_git_repo(dir.path()).await;

        unsafe {
            match prior {
                Some(v) => std::env::set_var("GIT_CONFIG_GLOBAL", v),
                None => std::env::remove_var("GIT_CONFIG_GLOBAL"),
            }
        }
        result.expect("must init + baseline-commit a bare workspace");
        assert!(dir.path().join(".git").exists());
    }

    /// The `\?\` canonical form round-trips through git on this host, so idempotency can be
    /// asserted on file content: the second call must not append. A pre-seeded *foreign* entry
    /// (a different safe.directory) makes the membership check's other arm reachable — without
    /// it, only the exact-path arm is ever exercised.
    #[test]
    fn configure_git_safe_directory_is_scoped_and_idempotent() {
        // Sync test: block on the async mutex so the concurrent GIT_CONFIG_GLOBAL tests are excluded.
        let _guard = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(ENV_LOCK.lock());
        let repo = temp_repo();
        let cfg = identity_gitconfig();
        // A foreign entry from an earlier machine config: must not short-circuit the add.
        std::fs::write(
            cfg.path(),
            format!(
                "{}\n[safe]\n\tdirectory = C:/other/work\n",
                std::fs::read_to_string(cfg.path()).unwrap()
            ),
        )
        .unwrap();

        let prior = std::env::var_os("GIT_CONFIG_GLOBAL");
        unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", cfg.path()) };

        let first = configure_git_safe_directory(repo.path());
        let before = std::fs::read_to_string(cfg.path()).unwrap();
        let second = configure_git_safe_directory(repo.path());
        let after = std::fs::read_to_string(cfg.path()).unwrap();

        // Restore the env before any assertion, so a failed assert can't leave it poisoned.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("GIT_CONFIG_GLOBAL", v),
                None => std::env::remove_var("GIT_CONFIG_GLOBAL"),
            }
        }

        first.expect("first call must configure");
        second.expect("second call must succeed");
        assert_eq!(
            before, after,
            "a second call must not append a duplicate line:\n{after}"
        );
        assert_eq!(
            before.matches("directory = ").count(),
            2,
            "the foreign entry plus ours must both be present:\n{before}"
        );
    }

    /// The headless no-config provider factory builds an OpenAI-compatible provider wired to the
    /// configured model/base-url — construction must not touch the network.
    #[test]
    fn direct_provider_factory_builds_a_configured_provider() {
        let factory = DirectProviderFactory {
            api_key: "k".into(),
            base_url: "https://llm.test/v1".into(),
        };
        let role = CoderRoleConfig {
            model: "m3".into(),
            reasoning: Some("high".into()),
            ..Default::default()
        };
        let provider = factory
            .provider_for("coder", &role)
            .expect("constructs without network");
        assert_eq!(provider.model(), "m3");
    }
}
