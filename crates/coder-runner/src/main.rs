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

use liberado_coder_agent::{CoderProviderFactory, LiberadoLoopBackend};
use liberado_coder_core::{
    CoderBackend, CoderError, CoderGateConfig, CoderRoleConfig, CoderRunConfig, CoderRunRequest,
    CoderTask, CoderTuning, CommandPolicy, HashlineConfig, ProgressPolicy, SandboxSpec,
    VerifierSpec, WorkspaceRef,
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

    let tuning = read_tuning(config_dir.as_deref());
    let repo_map = if tuning.repo_map.enabled {
        repo_map::generate_repo_map(
            &workspace,
            &RepoMapOptions {
                max_map_tokens: tuning.repo_map.max_map_tokens,
                min_source_files: tuning.repo_map.min_source_files,
                ..Default::default()
            },
        )
        .await
    } else {
        None
    };

    let session_context = if let Some(ref sid) = session_id {
        let prior = load_prior_rounds(&workspace, sid)?;
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
    task_context.push_str(&workspace_summary);
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

    let request = CoderRunRequest {
        task: CoderTask {
            id: "task-1".to_string(),
            description: prompt.clone(),
            context: task_context,
            success_criteria: Vec::new(),
        },
        workspace: WorkspaceRef::new(workspace.to_string_lossy().to_string(), "HEAD"),
        config: CoderRunConfig {
            backend: "liberado-loop".to_string(),
            trace_dir: None,
            planner: disabled_role(),
            coder: CoderRoleConfig {
                model,
                prompt_path: None,
                prompt: Some(
                    "You are Liberado's coding agent. Edit files in the workspace to complete \
                     the task. Use these tools: \
                     read_file, write_file (auto-creates parent directories), edit_file, \
                     apply_patch, hashline_edit (line-anchored edits — use read_file first \
                     to get [path#TAG] headers, then hashline_edit with PUT/CUT/REM ops), \
                     search_text, list_files, list_symbols, \
                     git_status, git_diff, git_log, git_branch, git_commit, git_push, \
                     git_fetch, git_merge, run_command, run_command_background (start long \
                     builds/tests without blocking; use check_background to poll for results), \
                     validate. \
                     Git safe.directory is configured automatically. write_file creates \
                     missing directories. You have TWO attempts — if the first fails, \
                     you will see feedback and can retry. When done, call submit_report."
                        .to_string(),
                ),
                temperature: None,
                max_tokens: None,
                max_turns: Some(max_turns),
            },
            critic: disabled_role(),
            gate: CoderGateConfig {
                enabled: false,
                ..Default::default()
            },
            repair: None,
            sandbox: SandboxSpec::HostLocal,
            command_policy: CommandPolicy::default(),
            validation_command: None,
            verifiers: vec![VerifierSpec::GitNonemptyDiff {
                id: "nonempty-diff".into(),
            }],
            verify_policy: Default::default(),
            path_policy: Default::default(),
            progress: ProgressPolicy {
                max_attempts: 2,
                // `read_only_turn_limit` was pinned to 6 here (fatal at 12), which silently
                // overrode the shared default and starved exploration on anything spanning more
                // than a couple of files — the headless runner is the path used for harness-bench
                // and unattended runs, so it was the one place the tighter number hurt most.
                // Take the shared default instead; it is tuned in one place, `ProgressPolicy`.
                ..Default::default()
            },
            hashline: HashlineConfig {
                enabled: true,
                hash_length: 7,
            },
        },
        attempt: 0,
        prior_feedback: Vec::new(),
        strategist_directive: None,
    };

    let providers: Arc<dyn CoderProviderFactory> = match config_dir {
        Some(ref dir) => {
            let profile = provider_profile(Some(dir))?;
            Arc::new(OpenAiProfileProviderFactory::from_profile(profile)?)
        }
        None => Arc::new(DirectProviderFactory { api_key, base_url }),
    };

    let backend = LiberadoLoopBackend::with_provider_factory(providers);
    let result = backend.run(request).await.map_err(|error| {
        eprintln!("CoderError: {error:?}");
        format!("coder backend failed: {error}")
    })?;

    let json = serde_json::to_string_pretty(&result)
        .map_err(|error| format!("serialize coder result: {error}"))?;
    println!("{json}");

    if let Some(ref sid) = session_id
        && result.outcome == Outcome::Succeeded
    {
        save_round_state(&workspace, sid, &prompt, &result)?;
    }

    match result.outcome {
        Outcome::Succeeded => Ok(()),
        _ => Err(format!("task completed with outcome: {:?}", result.outcome)),
    }
}

fn disabled_role() -> CoderRoleConfig {
    CoderRoleConfig {
        model: "mock".to_string(),
        prompt_path: None,
        prompt: None,
        temperature: None,
        max_tokens: None,
        max_turns: Some(4),
    }
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
        let output = std::process::Command::new("git")
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

    let existing = std::process::Command::new("git")
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

    let output = std::process::Command::new("git")
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
            .with_extra_client_error_status(vec![429]);
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

fn read_tuning(config_dir: Option<&Path>) -> CoderTuning {
    let Some(dir) = config_dir else {
        return CoderTuning::default();
    };
    let path = dir.join("tuning.toml");
    if !path.exists() {
        return CoderTuning::default();
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return CoderTuning::default();
    };
    let Ok(value) = toml::from_str::<toml::Value>(&raw) else {
        return CoderTuning::default();
    };
    let coder_section = value.get("coder");
    CoderTuning::from_value(coder_section).unwrap_or_default()
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
                .with_extra_client_error_status(self.profile.extra_client_error_status.clone());
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
                ..
            } => {
                assert_eq!(prompt, "do thing");
                assert_eq!(workspace, PathBuf::from("/tmp/ws"));
                assert_eq!(model, Some("deepseek-v4-pro".to_string()));
                assert_eq!(max_turns, Some(15));
                assert_eq!(api_key_env, Some("OPENROUTER_API_KEY".to_string()));
                assert_eq!(base_url, Some("https://openrouter.ai/api/v1".to_string()));
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
}
