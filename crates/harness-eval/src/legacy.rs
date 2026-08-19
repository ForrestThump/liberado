//! Durable, repeatable cross-harness comparison runs.
//!
//! The comparison owns its worktrees, build caches, logs, sessions, traces, and saved Git refs.
//! This keeps orchestration policy in compiled code. Shell wrappers only need to pass arguments.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::Utc;
use liberado_common::path::child_process_path;
use liberado_common::process::{command, output_within, std_command};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::adapter::{AdapterPreflight, HarnessAdapter, HarnessExecution};
use crate::contract::{JobSpec, SAMPLING_OMITTED, default_run_order};
use crate::preflight::ResolvedCredential;

const MANIFEST_VERSION: u32 = 1;
const DEFAULT_COMPILE_TIMEOUT_SECS: u64 = 1_800;
const DEFAULT_MODEL: &str = "deepseek/deepseek-v4-flash";
const DEFAULT_PROVIDER: &str = "openrouter";
const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
const DEFAULT_API_KEY_ENV: &str = "OPENROUTER_API_KEY";
const DEFAULT_THINKING: &str = "high";
const DEFAULT_MAX_TURNS: u32 = 400;
const DEFAULT_RUN_TIMEOUT_SECS: u64 = 14_400;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarnessLayout {
    worktree: PathBuf,
    target_dir: PathBuf,
    artifacts: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompareManifest {
    version: u32,
    source_root: PathBuf,
    run_root: PathBuf,
    base_revision: String,
    base_commit: String,
    compile_timeout_secs: u64,
    harnesses: BTreeMap<String, HarnessLayout>,
}

#[derive(Debug, Clone)]
pub(crate) struct RunArgs {
    run_root: PathBuf,
    task: PathBuf,
    model: String,
    provider: String,
    base_url: String,
    api_key_env: String,
    thinking: String,
    max_turns: u32,
    sampling: String,
    run_order: Vec<String>,
    run_timeout_secs: u64,
    verifier_repair_attempts: u32,
    task_aware_context: bool,
    acceptance_overlay: Option<PathBuf>,
    liberado_bin: Option<PathBuf>,
    pi_bin: Option<PathBuf>,
    cancel_file: Option<PathBuf>,
}

/// Parsed `coder compare prepare` options.
struct PrepareOptions {
    run_root: Option<PathBuf>,
    source_root: Option<PathBuf>,
    revision: String,
    compile_timeout_secs: u64,
}

/// Parse the positional run-dir plus the optional flags.
fn parse_prepare_args(args: &[String]) -> Result<PrepareOptions, Box<dyn Error>> {
    let mut run_root = None;
    let mut source_root = None;
    let mut revision = "main".to_string();
    let mut compile_timeout_secs = DEFAULT_COMPILE_TIMEOUT_SECS;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                index += 1;
                source_root = Some(PathBuf::from(value(args, index, "--source")?));
            }
            "--commit" => {
                index += 1;
                revision = value(args, index, "--commit")?.to_string();
            }
            "--compile-timeout-secs" => {
                index += 1;
                compile_timeout_secs = value(args, index, "--compile-timeout-secs")?
                    .parse()
                    .map_err(|_| "--compile-timeout-secs must be a positive integer")?;
                if compile_timeout_secs == 0 {
                    return Err("--compile-timeout-secs must be a positive integer".into());
                }
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown flag for coder compare prepare: {flag}").into());
            }
            path => {
                if run_root.is_some() {
                    return Err("coder compare prepare takes one run directory".into());
                }
                run_root = Some(PathBuf::from(path));
            }
        }
        index += 1;
    }
    Ok(PrepareOptions {
        run_root,
        source_root,
        revision,
        compile_timeout_secs,
    })
}

/// Resolve the run directory, absolute (positional path or the repository root for the source).
fn resolve_run_root(run_root: Option<PathBuf>) -> Result<PathBuf, Box<dyn Error>> {
    absolute_unchecked(
        &run_root.ok_or("usage: liberado coder compare prepare <run-dir> [--commit <ref>]")?,
    )
}

fn resolve_source_root(source_root: Option<PathBuf>) -> Result<PathBuf, Box<dyn Error>> {
    match source_root {
        Some(path) => absolute(&path),
        None => absolute(&crate::repository_root()?),
    }
}

/// The base commit the worktrees will be frozen at.
fn resolve_base_commit(source_root: &Path, revision: &str) -> Result<String, Box<dyn Error>> {
    Ok(git_capture(
        &child_process_path(source_root),
        &["rev-parse", &format!("{revision}^{{commit}}")],
    )?
    .trim()
    .to_string())
}

pub fn prepare(args: &[String]) -> Result<(), Box<dyn Error>> {
    if args == ["-h"] || args == ["--help"] {
        println!(
            "usage: liberado coder compare prepare <run-dir> [--source <repo>] [--commit <ref>] \
             [--compile-timeout-secs <n>]"
        );
        return Ok(());
    }

    let opts = parse_prepare_args(args)?;
    let run_root = resolve_run_root(opts.run_root)?;
    let source_root = resolve_source_root(opts.source_root)?;
    let base_commit = resolve_base_commit(&source_root, &opts.revision)?;
    prepare_parsed(
        &run_root,
        &source_root,
        &opts.revision,
        &base_commit,
        opts.compile_timeout_secs,
    )
}

/// Prepare a comparison run directory from already-resolved inputs. This is the single execution
/// path shared by the `prepare` verb and the job coordinator: the coordinator calls it directly
/// with the preflight-resolved repository and commit instead of serializing them into argv.
pub(crate) fn prepare_parsed(
    run_root: &Path,
    source_root: &Path,
    revision: &str,
    base_commit: &str,
    compile_timeout_secs: u64,
) -> Result<(), Box<dyn Error>> {
    let run_root = child_process_path(run_root);
    let source_root = child_process_path(source_root);
    if run_root.exists() {
        return Err(format!(
            "comparison run directory already exists: {}",
            run_root.display()
        )
        .into());
    }

    let worktrees = run_root.join("worktrees");
    let targets = run_root.join("targets");
    let artifacts = run_root.join("artifacts");
    fs::create_dir_all(&worktrees)?;
    fs::create_dir_all(&targets)?;
    fs::create_dir_all(&artifacts)?;

    let mut harnesses = BTreeMap::new();
    for name in ["liberado", "pi"] {
        let layout = HarnessLayout {
            worktree: worktrees.join(name),
            target_dir: targets.join(name),
            artifacts: artifacts.join(name),
        };
        fs::create_dir_all(&layout.target_dir)?;
        fs::create_dir_all(layout.artifacts.join("traces"))?;
        fs::create_dir_all(layout.artifacts.join("sessions"))?;
        harnesses.insert(name.to_string(), layout);
    }

    let result = prepare_worktrees(&source_root, base_commit, &harnesses);
    if let Err(error) = result {
        cleanup_prepared_worktrees(&source_root, &harnesses);
        let _ = fs::remove_dir_all(&run_root);
        return Err(error);
    }

    let manifest = CompareManifest {
        version: MANIFEST_VERSION,
        source_root,
        run_root: run_root.clone(),
        base_revision: revision.to_string(),
        base_commit: base_commit.to_string(),
        compile_timeout_secs,
        harnesses,
    };
    write_json(&run_root.join("manifest.json"), &manifest)?;
    fs::write(
        run_root.join("README.txt"),
        "worktrees/  isolated Git worktrees at the pinned base\n\
         targets/    one Cargo cache per harness; never shared across source roots\n\
         artifacts/  stdout, stderr, sessions, traces, Git patches, and saved-result metadata\n",
    )?;

    println!("comparison prepared: {}", run_root.display());
    println!(
        "base: {} ({})",
        manifest.base_revision, manifest.base_commit
    );
    println!("compile timeout: {}s", manifest.compile_timeout_secs);
    for (name, layout) in &manifest.harnesses {
        println!("{name} worktree: {}", layout.worktree.display());
        println!("{name} target:   {}", layout.target_dir.display());
        println!("{name} artifacts:{}", layout.artifacts.display());
    }
    Ok(())
}

pub fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
    if args == ["-h"] || args == ["--help"] {
        println!(
            "usage: liberado coder compare run <run-dir> --task <file> [--model <id>] \
             [--provider <name>] [--base-url <url>] [--api-key-env <name>] \
             [--thinking <level>] [--max-turns <n>] [--run-timeout-secs <n>] \
             [--verifier-repair-attempts <n>] [--task-aware-context] \
             [--acceptance-overlay <dir>] \
             [--liberado-bin <path>] [--pi-bin <path>]"
        );
        return Ok(());
    }
    let parsed = parse_run_args(args)?;
    let credential = std::env::var(&parsed.api_key_env)
        .map_err(|_| format!("{} is not set in this process", parsed.api_key_env))?;
    run_parsed(parsed, ResolvedCredential::new(credential))
}

pub(crate) fn run_parsed(
    parsed: RunArgs,
    credential: ResolvedCredential,
) -> Result<(), Box<dyn Error>> {
    let manifest = load_manifest(&parsed.run_root)?;
    let task = fs::read_to_string(&parsed.task)?;
    if task.trim().is_empty() {
        return Err("comparison task file is empty".into());
    }
    fs::write(manifest.run_root.join("task.txt"), &task)?;
    let acceptance_overlay = capture_acceptance_overlay(&manifest, &parsed)?;
    write_run_config(&manifest, &parsed)?;
    write_run_pins(&manifest, &parsed, acceptance_overlay.as_deref())?;

    println!(
        "prewarming isolated Cargo caches (timeout {}s each)",
        manifest.compile_timeout_secs
    );
    for harness in ["liberado", "pi"] {
        warm_harness(&manifest, harness)?;
    }

    let run_slug = run_slug(&manifest.run_root);
    let liberado_session = format!("{run_slug}-liberado");
    let pi_session = format!("{run_slug}-pi");
    let liberado = LiberadoAdapter {
        manifest: &manifest,
        args: &parsed,
        task: &task,
        session_id: &liberado_session,
        credential: &credential,
    };
    let pi = PiAdapter {
        manifest: &manifest,
        args: &parsed,
        session_id: &pi_session,
        credential: &credential,
    };
    let mut adapters: Vec<&dyn HarnessAdapter> = vec![&liberado, &pi];
    // Run in the declared order, not a hardcoded one. Unknown ids (rejected earlier by the job
    // spec) sort last and therefore never run.
    adapters.sort_by_key(|adapter| {
        parsed
            .run_order
            .iter()
            .position(|id| id.as_str() == adapter.id())
            .unwrap_or(usize::MAX)
    });

    // Every adapter must be ready before the first paid model request.
    let preflights: std::collections::BTreeMap<String, Result<AdapterPreflight, String>> = adapters
        .iter()
        .map(|adapter| {
            (
                adapter.id().to_string(),
                adapter.preflight().map_err(|error| error.to_string()),
            )
        })
        .collect();

    let mut exits = std::collections::BTreeMap::new();
    let mut verifier_exits = std::collections::BTreeMap::new();
    for adapter in &adapters {
        let name = adapter.id();
        let preflight = &preflights[name];
        let (exit, verifier_exit) = run_with_verifier_repairs(
            &manifest,
            &parsed,
            name,
            || match preflight {
                Ok(_) => adapter.launch().map(|result| result.exit_code),
                Err(error) => Err(error.clone().into()),
            },
            |prompt, stem| adapter.run(prompt, stem),
            acceptance_overlay.as_deref(),
        );
        save_result(
            &manifest,
            name,
            Some(adapter.session_id()),
            Some(exit),
            Some(verifier_exit),
        )?;
        exits.insert(name.to_string(), exit);
        verifier_exits.insert(name.to_string(), verifier_exit);
    }

    for (name, exit) in &exits {
        println!("{name} exit: {exit}");
    }
    for (name, verifier_exit) in &verifier_exits {
        println!("{name} verifier exit: {verifier_exit}");
    }
    println!(
        "artifacts: {}",
        manifest.run_root.join("artifacts").display()
    );
    if exits
        .values()
        .chain(verifier_exits.values())
        .any(|code| *code != 0)
    {
        return Err(
            "one or more harnesses or common verifiers failed; work and artifacts were saved"
                .into(),
        );
    }
    Ok(())
}

/// Build the typed run arguments from a job spec, without the argv round-trip. The coordinator
/// calls [`run_parsed`] directly with this; the `run` verb parses argv into the same shape.
pub(crate) fn run_args_from_spec(
    spec: &JobSpec,
    job_root: &Path,
    execution_root: &Path,
    credential_environment: &str,
) -> RunArgs {
    let mut args = RunArgs {
        run_root: execution_root.to_path_buf(),
        task: job_root.join("input/task.txt"),
        model: spec.model.model.clone(),
        provider: spec.model.provider.clone(),
        base_url: spec.model.base_url.clone(),
        api_key_env: credential_environment.to_string(),
        thinking: spec.model.thinking.clone(),
        max_turns: spec.model.max_turns,
        sampling: spec.model.sampling.clone(),
        run_order: spec.run_order.clone(),
        run_timeout_secs: spec.limits.run_timeout_secs,
        verifier_repair_attempts: spec.limits.verifier_repair_attempts,
        task_aware_context: spec.task_aware_context,
        acceptance_overlay: spec
            .acceptance
            .as_ref()
            .map(|a| job_root.join(&a.directory)),
        liberado_bin: None,
        pi_bin: None,
        cancel_file: Some(job_root.join("cancel-requested")),
    };
    for harness in &spec.harnesses {
        if let Some(binary) = &harness.binary {
            match harness.id.as_str() {
                "liberado" => args.liberado_bin = Some(binary.clone()),
                "pi" => args.pi_bin = Some(binary.clone()),
                _ => {}
            }
        }
    }
    args
}

fn run_or_record_launch_error(
    manifest: &CompareManifest,
    name: &str,
    operation: impl FnOnce() -> Result<i32, Box<dyn Error>>,
) -> i32 {
    match operation() {
        Ok(code) => code,
        Err(error) => {
            if let Ok(layout) = harness(manifest, name) {
                let _ = fs::write(
                    layout.artifacts.join("launch-error.txt"),
                    format!("{error}\n"),
                );
            }
            eprintln!("{name} launch failed: {error}");
            127
        }
    }
}

/// Run a harness, then give it bounded repair turns when the common verifier finds an
/// actionable failure. This is outside the coding executor: the executor's in-session gates
/// cannot see the comparison acceptance overlay or the verifier process that runs after it exits.
fn run_with_verifier_repairs<F, R>(
    manifest: &CompareManifest,
    args: &RunArgs,
    name: &str,
    initial: F,
    mut repair: R,
    acceptance_overlay: Option<&Path>,
) -> (i32, i32)
where
    F: FnOnce() -> Result<i32, Box<dyn Error>>,
    R: FnMut(&str, &str) -> Result<i32, Box<dyn Error>>,
{
    let mut exit = run_or_record_launch_error(manifest, name, initial);
    let mut verifier = verify_harness(manifest, name, acceptance_overlay);
    for attempt in 1..=args.verifier_repair_attempts {
        if exit != 0 || !repairable_verifier_exit(verifier) {
            break;
        }
        let feedback = verifier_feedback(manifest, name);
        let prompt = format!(
            "The common comparison verifier rejected your completed work.\n\n\
             Verifier feedback:\n{feedback}\n\n\
             Repair attempt {attempt} of {}: inspect the failing evidence, make the smallest \
             correction in the existing workspace, run the relevant check, and submit the result \
             again. Do not undo correct work or change unrelated files.",
            args.verifier_repair_attempts
        );
        let stem = format!("repair-{attempt}-session");
        match repair(&prompt, &stem) {
            Ok(code) => exit = code,
            Err(error) => {
                let _ = fs::write(
                    harness(manifest, name)
                        .map(|layout| layout.artifacts.join("launch-error.txt"))
                        .unwrap_or_else(|_| PathBuf::from("launch-error.txt")),
                    format!("repair attempt {attempt} failed to launch: {error}\n"),
                );
                // The original harness session did finish successfully. Keep that exit code so
                // the result distinguishes an unrepairable verifier failure from a failed worker;
                // the launch-error artifact still makes the repair infrastructure problem clear.
                break;
            }
        }
        verifier = verify_harness(manifest, name, acceptance_overlay);
    }
    (exit, verifier)
}

fn repairable_verifier_exit(exit: i32) -> bool {
    // `cargo test` uses 101 for a test/build failure. Other non-zero exits include coordinator
    // setup, scope, and timeout failures, which are infrastructure decisions rather than model
    // repair opportunities.
    exit == 101
}

fn verifier_feedback(manifest: &CompareManifest, name: &str) -> String {
    let Ok(layout) = harness(manifest, name) else {
        return "verifier failed, but its artifact directory could not be resolved".to_string();
    };
    let mut feedback = String::new();
    for file in ["verifier.stderr.log", "verifier.stdout.log"] {
        let path = layout.artifacts.join(file);
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        if !feedback.is_empty() {
            feedback.push('\n');
        }
        feedback.push_str(file);
        feedback.push_str(":\n");
        feedback.push_str(&bounded_feedback(&text, 12_000));
    }
    if feedback.is_empty() {
        "the verifier exited unsuccessfully; inspect the saved verifier logs".to_string()
    } else {
        feedback
    }
}

fn bounded_feedback(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let boundary = |index: usize| {
        text.char_indices()
            .map(|(offset, _)| offset)
            .take_while(|offset| *offset <= index)
            .last()
            .unwrap_or(0)
    };
    let head = boundary(max_bytes / 2);
    let tail_start = boundary(text.len().saturating_sub(max_bytes - head));
    format!(
        "{}\n... [feedback clipped] ...\n{}",
        &text[..head],
        &text[tail_start..]
    )
}

pub fn save(args: &[String]) -> Result<(), Box<dyn Error>> {
    if args == ["-h"] || args == ["--help"] {
        println!(
            "usage: liberado coder compare save <run-dir> <liberado|pi> \
             [--session-id <id>] [--exit-code <n>] [--verifier-exit-code <n>]"
        );
        return Ok(());
    }
    let mut positional = Vec::new();
    let mut session_id = None;
    let mut exit_code = None;
    let mut verifier_exit_code = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--session-id" => {
                index += 1;
                session_id = Some(value(args, index, "--session-id")?.to_string());
            }
            "--exit-code" => {
                index += 1;
                exit_code = Some(
                    value(args, index, "--exit-code")?
                        .parse()
                        .map_err(|_| "--exit-code must be an integer")?,
                );
            }
            "--verifier-exit-code" => {
                index += 1;
                verifier_exit_code = Some(
                    value(args, index, "--verifier-exit-code")?
                        .parse()
                        .map_err(|_| "--verifier-exit-code must be an integer")?,
                );
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown flag for coder compare save: {flag}").into());
            }
            value => positional.push(value.to_string()),
        }
        index += 1;
    }
    if positional.len() != 2 {
        return Err("usage: liberado coder compare save <run-dir> <liberado|pi>".into());
    }
    let manifest = load_manifest(Path::new(&positional[0]))?;
    save_result(
        &manifest,
        &positional[1],
        session_id.as_deref(),
        exit_code,
        verifier_exit_code,
    )
}

fn parse_run_args(args: &[String]) -> Result<RunArgs, Box<dyn Error>> {
    let mut parsed = RunArgsBuilder::default();
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        if flag.starts_with('-') {
            if let Some((_, apply)) = RUN_FLAG_HANDLERS.iter().find(|(name, _)| *name == flag) {
                apply(args, &mut index, &mut parsed)?;
            } else {
                return Err(format!("unknown flag for coder compare run: {flag}").into());
            }
        } else {
            // The positional argument is the run directory.
            if parsed.run_root.is_some() {
                return Err("coder compare run takes one run directory".into());
            }
            parsed.run_root = Some(PathBuf::from(flag));
        }
        index += 1;
    }
    Ok(RunArgs {
        run_root: absolute(
            &parsed
                .run_root
                .ok_or("coder compare run requires <run-dir>")?,
        )?,
        task: absolute(
            &parsed
                .task
                .ok_or("coder compare run requires --task <file>")?,
        )?,
        model: parsed.model,
        provider: parsed.provider,
        base_url: parsed.base_url,
        api_key_env: parsed.api_key_env,
        thinking: parsed.thinking,
        max_turns: parsed.max_turns,
        sampling: parsed.sampling,
        run_order: parsed.run_order,
        run_timeout_secs: parsed.run_timeout_secs,
        verifier_repair_attempts: parsed.verifier_repair_attempts,
        task_aware_context: parsed.task_aware_context,
        acceptance_overlay: parsed.acceptance_overlay,
        liberado_bin: parsed.liberado_bin,
        pi_bin: parsed.pi_bin,
        cancel_file: parsed.cancel_file,
    })
}

/// Parsed `coder compare run` flags with the CLI's historical defaults, so a bare
/// `run <dir> --task <file>` behaves exactly as before.
#[derive(Debug)]
struct RunArgsBuilder {
    run_root: Option<PathBuf>,
    task: Option<PathBuf>,
    model: String,
    provider: String,
    base_url: String,
    api_key_env: String,
    thinking: String,
    max_turns: u32,
    sampling: String,
    run_order: Vec<String>,
    run_timeout_secs: u64,
    verifier_repair_attempts: u32,
    task_aware_context: bool,
    acceptance_overlay: Option<PathBuf>,
    liberado_bin: Option<PathBuf>,
    pi_bin: Option<PathBuf>,
    cancel_file: Option<PathBuf>,
}

impl Default for RunArgsBuilder {
    fn default() -> Self {
        Self {
            run_root: None,
            task: None,
            model: DEFAULT_MODEL.to_string(),
            provider: DEFAULT_PROVIDER.to_string(),
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
            thinking: DEFAULT_THINKING.to_string(),
            max_turns: DEFAULT_MAX_TURNS,
            sampling: SAMPLING_OMITTED.to_string(),
            run_order: default_run_order(),
            run_timeout_secs: DEFAULT_RUN_TIMEOUT_SECS,
            // Keep external verifier repair off for benchmark runs. Operators can opt in
            // explicitly; otherwise the comparison would measure the coordinator's recovery
            // policy, not the harness.
            verifier_repair_attempts: 0,
            task_aware_context: false,
            acceptance_overlay: None,
            liberado_bin: None,
            pi_bin: None,
            cancel_file: None,
        }
    }
}

/// One flag's parser: consumes the flag's value argument (via `value`) on `RunArgsBuilder`.
type FlagHandler = fn(&[String], &mut usize, &mut RunArgsBuilder) -> Result<(), Box<dyn Error>>;

macro_rules! string_run_flag {
    ($name:ident, $flag:literal, $field:ident) => {
        fn $name(
            args: &[String],
            index: &mut usize,
            parsed: &mut RunArgsBuilder,
        ) -> Result<(), Box<dyn Error>> {
            *index += 1;
            parsed.$field = value(args, *index, $flag)?.to_string();
            Ok(())
        }
    };
}

macro_rules! path_run_flag {
    ($name:ident, $flag:literal, $field:ident) => {
        fn $name(
            args: &[String],
            index: &mut usize,
            parsed: &mut RunArgsBuilder,
        ) -> Result<(), Box<dyn Error>> {
            *index += 1;
            parsed.$field = Some(PathBuf::from(value(args, *index, $flag)?));
            Ok(())
        }
    };
}

macro_rules! bool_run_flag {
    ($name:ident, $flag:literal, $field:ident) => {
        fn $name(
            _args: &[String],
            _index: &mut usize,
            parsed: &mut RunArgsBuilder,
        ) -> Result<(), Box<dyn Error>> {
            parsed.$field = true;
            Ok(())
        }
    };
}

string_run_flag!(model_flag, "--model", model);
string_run_flag!(provider_flag, "--provider", provider);
string_run_flag!(base_url_flag, "--base-url", base_url);
string_run_flag!(api_key_env_flag, "--api-key-env", api_key_env);
string_run_flag!(thinking_flag, "--thinking", thinking);

path_run_flag!(task_flag, "--task", task);
path_run_flag!(liberado_bin_flag, "--liberado-bin", liberado_bin);
path_run_flag!(pi_bin_flag, "--pi-bin", pi_bin);
path_run_flag!(cancel_file_flag, "--cancel-file", cancel_file);

bool_run_flag!(
    task_aware_context_flag,
    "--task-aware-context",
    task_aware_context
);

fn max_turns_flag(
    args: &[String],
    index: &mut usize,
    parsed: &mut RunArgsBuilder,
) -> Result<(), Box<dyn Error>> {
    *index += 1;
    parsed.max_turns = value(args, *index, "--max-turns")?
        .parse()
        .map_err(|_| "--max-turns must be a positive integer")?;
    if parsed.max_turns == 0 {
        return Err("--max-turns must be a positive integer".into());
    }
    Ok(())
}

fn sampling_flag(
    args: &[String],
    index: &mut usize,
    parsed: &mut RunArgsBuilder,
) -> Result<(), Box<dyn Error>> {
    *index += 1;
    parsed.sampling = value(args, *index, "--sampling")?.to_string();
    if parsed.sampling != SAMPLING_OMITTED {
        return Err(format!(
            "sampling '{}' is not yet applied by either client; only '{SAMPLING_OMITTED}' is supported",
            parsed.sampling
        )
        .into());
    }
    Ok(())
}

fn run_order_flag(
    args: &[String],
    index: &mut usize,
    parsed: &mut RunArgsBuilder,
) -> Result<(), Box<dyn Error>> {
    *index += 1;
    parsed.run_order = value(args, *index, "--run-order")?
        .split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect();
    if parsed.run_order.is_empty() {
        return Err("--run-order must name at least one harness".into());
    }
    Ok(())
}

fn run_timeout_flag(
    args: &[String],
    index: &mut usize,
    parsed: &mut RunArgsBuilder,
) -> Result<(), Box<dyn Error>> {
    *index += 1;
    parsed.run_timeout_secs = value(args, *index, "--run-timeout-secs")?
        .parse()
        .map_err(|_| "--run-timeout-secs must be a positive integer")?;
    if parsed.run_timeout_secs == 0 {
        return Err("--run-timeout-secs must be a positive integer".into());
    }
    Ok(())
}

fn verifier_repair_flag(
    args: &[String],
    index: &mut usize,
    parsed: &mut RunArgsBuilder,
) -> Result<(), Box<dyn Error>> {
    *index += 1;
    parsed.verifier_repair_attempts = value(args, *index, "--verifier-repair-attempts")?
        .parse()
        .map_err(|_| "--verifier-repair-attempts must be a non-negative integer")?;
    Ok(())
}

fn acceptance_overlay_flag(
    args: &[String],
    index: &mut usize,
    parsed: &mut RunArgsBuilder,
) -> Result<(), Box<dyn Error>> {
    *index += 1;
    parsed.acceptance_overlay = Some(absolute(&PathBuf::from(value(
        args,
        *index,
        "--acceptance-overlay",
    )?))?);
    Ok(())
}

const RUN_FLAG_HANDLERS: &[(&str, FlagHandler)] = &[
    ("--task", task_flag),
    ("--model", model_flag),
    ("--provider", provider_flag),
    ("--base-url", base_url_flag),
    ("--api-key-env", api_key_env_flag),
    ("--thinking", thinking_flag),
    ("--max-turns", max_turns_flag),
    ("--sampling", sampling_flag),
    ("--run-order", run_order_flag),
    ("--run-timeout-secs", run_timeout_flag),
    ("--verifier-repair-attempts", verifier_repair_flag),
    ("--task-aware-context", task_aware_context_flag),
    ("--acceptance-overlay", acceptance_overlay_flag),
    ("--liberado-bin", liberado_bin_flag),
    ("--pi-bin", pi_bin_flag),
    ("--cancel-file", cancel_file_flag),
];

fn prepare_worktrees(
    source_root: &Path,
    base_commit: &str,
    harnesses: &BTreeMap<String, HarnessLayout>,
) -> Result<(), Box<dyn Error>> {
    for layout in harnesses.values() {
        git_worktree_add(source_root, &layout.worktree, base_commit)?;
    }
    for layout in harnesses.values() {
        for sibling in ["turbovault", "turbomcp"] {
            let source = source_root.join(sibling);
            if !source.is_dir() {
                return Err(
                    format!("required sibling checkout is missing: {}", source.display()).into(),
                );
            }
            copy_path_dependency_tree(&source, &layout.worktree.join(sibling))?;
        }
    }
    Ok(())
}

fn cleanup_prepared_worktrees(source_root: &Path, harnesses: &BTreeMap<String, HarnessLayout>) {
    for layout in harnesses.values() {
        if layout.worktree.exists() {
            let _ = std_command("git")
                .arg("-C")
                .arg(source_root)
                .args(["worktree", "remove", "--force"])
                .arg(&layout.worktree)
                .status();
        }
    }
}

pub(crate) fn remove_job_worktrees(run_root: &Path) -> Result<(), Box<dyn Error>> {
    let manifest = load_manifest(run_root)?;
    for layout in manifest.harnesses.values() {
        if !layout.worktree.exists() {
            continue;
        }
        let output = std_command("git")
            .arg("-C")
            .arg(&manifest.source_root)
            .args(["worktree", "remove", "--force"])
            .arg(&layout.worktree)
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "could not remove completed worktree {}: {}",
                layout.worktree.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }
    }
    git_status(&manifest.source_root, &["worktree", "prune"])
}

fn git_worktree_add(source_root: &Path, path: &Path, commit: &str) -> Result<(), Box<dyn Error>> {
    let output = std_command("git")
        .arg("-C")
        .arg(source_root)
        .args(["worktree", "add", "--detach"])
        .arg(path)
        .arg(commit)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git worktree add {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into())
    }
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    if is_link_like(source)? {
        return Err(format!("refusing linked path dependency: {}", source.display()).into());
    }
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if is_link_like(&source_path)? {
            return Err(format!(
                "refusing link inside path dependency: {}",
                source_path.display()
            )
            .into());
        }
        if entry.file_type()?.is_dir() {
            copy_tree(&source_path, &destination_path)?;
        } else {
            fs::copy(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

fn copy_path_dependency_tree(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    if is_link_like(source)? {
        return Err(format!("refusing linked path dependency: {}", source.display()).into());
    }
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if is_link_like(&source_path)? {
            return Err(format!(
                "refusing link inside path dependency: {}",
                source_path.display()
            )
            .into());
        }
        if entry.file_type()?.is_dir()
            && matches!(
                entry.file_name().to_str(),
                Some(".git" | "target" | ".liberado" | ".fastembed_cache")
            )
        {
            continue;
        }
        if entry.file_type()?.is_dir() {
            copy_path_dependency_tree(&source_path, &destination_path)?;
        } else {
            fs::copy(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

fn is_link_like(path: &Path) -> io::Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        Ok(metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
    }
    #[cfg(not(windows))]
    Ok(false)
}

fn write_run_config(manifest: &CompareManifest, args: &RunArgs) -> Result<(), Box<dyn Error>> {
    let config = manifest.run_root.join("config");
    fs::create_dir_all(&config)?;
    let liberado = harness(manifest, "liberado")?;
    fs::write(
        config.join("topology.toml"),
        format!(
            "provider = {}\nvault_path = {}\n\n[[projects]]\nname = \"liberado\"\nroot = {}\nwrite_class = \"agent_writable\"\n",
            toml_string(&args.provider),
            toml_string(&path_text(&manifest.run_root.join("vault"))),
            toml_string(&path_text(&liberado.worktree)),
        ),
    )?;
    fs::create_dir_all(manifest.run_root.join("vault"))?;
    let repo_map = if args.task_aware_context {
        "\n[coder.repo_map]\ntask_aware = true\n"
    } else {
        ""
    };
    fs::write(
        config.join("tuning.toml"),
        format!(
            "[coder]\ntrace_dir = \"coder-traces\"\n\n\
             [coder.coder]\nmodel = {}\nmax_turns = {}\nreasoning = {}\n\n\
             [coder.command_policy]\ntimeout_secs = {}\noutput_max_bytes = 65536\ndeny = [\"git\"]\n\n\
             [coder.workspace]\nshared_target_dir = {}\nwarmup = false\nwarmup_timeout_secs = {}\n{}",
            toml_string(&args.model),
            args.max_turns,
            toml_string(&args.thinking),
            manifest.compile_timeout_secs,
            toml_string(&path_text(&liberado.target_dir)),
            manifest.compile_timeout_secs,
            repo_map,
        ),
    )?;
    Ok(())
}

fn write_run_pins(
    manifest: &CompareManifest,
    args: &RunArgs,
    acceptance_overlay: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
    let overlay_hash = acceptance_overlay
        .map(overlay_fingerprint)
        .transpose()?
        .unwrap_or_else(|| "none".into());
    fs::write(
        manifest.run_root.join("pins.txt"),
        format!(
            "base_revision={}\nbase_commit={}\nprovider={}\nmodel={}\nthinking={}\nliberado_max_turns={}\npi_turn_cap=unset (pi native default)\ntool_surface=native (full tool catalog)\nrun_order={}\ncompile_timeout_secs={}\nverifier_repair_attempts={}\ntask_aware_context={}\nacceptance_overlay_hash={}\nsampling={}\n",
            manifest.base_revision,
            manifest.base_commit,
            args.provider,
            args.model,
            args.thinking,
            args.max_turns,
            args.run_order.join(","),
            manifest.compile_timeout_secs,
            args.verifier_repair_attempts,
            args.task_aware_context,
            overlay_hash,
            args.sampling,
        ),
    )?;
    Ok(())
}

fn capture_acceptance_overlay(
    manifest: &CompareManifest,
    args: &RunArgs,
) -> Result<Option<PathBuf>, Box<dyn Error>> {
    let Some(source) = args.acceptance_overlay.as_deref() else {
        return Ok(None);
    };
    if !source.is_dir() {
        return Err(format!(
            "acceptance overlay is not a directory: {}",
            source.display()
        )
        .into());
    }
    let captured = manifest.run_root.join("acceptance-overlay");
    if captured.exists() {
        return Err(format!(
            "captured acceptance overlay already exists: {}",
            captured.display()
        )
        .into());
    }
    copy_tree(source, &captured)?;
    if overlay_files(&captured)?.is_empty() {
        return Err("acceptance overlay contains no files".into());
    }
    Ok(Some(captured))
}

fn overlay_fingerprint(root: &Path) -> Result<String, Box<dyn Error>> {
    let files = overlay_files(root)?;
    let mut digest = Sha256::new();
    for (relative, source) in files {
        let relative = path_text(&relative);
        let bytes = fs::read(source)?;
        digest.update((relative.len() as u64).to_le_bytes());
        digest.update(relative.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn overlay_files(root: &Path) -> Result<Vec<(PathBuf, PathBuf)>, Box<dyn Error>> {
    fn visit(
        root: &Path,
        directory: &Path,
        files: &mut Vec<(PathBuf, PathBuf)>,
    ) -> Result<(), Box<dyn Error>> {
        let mut entries: Vec<_> = fs::read_dir(directory)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if is_link_like(&path)? {
                return Err(
                    format!("refusing link in acceptance overlay: {}", path.display()).into(),
                );
            }
            if entry.file_type()?.is_dir() {
                visit(root, &path, files)?;
            } else {
                files.push((path.strip_prefix(root)?.to_path_buf(), path));
            }
        }
        Ok(())
    }

    if is_link_like(root)? {
        return Err(format!("refusing linked acceptance overlay: {}", root.display()).into());
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

struct InstalledAcceptanceOverlay {
    files: Vec<PathBuf>,
}

impl InstalledAcceptanceOverlay {
    fn install(source: &Path, worktree: &Path) -> Result<Self, Box<dyn Error>> {
        let sources = overlay_files(source)?;
        for (relative, _) in &sources {
            let target = worktree.join(relative);
            ensure_install_target_is_safe(worktree, relative, &target)?;
        }

        let mut installed = Self { files: Vec::new() };
        for (relative, source) in sources {
            let target = worktree.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(source, &target)?;
            installed.files.push(target);
        }
        Ok(installed)
    }
}

fn ensure_install_target_is_safe(
    worktree: &Path,
    relative: &Path,
    target: &Path,
) -> Result<(), Box<dyn Error>> {
    let mut current = worktree.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if is_link_like(&current)? {
                    return Err(format!(
                        "acceptance overlay path crosses a link: {}",
                        current.display()
                    )
                    .into());
                }
                if current == target || !metadata.is_dir() {
                    return Err(format!(
                        "acceptance overlay would overwrite model-visible path: {}",
                        current.display()
                    )
                    .into());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

impl Drop for InstalledAcceptanceOverlay {
    fn drop(&mut self) {
        for path in self.files.iter().rev() {
            let _ = fs::remove_file(path);
        }
    }
}

fn warm_harness(manifest: &CompareManifest, name: &str) -> Result<(), Box<dyn Error>> {
    let layout = harness(manifest, name)?;
    let mut cmd = command("cargo");
    cmd.args(["check", "--workspace", "--locked"])
        .current_dir(&layout.worktree)
        .env("CARGO_TARGET_DIR", &layout.target_dir);
    let output = run_async_command(
        &mut cmd,
        "cargo check --workspace --locked",
        Duration::from_secs(manifest.compile_timeout_secs),
    )?;
    fs::write(layout.artifacts.join("warmup.stdout.log"), &output.stdout)?;
    fs::write(layout.artifacts.join("warmup.stderr.log"), &output.stderr)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("{name} baseline warm-up failed with {}", output.status).into())
    }
}

fn verify_harness(
    manifest: &CompareManifest,
    name: &str,
    acceptance_overlay: Option<&Path>,
) -> i32 {
    let layout = match harness(manifest, name) {
        Ok(layout) => layout,
        Err(error) => {
            eprintln!("{name} verifier setup failed: {error}");
            return 125;
        }
    };
    let _installed_overlay = match acceptance_overlay
        .map(|source| InstalledAcceptanceOverlay::install(source, &layout.worktree))
        .transpose()
    {
        Ok(overlay) => overlay,
        Err(error) => {
            let message = format!("{name} acceptance overlay setup failed: {error}\n");
            eprint!("{message}");
            let _ = fs::write(layout.artifacts.join("verifier.stdout.log"), b"");
            let _ = fs::write(layout.artifacts.join("verifier.stderr.log"), &message);
            let now = Utc::now();
            let _ = fs::write(
                layout.artifacts.join("verifier-status.txt"),
                format!(
                    "started={}\nfinished={}\nexit=125\n",
                    now.to_rfc3339(),
                    now.to_rfc3339()
                ),
            );
            return 125;
        }
    };
    let mut cmd = command("cargo");
    cmd.args(["test", "--workspace", "--no-fail-fast"])
        .current_dir(&layout.worktree)
        .env("CARGO_TARGET_DIR", &layout.target_dir);
    let started = Utc::now();
    let result = run_async_command(
        &mut cmd,
        "cargo test --workspace --no-fail-fast",
        Duration::from_secs(manifest.compile_timeout_secs),
    );
    let finished = Utc::now();
    let (exit, stdout, stderr) = match result {
        Ok(output) => (
            output.status.code().unwrap_or(1),
            output.stdout,
            output.stderr,
        ),
        Err(error) => (124, Vec::new(), format!("{error}\n").into_bytes()),
    };
    if let Err(error) = fs::write(layout.artifacts.join("verifier.stdout.log"), stdout) {
        eprintln!("could not save {name} verifier stdout: {error}");
        return 125;
    }
    if let Err(error) = fs::write(layout.artifacts.join("verifier.stderr.log"), stderr) {
        eprintln!("could not save {name} verifier stderr: {error}");
        return 125;
    }
    if let Err(error) = fs::write(
        layout.artifacts.join("verifier-status.txt"),
        format!(
            "started={}\nfinished={}\nexit={}\n",
            started.to_rfc3339(),
            finished.to_rfc3339(),
            exit
        ),
    ) {
        eprintln!("could not save {name} verifier status: {error}");
        return 125;
    }
    exit
}

struct LiberadoAdapter<'a> {
    manifest: &'a CompareManifest,
    args: &'a RunArgs,
    task: &'a str,
    session_id: &'a str,
    credential: &'a ResolvedCredential,
}

impl HarnessAdapter for LiberadoAdapter<'_> {
    fn id(&self) -> &'static str {
        "liberado"
    }

    fn session_id(&self) -> &str {
        self.session_id
    }

    fn preflight(&self) -> Result<AdapterPreflight, Box<dyn Error>> {
        let layout = harness(self.manifest, self.id())?;
        let executable = ensure_liberado_runner(self.manifest, self.args, layout)?;
        Ok(AdapterPreflight {
            harness: self.id().to_string(),
            executable: path_text(&executable),
        })
    }

    fn launch(&self) -> Result<HarnessExecution, Box<dyn Error>> {
        let exit_code = run_liberado(
            self.manifest,
            self.args,
            self.task,
            self.session_id,
            self.credential,
            "session",
        )?;
        Ok(HarnessExecution {
            harness: self.id().to_string(),
            session_id: self.session_id.to_string(),
            exit_code,
        })
    }

    fn run(&self, prompt: &str, stem: &str) -> Result<i32, Box<dyn Error>> {
        run_liberado(
            self.manifest,
            self.args,
            prompt,
            self.session_id,
            self.credential,
            stem,
        )
    }
}

struct PiAdapter<'a> {
    manifest: &'a CompareManifest,
    args: &'a RunArgs,
    session_id: &'a str,
    credential: &'a ResolvedCredential,
}

impl HarnessAdapter for PiAdapter<'_> {
    fn id(&self) -> &'static str {
        "pi"
    }

    fn session_id(&self) -> &str {
        self.session_id
    }

    fn preflight(&self) -> Result<AdapterPreflight, Box<dyn Error>> {
        let executable = self
            .args
            .pi_bin
            .clone()
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "pi.cmd" } else { "pi" }));
        if self.args.pi_bin.is_some() && !executable.is_file() {
            return Err(format!("Pi binary does not exist: {}", executable.display()).into());
        }
        Ok(AdapterPreflight {
            harness: self.id().to_string(),
            executable: path_text(&executable),
        })
    }

    fn launch(&self) -> Result<HarnessExecution, Box<dyn Error>> {
        let prompt = format!("@{}", self.manifest.run_root.join("task.txt").display());
        let exit_code = run_pi(
            self.manifest,
            self.args,
            &prompt,
            self.session_id,
            self.credential,
            "session",
        )?;
        Ok(HarnessExecution {
            harness: self.id().to_string(),
            session_id: self.session_id.to_string(),
            exit_code,
        })
    }

    fn run(&self, prompt: &str, stem: &str) -> Result<i32, Box<dyn Error>> {
        let path = self
            .manifest
            .run_root
            .join("artifacts")
            .join("pi")
            .join(format!("{stem}.prompt.txt"));
        fs::write(&path, prompt)?;
        let prompt_arg = format!("@{}", path.display());
        run_pi(
            self.manifest,
            self.args,
            &prompt_arg,
            self.session_id,
            self.credential,
            stem,
        )
    }
}

fn run_liberado(
    manifest: &CompareManifest,
    args: &RunArgs,
    task: &str,
    session_id: &str,
    credential: &ResolvedCredential,
    stem: &str,
) -> Result<i32, Box<dyn Error>> {
    let layout = harness(manifest, "liberado")?;
    let binary = ensure_liberado_runner(manifest, args, layout)?;
    let mut cmd = std_command(&binary);
    cmd.args(["task", "run", "--prompt"])
        .arg(task)
        .arg("--workspace")
        .arg(&layout.worktree)
        .args(["--model", &args.model, "--max-turns"])
        .arg(args.max_turns.to_string())
        .arg("--config-dir")
        .arg(manifest.run_root.join("config"))
        .args([
            "--api-key-env",
            &args.api_key_env,
            "--base-url",
            &args.base_url,
        ])
        .args(["--session-id", session_id])
        .current_dir(&layout.worktree)
        .env("CARGO_TARGET_DIR", &layout.target_dir)
        .env("LIBERADO_CODER_PROVIDER", &args.provider)
        .env(&args.api_key_env, credential.expose());
    execute_logged(
        &mut cmd,
        layout,
        stem,
        args.run_timeout_secs,
        args.cancel_file.as_deref(),
    )
}

/// Resolve the runner from the same pinned worktree and isolated cache as the Liberado harness.
///
/// `cargo check` prewarms dependencies but does not create an executable. Building this binary in
/// the harness cache avoids an accidental dependency on whichever `target/debug` the caller last
/// happened to build, and makes the runner source match the comparison's pinned revision.
fn ensure_liberado_runner(
    manifest: &CompareManifest,
    args: &RunArgs,
    layout: &HarnessLayout,
) -> Result<PathBuf, Box<dyn Error>> {
    let binary = liberado_runner_path(layout, args.liberado_bin.as_deref());
    if args.liberado_bin.is_some() {
        if binary.is_file() {
            return Ok(binary);
        }
        return Err(format!("Liberado runner does not exist: {}", binary.display()).into());
    }
    if binary.is_file() {
        return Ok(binary);
    }

    let mut cmd = command("cargo");
    cmd.args(["build", "--locked", "-p", "liberado-coder-runner"])
        .current_dir(&layout.worktree)
        .env("CARGO_TARGET_DIR", &layout.target_dir);
    let output = run_async_command(
        &mut cmd,
        "cargo build --locked -p liberado-coder-runner",
        Duration::from_secs(manifest.compile_timeout_secs),
    );
    match output {
        Ok(output) => {
            fs::write(
                layout.artifacts.join("runner-build.stdout.log"),
                &output.stdout,
            )?;
            fs::write(
                layout.artifacts.join("runner-build.stderr.log"),
                &output.stderr,
            )?;
            if !output.status.success() {
                return Err(format!("Liberado runner build failed with {}", output.status).into());
            }
        }
        Err(error) => {
            fs::write(
                layout.artifacts.join("runner-build.stderr.log"),
                format!("{error}\n"),
            )?;
            return Err(format!("Liberado runner build failed: {error}").into());
        }
    }
    if binary.is_file() {
        Ok(binary)
    } else {
        Err(format!(
            "Liberado runner build succeeded but did not create: {}",
            binary.display()
        )
        .into())
    }
}

/// Run an async command from the synchronous comparison coordinator.
///
/// The worker is intentionally a blocking process with a filesystem wake loop. Calling
/// `Handle::current` or `block_in_place` here assumes an unrelated Tokio runtime and panics when
/// the worker is launched as a normal executable. Own the small runtime at this boundary instead.
fn run_async_command(
    command: &mut tokio::process::Command,
    program: &str,
    timeout: Duration,
) -> Result<std::process::Output, Box<dyn Error>> {
    let mut run = || -> Result<std::process::Output, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        runtime
            .block_on(output_within(command, program, timeout))
            .map_err(|error| error.to_string())
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::scope(|scope| {
            let result = scope
                .spawn(run)
                .join()
                .map_err(|_| "comparison subprocess runtime panicked".to_string())?;
            result.map_err(Into::into)
        })
    } else {
        run().map_err(Into::into)
    }
}

fn liberado_runner_path(layout: &HarnessLayout, explicit: Option<&Path>) -> PathBuf {
    explicit.map(PathBuf::from).unwrap_or_else(|| {
        layout.target_dir.join("debug").join(if cfg!(windows) {
            "liberado-coder-run.exe"
        } else {
            "liberado-coder-run"
        })
    })
}

fn run_pi(
    manifest: &CompareManifest,
    args: &RunArgs,
    prompt: &str,
    session_id: &str,
    credential: &ResolvedCredential,
    stem: &str,
) -> Result<i32, Box<dyn Error>> {
    let layout = harness(manifest, "pi")?;
    let binary = args
        .pi_bin
        .clone()
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "pi.cmd" } else { "pi" }));
    let mut cmd = std_command(&binary);
    cmd.args(["--provider", &args.provider, "--model", &args.model])
        .args(["--thinking", &args.thinking, "--mode", "json"])
        .args(["--session-id", session_id])
        .arg("--session-dir")
        .arg(layout.artifacts.join("sessions"))
        .arg("-p")
        .arg(prompt)
        .current_dir(&layout.worktree)
        .env("CARGO_TARGET_DIR", &layout.target_dir)
        .env(&args.api_key_env, credential.expose());
    execute_logged(
        &mut cmd,
        layout,
        stem,
        args.run_timeout_secs,
        args.cancel_file.as_deref(),
    )
}

fn execute_logged(
    command: &mut Command,
    layout: &HarnessLayout,
    stem: &str,
    timeout_secs: u64,
    cancel_file: Option<&Path>,
) -> Result<i32, Box<dyn Error>> {
    let stdout_path = layout.artifacts.join(format!("{stem}.stdout.log"));
    let stderr_path = layout.artifacts.join(format!("{stem}.stderr.log"));
    command
        .stdout(Stdio::from(File::create(&stdout_path)?))
        .stderr(Stdio::from(File::create(&stderr_path)?));
    let started = Utc::now();
    let mut child = command.spawn()?;
    #[cfg(windows)]
    let _process_tree = match WindowsProcessTree::assign(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let cancelled = cancel_file.is_some_and(Path::is_file);
        if cancelled || std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            if cancelled {
                return Err("comparison job was cancelled; harness process was killed".into());
            }
            return Err(format!(
                "harness process exceeded its {} second wall-clock limit and was killed",
                timeout_secs
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let finished = Utc::now();
    let exit = status.code().unwrap_or(1);
    fs::write(
        layout.artifacts.join("run-status.txt"),
        format!(
            "started={}\nfinished={}\nexit={}\n",
            started.to_rfc3339(),
            finished.to_rfc3339(),
            exit
        ),
    )?;
    Ok(exit)
}

#[cfg(windows)]
struct WindowsProcessTree {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl WindowsProcessTree {
    fn assign(child: &std::process::Child) -> Result<Self, Box<dyn Error>> {
        use std::mem::{size_of, zeroed};
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut information: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&raw const information).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        let assigned = configured != 0
            && unsafe { AssignProcessToJobObject(handle, child.as_raw_handle().cast()) } != 0;
        if !assigned {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Err(format!(
                "could not contain harness process tree in a Windows job object: {}",
                std::io::Error::last_os_error()
            )
            .into());
        }
        Ok(Self { handle })
    }
}

#[cfg(windows)]
impl Drop for WindowsProcessTree {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
    }
}

/// Commit any dirty harness state, point the archive branch at the new head, and return both.
fn commit_and_archive(
    layout: &HarnessLayout,
    name: &str,
    manifest: &CompareManifest,
    status_before: &str,
) -> Result<(String, String), Box<dyn Error>> {
    if !status_before.trim().is_empty() {
        git_status(&layout.worktree, &["add", "-A"])?;
        let message = format!("wip(compare): preserve {name} harness result");
        git_status(
            &layout.worktree,
            &[
                "-c",
                "user.name=Liberado Compare",
                "-c",
                "user.email=compare@liberado.local",
                "commit",
                "-m",
                &message,
            ],
        )?;
    }

    let head = git_capture(&layout.worktree, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let branch = format!(
        "archive/harness-compare/{}/{}",
        run_slug(&manifest.run_root),
        name
    );
    git_status(&layout.worktree, &["branch", "-f", &branch, &head])?;
    Ok((head, branch))
}

/// Write the git forensic artifacts: head, status-after-save, the diff, diff-stat, and log.
fn write_git_artifacts(
    layout: &HarnessLayout,
    manifest: &CompareManifest,
    head: &str,
) -> Result<(), Box<dyn Error>> {
    let git_dir = layout.artifacts.join("git");
    fs::write(git_dir.join("head.txt"), format!("{head}\n"))?;
    fs::write(
        git_dir.join("status-after-save.txt"),
        git_capture(&layout.worktree, &["status", "--short"])?,
    )?;
    fs::write(
        git_dir.join("diff.patch"),
        git_capture(
            &layout.worktree,
            &[
                "diff",
                "--binary",
                &format!("{}..HEAD", manifest.base_commit),
            ],
        )?,
    )?;
    fs::write(
        git_dir.join("diff-stat.txt"),
        git_capture(
            &layout.worktree,
            &["diff", "--stat", &format!("{}..HEAD", manifest.base_commit)],
        )?,
    )?;
    fs::write(
        git_dir.join("log.txt"),
        git_capture(&layout.worktree, &["log", "--oneline", "--decorate", "-5"])?,
    )?;
    Ok(())
}

/// The durable result.json for one harness.
#[allow(clippy::too_many_arguments)] // the json payload mirrors the save inputs 1:1
fn write_result_json(
    layout: &HarnessLayout,
    manifest: &CompareManifest,
    name: &str,
    head: &str,
    branch: &str,
    exit_code: Option<i32>,
    verifier_exit_code: Option<i32>,
    session_id: Option<&str>,
    status_before: &str,
) -> Result<(), Box<dyn Error>> {
    write_json(
        &layout.artifacts.join("result.json"),
        &serde_json::json!({
            "harness": name,
            "base_commit": manifest.base_commit,
            "head_commit": head,
            "archive_branch": branch,
            "exit_code": exit_code,
            "verifier_exit_code": verifier_exit_code,
            "session_id": session_id,
            "saved_at": Utc::now(),
            "had_uncommitted_changes": !status_before.trim().is_empty(),
        }),
    )
}

fn save_result(
    manifest: &CompareManifest,
    name: &str,
    session_id: Option<&str>,
    exit_code: Option<i32>,
    verifier_exit_code: Option<i32>,
) -> Result<(), Box<dyn Error>> {
    let layout = harness(manifest, name)?;
    fs::create_dir_all(layout.artifacts.join("git"))?;
    let status_before = git_capture(&layout.worktree, &["status", "--short"])?;
    fs::write(
        layout.artifacts.join("git").join("status-before-save.txt"),
        &status_before,
    )?;

    let (head, branch) = commit_and_archive(layout, name, manifest, &status_before)?;
    write_git_artifacts(layout, manifest, &head)?;
    copy_traces(layout, session_id)?;
    write_result_json(
        layout,
        manifest,
        name,
        &head,
        &branch,
        exit_code,
        verifier_exit_code,
        session_id,
        &status_before,
    )?;
    println!("saved {name}: {head} -> {branch}");
    Ok(())
}

fn copy_traces(layout: &HarnessLayout, session_id: Option<&str>) -> Result<(), Box<dyn Error>> {
    let source = layout.worktree.join("coder-traces");
    if !source.is_dir() {
        return Ok(());
    }
    let destination = layout.artifacts.join("traces");
    fs::create_dir_all(&destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if session_id.is_none_or(|prefix| text.starts_with(prefix)) {
            fs::copy(entry.path(), destination.join(name))?;
        }
    }
    Ok(())
}

fn load_manifest(run_root: &Path) -> Result<CompareManifest, Box<dyn Error>> {
    let run_root = absolute(run_root)?;
    let manifest: CompareManifest =
        serde_json::from_slice(&fs::read(run_root.join("manifest.json"))?)?;
    if manifest.version != MANIFEST_VERSION {
        return Err(format!(
            "unsupported comparison manifest version {}",
            manifest.version
        )
        .into());
    }
    Ok(manifest)
}

fn harness<'a>(
    manifest: &'a CompareManifest,
    name: &str,
) -> Result<&'a HarnessLayout, Box<dyn Error>> {
    manifest
        .harnesses
        .get(name)
        .ok_or_else(|| format!("unknown comparison harness '{name}'").into())
}

fn git_capture(path: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = std_command("git").arg("-C").arg(path).args(args).output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into())
    }
}

fn git_status(path: &Path, args: &[&str]) -> Result<(), Box<dyn Error>> {
    let output = std_command("git").arg("-C").arg(path).args(args).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into())
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), Box<dyn Error>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(path, bytes)?;
    Ok(())
}

fn absolute(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    Ok(path.canonicalize()?)
}

fn absolute_unchecked(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn value<'a>(args: &'a [String], index: usize, flag: &str) -> Result<&'a str, Box<dyn Error>> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} requires a value").into())
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn run_slug(path: &Path) -> String {
    let raw = path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("comparison");
    let slug: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    // A leading dot is a valid slug character but an invalid git ref component, and the slug is
    // embedded in archive branch names. Hidden run directories must not break result saving.
    let slug = slug.trim_start_matches('.').to_string();
    if slug.is_empty() {
        "comparison".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::run_or_record_launch_error;
    use super::{
        CompareManifest, DEFAULT_API_KEY_ENV, DEFAULT_BASE_URL, DEFAULT_MAX_TURNS, DEFAULT_MODEL,
        DEFAULT_PROVIDER, DEFAULT_RUN_TIMEOUT_SECS, DEFAULT_THINKING, HarnessLayout, RunArgs,
        SAMPLING_OMITTED, absolute, absolute_unchecked, bounded_feedback,
        capture_acceptance_overlay, copy_path_dependency_tree, copy_traces, copy_tree,
        default_run_order, ensure_install_target_is_safe, execute_logged, git_capture, git_status,
        git_worktree_add, liberado_runner_path, overlay_files, overlay_fingerprint, parse_run_args,
        path_text, repairable_verifier_exit, run_args_from_spec, run_async_command, run_slug,
        save_result, toml_string, value, verifier_feedback, write_run_config, write_run_pins,
    };
    use super::{parse_prepare_args, prepare, remove_job_worktrees};
    use crate::contract::{
        AcceptanceBundle, HarnessRequest, JOB_SPEC_VERSION, JobId, JobSpec, ModelPins,
        ResourceLimits, TaskBundle, VerifierProfile,
    };
    use chrono::Utc;
    use liberado_common::process::command;
    use std::collections::BTreeMap;
    #[cfg(windows)]
    use std::error::Error;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;

    /// parse_prepare_args: the positional run-dir plus the optional flags, with the trailing error
    /// guards (unknown flag, two run dirs, zero timeout, missing value).
    #[test]
    fn parse_prepare_args_parses_flags_and_positional() {
        let opts = parse_prepare_args(&[
            "run-dir".into(),
            "--source".into(),
            "/src".into(),
            "--commit".into(),
            "v1.0".into(),
            "--compile-timeout-secs".into(),
            "120".into(),
        ])
        .unwrap();
        assert_eq!(opts.run_root, Some(PathBuf::from("run-dir")));
        assert_eq!(opts.source_root, Some(PathBuf::from("/src")));
        assert_eq!(opts.revision, "v1.0");
        assert_eq!(opts.compile_timeout_secs, 120);

        assert!(
            parse_prepare_args(&["--bogus".into()]).is_err(),
            "an unknown flag must be rejected"
        );
        assert!(
            parse_prepare_args(&["a".into(), "b".into()]).is_err(),
            "two run directories must be rejected"
        );
        assert!(
            parse_prepare_args(&["--compile-timeout-secs".into(), "0".into()]).is_err(),
            "a zero timeout must be rejected"
        );
        assert!(
            parse_prepare_args(&["--source".into()]).is_err(),
            "a flag with no value must be rejected"
        );
    }

    /// Try to create a directory link; Some(path) only when the platform allowed it. Windows needs
    /// Developer Mode or an elevated shell, so tests that exercise link refusal degrade gracefully
    /// when that is unavailable.
    fn try_link(target: &Path, link: &Path) -> Option<PathBuf> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::symlink_dir;
            if symlink_dir(target, link).is_ok() {
                return Some(link.to_path_buf());
            }
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::symlink;
            if symlink(target, link).is_ok() {
                return Some(link.to_path_buf());
            }
        }
        None
    }

    fn layout() -> HarnessLayout {
        HarnessLayout {
            worktree: PathBuf::from("C:/comparison/worktree"),
            target_dir: PathBuf::from("C:/comparison/targets/liberado"),
            artifacts: PathBuf::from("C:/comparison/artifacts/liberado"),
        }
    }

    fn compare_manifest() -> (tempfile::TempDir, CompareManifest) {
        let temp = tempfile::tempdir().unwrap();
        let mut harnesses = BTreeMap::new();
        for name in ["liberado", "pi"] {
            harnesses.insert(
                name.to_string(),
                HarnessLayout {
                    worktree: temp.path().join("worktrees").join(name),
                    target_dir: temp.path().join("targets").join(name),
                    artifacts: temp.path().join("artifacts").join(name),
                },
            );
        }
        let manifest = CompareManifest {
            version: 1,
            source_root: temp.path().join("source"),
            run_root: temp.path().to_path_buf(),
            base_revision: "main".to_string(),
            base_commit: "abc123".to_string(),
            compile_timeout_secs: 1800,
            harnesses,
        };
        (temp, manifest)
    }

    fn run_args() -> RunArgs {
        RunArgs {
            run_root: PathBuf::new(),
            task: PathBuf::from("task.txt"),
            model: "deepseek/test".to_string(),
            provider: "openrouter".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            api_key_env: "OPENROUTER_API_KEY".to_string(),
            thinking: "high".to_string(),
            max_turns: 400,
            sampling: SAMPLING_OMITTED.to_string(),
            run_order: default_run_order(),
            run_timeout_secs: 14_400,
            verifier_repair_attempts: 0,
            task_aware_context: false,
            acceptance_overlay: None,
            liberado_bin: None,
            pi_bin: None,
            cancel_file: None,
        }
    }

    #[test]
    fn generated_config_keeps_native_tool_catalog_and_command_policy() {
        let (_temp, manifest) = compare_manifest();
        write_run_config(&manifest, &run_args()).unwrap();
        let tuning = fs::read_to_string(manifest.run_root.join("config/tuning.toml")).unwrap();
        // The coordinator must not narrow the model's tool surface: native Liberado offers the
        // full catalog. `deny = ["git"]` stays because it matches Liberado's native command
        // policy (CommandPolicy::default), not a coordinator-imposed narrowing.
        assert!(!tuning.contains("offered_tools"));
        assert!(tuning.contains("deny = [\"git\"]"));
    }

    #[test]
    fn pins_record_native_surface_and_honest_sampling_and_turn_budget() {
        let (_temp, manifest) = compare_manifest();
        write_run_pins(&manifest, &run_args(), None).unwrap();
        let pins = fs::read_to_string(manifest.run_root.join("pins.txt")).unwrap();
        assert!(pins.contains("tool_surface=native"));
        assert!(pins.contains("pi_turn_cap=unset"));
        assert!(pins.contains("sampling=omitted"));
        assert!(!pins.contains("client default"));
        assert!(!pins.contains("temperature omitted"));
    }

    #[test]
    fn sampling_flag_rejects_values_not_applied_to_clients() {
        let error = parse_run_args(&[
            "C:/comparison/run".to_string(),
            "--task".to_string(),
            "C:/comparison/task.txt".to_string(),
            "--sampling".to_string(),
            "0.1".to_string(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("not yet applied"));
    }

    #[test]
    fn run_order_flag_parses_and_defaults_to_liberado_first() {
        let temp = tempfile::tempdir().unwrap();
        let run = temp.path().join("run");
        fs::create_dir(&run).unwrap();
        let task = temp.path().join("task.txt");
        fs::write(&task, "do it").unwrap();

        let default = parse_run_args(&[
            run.to_string_lossy().into_owned(),
            "--task".to_string(),
            task.to_string_lossy().into_owned(),
        ])
        .unwrap();
        assert_eq!(default.run_order, vec!["liberado", "pi"]);

        let reversed = parse_run_args(&[
            run.to_string_lossy().into_owned(),
            "--task".to_string(),
            task.to_string_lossy().into_owned(),
            "--run-order".to_string(),
            "pi,liberado".to_string(),
        ])
        .unwrap();
        assert_eq!(reversed.run_order, vec!["pi", "liberado"]);
    }

    #[test]
    fn pins_record_the_run_order() {
        let (_temp, manifest) = compare_manifest();
        let mut args = run_args();
        args.run_order = vec!["pi".to_string(), "liberado".to_string()];
        write_run_pins(&manifest, &args, None).unwrap();
        let pins = fs::read_to_string(manifest.run_root.join("pins.txt")).unwrap();
        assert!(pins.contains("run_order=pi,liberado"));
    }

    #[test]
    fn default_runner_is_built_in_the_liberado_harness_target() {
        let path = liberado_runner_path(&layout(), None);
        assert_eq!(
            path,
            PathBuf::from("C:/comparison/targets/liberado/debug").join(if cfg!(windows) {
                "liberado-coder-run.exe"
            } else {
                "liberado-coder-run"
            })
        );
    }

    #[test]
    fn explicit_runner_path_remains_an_operator_override() {
        let explicit = PathBuf::from("C:/tools/liberado-coder-run.exe");
        assert_eq!(liberado_runner_path(&layout(), Some(&explicit)), explicit,);
    }

    #[test]
    fn verifier_repair_excludes_host_and_scope_failures() {
        assert!(repairable_verifier_exit(101));
        assert!(!repairable_verifier_exit(0));
        assert!(!repairable_verifier_exit(124));
        assert!(!repairable_verifier_exit(126));
    }

    #[test]
    fn verifier_feedback_is_bounded_without_splitting_utf8() {
        let feedback = bounded_feedback("αβγδεζηθ", 8);
        assert!(feedback.contains("[feedback clipped]"));
        assert!(feedback.is_char_boundary(feedback.len()));
    }

    #[test]
    fn path_dependency_copy_excludes_rebuildable_local_state() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir_all(source.join("crate/src")).unwrap();
        fs::create_dir_all(source.join("crate/target/debug")).unwrap();
        fs::create_dir_all(source.join(".git/objects")).unwrap();
        fs::write(source.join("crate/src/lib.rs"), "source").unwrap();
        fs::write(source.join("crate/target/debug/cache"), "cache").unwrap();
        fs::write(source.join(".git/objects/object"), "git").unwrap();
        copy_path_dependency_tree(&source, &destination).unwrap();
        assert!(destination.join("crate/src/lib.rs").is_file());
        assert!(!destination.join("crate/target").exists());
        assert!(!destination.join(".git").exists());
    }

    #[test]
    fn synchronous_comparison_can_run_async_processes_without_an_outer_runtime() {
        let mut command = command("rustc");
        command.arg("--version");
        let output =
            run_async_command(&mut command, "rustc --version", Duration::from_secs(30)).unwrap();
        assert!(output.status.success());
    }

    #[cfg(windows)]
    #[test]
    fn prepare_passes_plain_paths_to_git_for_canonical_windows_roots() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        assert!(
            Command::new("git")
                .arg("init")
                .arg(&repository)
                .status()
                .unwrap()
                .success()
        );
        fs::write(repository.join("README.md"), "base\n").unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["add", "README.md"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args([
                    "-c",
                    "user.name=Liberado Test",
                    "-c",
                    "user.email=liberado@example.invalid",
                    "commit",
                    "-m",
                    "base",
                ])
                .status()
                .unwrap()
                .success()
        );
        for sibling in ["turbovault", "turbomcp"] {
            fs::create_dir(repository.join(sibling)).unwrap();
            fs::write(repository.join(sibling).join("README.md"), sibling).unwrap();
        }
        let run_root = repository.join("comparison");
        prepare(&[
            run_root.to_string_lossy().into_owned(),
            "--source".to_string(),
            repository.to_string_lossy().into_owned(),
            "--commit".to_string(),
            "HEAD".to_string(),
        ])
        .unwrap();
        assert!(run_root.join("worktrees/liberado/.git").is_file());
        assert!(run_root.join("worktrees/pi/.git").is_file());
        remove_job_worktrees(&run_root).unwrap();
    }

    #[test]
    fn run_args_from_spec_maps_every_field_without_argv() {
        let spec = JobSpec {
            version: JOB_SPEC_VERSION,
            job_id: JobId::new(),
            submitted_at: Utc::now(),
            repository: PathBuf::from("C:/repo"),
            base_revision: "main".to_string(),
            task: TaskBundle::new("task.txt", "do it".to_string()).unwrap(),
            harnesses: vec![
                HarnessRequest {
                    id: "liberado".to_string(),
                    binary: Some(PathBuf::from("liberado.exe")),
                },
                HarnessRequest {
                    id: "pi".to_string(),
                    binary: Some(PathBuf::from("pi.exe")),
                },
            ],
            run_order: vec!["pi".to_string(), "liberado".to_string()],
            model: ModelPins {
                provider: "openrouter".to_string(),
                model: "deepseek/test".to_string(),
                base_url: "https://example.invalid".to_string(),
                credential_alias: "openrouter-default".to_string(),
                thinking: "high".to_string(),
                max_turns: 7,
                sampling: SAMPLING_OMITTED.to_string(),
            },
            limits: ResourceLimits {
                compile_timeout_secs: 11,
                run_timeout_secs: 13,
                minimum_free_bytes: 0,
                verifier_repair_attempts: 2,
            },
            verifier: VerifierProfile::WorkspaceTests,
            task_aware_context: true,
            acceptance: Some(AcceptanceBundle {
                directory: PathBuf::from("input/acceptance"),
                sha256: "x".to_string(),
                file_count: 1,
            }),
            experiment: None,
            experiment_id: String::new(),
        }
        .finalize()
        .unwrap();

        let job_root = PathBuf::from("C:/jobs/01");
        let execution_root = job_root.join("execution");
        let args = run_args_from_spec(&spec, &job_root, &execution_root, "OPENROUTER_API_KEY");

        assert_eq!(args.run_root, execution_root);
        assert_eq!(args.task, job_root.join("input/task.txt"));
        assert_eq!(args.model, "deepseek/test");
        assert_eq!(args.provider, "openrouter");
        assert_eq!(args.base_url, "https://example.invalid");
        assert_eq!(args.api_key_env, "OPENROUTER_API_KEY");
        assert_eq!(args.thinking, "high");
        assert_eq!(args.max_turns, 7);
        assert_eq!(args.sampling, SAMPLING_OMITTED);
        assert_eq!(args.run_order, vec!["pi", "liberado"]);
        assert_eq!(args.run_timeout_secs, 13);
        assert_eq!(args.verifier_repair_attempts, 2);
        assert!(args.task_aware_context);
        assert_eq!(
            args.acceptance_overlay,
            Some(job_root.join("input/acceptance"))
        );
        assert_eq!(args.liberado_bin, Some(PathBuf::from("liberado.exe")));
        assert_eq!(args.pi_bin, Some(PathBuf::from("pi.exe")));
        assert_eq!(args.cancel_file, Some(job_root.join("cancel-requested")));
    }

    fn args_fixture(temp: &tempfile::TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
        let run = temp.path().join("run");
        fs::create_dir(&run).unwrap();
        let task = temp.path().join("task.txt");
        fs::write(&task, "do it").unwrap();
        (run, task)
    }

    #[test]
    fn parse_run_args_applies_every_flag() {
        let temp = tempfile::tempdir().unwrap();
        let (run, task) = args_fixture(&temp);
        let overlay = temp.path().join("overlay");
        fs::create_dir(&overlay).unwrap();
        fs::write(overlay.join("a.txt"), "a").unwrap();

        let args = parse_run_args(&[
            run.to_string_lossy().into_owned(),
            "--task".to_string(),
            task.to_string_lossy().into_owned(),
            "--model".to_string(),
            "deepseek/test".to_string(),
            "--provider".to_string(),
            "openrouter".to_string(),
            "--base-url".to_string(),
            "https://example.invalid/v1".to_string(),
            "--api-key-env".to_string(),
            "MY_KEY".to_string(),
            "--thinking".to_string(),
            "low".to_string(),
            "--max-turns".to_string(),
            "7".to_string(),
            "--run-timeout-secs".to_string(),
            "9".to_string(),
            "--verifier-repair-attempts".to_string(),
            "2".to_string(),
            "--task-aware-context".to_string(),
            "--acceptance-overlay".to_string(),
            overlay.to_string_lossy().into_owned(),
            "--liberado-bin".to_string(),
            "liberado.exe".to_string(),
            "--pi-bin".to_string(),
            "pi.exe".to_string(),
            "--cancel-file".to_string(),
            "cancel.txt".to_string(),
        ])
        .unwrap();

        assert_eq!(args.run_root, run.canonicalize().unwrap());
        assert_eq!(args.task, task.canonicalize().unwrap());
        assert_eq!(args.model, "deepseek/test");
        assert_eq!(args.provider, "openrouter");
        assert_eq!(args.base_url, "https://example.invalid/v1");
        assert_eq!(args.api_key_env, "MY_KEY");
        assert_eq!(args.thinking, "low");
        assert_eq!(args.max_turns, 7);
        assert_eq!(args.run_timeout_secs, 9);
        assert_eq!(args.verifier_repair_attempts, 2);
        assert!(args.task_aware_context);
        assert_eq!(
            args.acceptance_overlay,
            Some(overlay.canonicalize().unwrap())
        );
        assert_eq!(args.liberado_bin, Some(PathBuf::from("liberado.exe")));
        assert_eq!(args.pi_bin, Some(PathBuf::from("pi.exe")));
        assert_eq!(args.cancel_file, Some(PathBuf::from("cancel.txt")));
    }

    #[test]
    fn parse_run_args_applies_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let (run, task) = args_fixture(&temp);
        let args = parse_run_args(&[
            run.to_string_lossy().into_owned(),
            "--task".to_string(),
            task.to_string_lossy().into_owned(),
        ])
        .unwrap();
        assert_eq!(args.model, DEFAULT_MODEL);
        assert_eq!(args.provider, DEFAULT_PROVIDER);
        assert_eq!(args.base_url, DEFAULT_BASE_URL);
        assert_eq!(args.api_key_env, DEFAULT_API_KEY_ENV);
        assert_eq!(args.thinking, DEFAULT_THINKING);
        assert_eq!(args.max_turns, DEFAULT_MAX_TURNS);
        assert_eq!(args.sampling, SAMPLING_OMITTED);
        assert_eq!(args.run_timeout_secs, DEFAULT_RUN_TIMEOUT_SECS);
        assert_eq!(args.verifier_repair_attempts, 0);
        assert!(!args.task_aware_context);
        assert!(args.acceptance_overlay.is_none());
    }

    #[test]
    fn parse_run_args_rejects_bad_input() {
        let temp = tempfile::tempdir().unwrap();
        let run = temp.path().join("run");
        fs::create_dir(&run).unwrap();
        let task = temp.path().join("task.txt");
        fs::write(&task, "do it").unwrap();

        let err = parse_run_args(&[
            run.to_string_lossy().into_owned(),
            "--task".to_string(),
            task.to_string_lossy().into_owned(),
            "--bogus".to_string(),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("unknown flag"), "{err}");

        let err = parse_run_args(&[
            run.to_string_lossy().into_owned(),
            "--task".to_string(),
            task.to_string_lossy().into_owned(),
            "--max-turns".to_string(),
            "0".to_string(),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("positive integer"), "{err}");
        let err = parse_run_args(&[
            run.to_string_lossy().into_owned(),
            "--task".to_string(),
            task.to_string_lossy().into_owned(),
            "--max-turns".to_string(),
            "abc".to_string(),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("positive integer"), "{err}");
        let err = parse_run_args(&[
            run.to_string_lossy().into_owned(),
            "--task".to_string(),
            task.to_string_lossy().into_owned(),
            "--run-timeout-secs".to_string(),
            "0".to_string(),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("positive integer"), "{err}");
        let err = parse_run_args(&[
            run.to_string_lossy().into_owned(),
            "--task".to_string(),
            task.to_string_lossy().into_owned(),
            "--run-order".to_string(),
            " , ".to_string(),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("at least one harness"), "{err}");
        let err = parse_run_args(&[
            run.to_string_lossy().into_owned(),
            "--task".to_string(),
            task.to_string_lossy().into_owned(),
            "--task".to_string(),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("requires a value"), "{err}");
        let err = parse_run_args(&[
            run.to_string_lossy().into_owned(),
            run.to_string_lossy().into_owned(),
            "--task".to_string(),
            task.to_string_lossy().into_owned(),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("one run directory"), "{err}");

        // Missing required positional/flag arguments.
        let err = parse_run_args(&["--task".to_string(), task.to_string_lossy().into_owned()])
            .unwrap_err();
        assert!(err.to_string().contains("requires <run-dir>"), "{err}");
        let err = parse_run_args(&[run.to_string_lossy().into_owned()]).unwrap_err();
        assert!(err.to_string().contains("--task"), "{err}");
    }

    #[test]
    fn verifier_feedback_renders_logs_or_a_generic_message() {
        // Unknown harness name: the artifact directory cannot be resolved.
        let (temp, manifest) = compare_manifest();
        let feedback = verifier_feedback(&manifest, "nope");
        assert!(feedback.contains("could not be resolved"), "{feedback}");

        // Logs present: feedback names each non-empty file and quotes its content.
        let layout = manifest.harnesses["liberado"].clone();
        fs::create_dir_all(&layout.artifacts).unwrap();
        fs::write(
            layout.artifacts.join("verifier.stderr.log"),
            "line one\nline two\n",
        )
        .unwrap();
        let feedback = verifier_feedback(&manifest, "liberado");
        assert!(feedback.contains("verifier.stderr.log:"), "{feedback}");
        assert!(feedback.contains("line two"), "{feedback}");

        // Only empty logs: the generic fallback, not a bare file name.
        fs::write(layout.artifacts.join("verifier.stdout.log"), "  \n").unwrap();
        fs::remove_file(layout.artifacts.join("verifier.stderr.log")).unwrap();
        let feedback = verifier_feedback(&manifest, "liberado");
        assert!(
            feedback.contains("inspect the saved verifier logs"),
            "{feedback}"
        );
        let _ = temp;
    }

    #[test]
    fn bounded_feedback_passes_short_text_and_clips_long_text() {
        assert_eq!(bounded_feedback("short", 100), "short");
        let long = "x".repeat(10_000);
        let clipped = bounded_feedback(&long, 100);
        assert!(clipped.contains("[feedback clipped]"), "{clipped}");
        assert!(clipped.starts_with("xxxxx"), "head must survive: {clipped}");
        assert!(clipped.ends_with('x'), "tail must survive");
        // The two kept halves together do not exceed the cap (plus the marker).
        assert!(clipped.len() < 200);
    }

    #[test]
    fn copy_tree_copies_recursively_and_refuses_links() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir_all(source.join("nested/deep")).unwrap();
        fs::write(source.join("root.txt"), "root").unwrap();
        fs::write(source.join("nested/a.txt"), "a").unwrap();
        fs::write(source.join("nested/deep/b.txt"), "b").unwrap();
        copy_tree(&source, &destination).unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("nested/deep/b.txt")).unwrap(),
            "b"
        );
        assert_eq!(
            fs::read_to_string(destination.join("root.txt")).unwrap(),
            "root"
        );

        // A link at the root is refused outright.
        if let Some(link) = try_link(&source, &source.join("linked")) {
            let err = copy_tree(&link, &temp.path().join("dest2")).unwrap_err();
            assert!(err.to_string().contains("refusing"), "{err}");
            let _ = fs::remove_file(&link);
        }
    }

    #[test]
    fn capture_acceptance_overlay_states() {
        let (temp, manifest) = compare_manifest();
        let run_args = run_args();

        // No overlay configured: Ok(None).
        assert!(
            capture_acceptance_overlay(&manifest, &run_args)
                .unwrap()
                .is_none()
        );

        // Configured but not a directory.
        let mut args = run_args.clone();
        args.acceptance_overlay = Some(temp.path().join("missing-dir"));
        let err = capture_acceptance_overlay(&manifest, &args).unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{err}");

        // Empty overlay directory.
        let empty = temp.path().join("empty-overlay");
        fs::create_dir(&empty).unwrap();
        let mut args = run_args.clone();
        args.acceptance_overlay = Some(empty);
        let err = capture_acceptance_overlay(&manifest, &args).unwrap_err();
        assert!(err.to_string().contains("contains no files"), "{err}");
        // The failed capture left its empty destination behind; the next capture must start clean.
        fs::remove_dir_all(manifest.run_root.join("acceptance-overlay")).unwrap();

        // A real overlay is copied into the run root.
        let overlay = temp.path().join("overlay");
        fs::create_dir_all(overlay.join("tests")).unwrap();
        fs::write(overlay.join("tests/golden.txt"), "golden").unwrap();
        let mut args = run_args.clone();
        args.acceptance_overlay = Some(overlay);
        let captured = capture_acceptance_overlay(&manifest, &args)
            .unwrap()
            .unwrap();
        assert_eq!(captured, manifest.run_root.join("acceptance-overlay"));
        assert_eq!(
            fs::read_to_string(captured.join("tests/golden.txt")).unwrap(),
            "golden"
        );

        // A second capture refuses to overwrite the first.
        let err = capture_acceptance_overlay(&manifest, &args).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn overlay_files_are_sorted_relative_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("overlay");
        fs::create_dir_all(root.join("z/deep")).unwrap();
        fs::create_dir_all(root.join("b")).unwrap();
        fs::write(root.join("a.txt"), "a").unwrap();
        fs::write(root.join("z/deep/m.txt"), "m").unwrap();
        fs::write(root.join("b/c.txt"), "c").unwrap();
        let files = overlay_files(&root).unwrap();
        let relatives: Vec<_> = files.iter().map(|(r, _)| r).collect();
        assert_eq!(
            relatives,
            vec![
                &PathBuf::from("a.txt"),
                &PathBuf::from("b/c.txt"),
                &PathBuf::from("z/deep/m.txt"),
            ]
        );
        // Every source path resolves under the root.
        for (relative, source) in &files {
            assert_eq!(*source, root.join(relative));
        }
    }

    #[test]
    fn overlay_fingerprint_is_deterministic_and_content_sensitive() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("overlay");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), "one").unwrap();
        fs::write(root.join("sub/b.txt"), "two").unwrap();
        let first = overlay_fingerprint(&root).unwrap();
        let second = overlay_fingerprint(&root).unwrap();
        assert_eq!(first, second, "fingerprint must be deterministic");
        assert_eq!(first.len(), 64, "sha256 hex");

        fs::write(root.join("sub/b.txt"), "TWO").unwrap();
        let changed = overlay_fingerprint(&root).unwrap();
        assert_ne!(first, changed, "content change must change the fingerprint");

        fs::write(root.join("c.txt"), "three").unwrap();
        let more = overlay_fingerprint(&root).unwrap();
        assert_ne!(first, more, "a new file must change the fingerprint");
    }

    #[test]
    fn ensure_install_target_is_safe_rejects_overwrites_and_links() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("worktree");
        fs::create_dir_all(worktree.join("existing-dir")).unwrap();
        fs::write(worktree.join("existing-file.txt"), "keep").unwrap();

        // A clean target path is fine.
        ensure_install_target_is_safe(
            &worktree,
            &PathBuf::from("new/path.txt"),
            &worktree.join("new/path.txt"),
        )
        .unwrap();

        // Crossing an existing *file* component means an overwrite of model-visible state.
        let err = ensure_install_target_is_safe(
            &worktree,
            &PathBuf::from("existing-file.txt/child"),
            &worktree.join("existing-file.txt/child"),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("would overwrite model-visible path"),
            "{err}"
        );

        // The final target itself being an existing file is also refused.
        let err = ensure_install_target_is_safe(
            &worktree,
            &PathBuf::from("existing-file.txt"),
            &worktree.join("existing-file.txt"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("would overwrite"), "{err}");

        // Crossing a link is refused (when the platform lets us make one).
        if let Some(link) = try_link(&worktree.join("existing-dir"), &worktree.join("linked-dir")) {
            let err = ensure_install_target_is_safe(
                &worktree,
                &PathBuf::from("linked-dir/file"),
                &worktree.join("linked-dir/file"),
            )
            .unwrap_err();
            assert!(err.to_string().contains("crosses a link"), "{err}");
            let _ = fs::remove_dir_all(&link);
        }
    }

    #[test]
    fn write_run_config_emits_topology_and_tuning() {
        let (temp, manifest) = compare_manifest();
        let args = run_args();
        write_run_config(&manifest, &args).unwrap();

        let topology = fs::read_to_string(manifest.run_root.join("config/topology.toml")).unwrap();
        assert!(topology.contains("provider = \"openrouter\""), "{topology}");
        assert!(topology.contains("vault_path ="), "{topology}");
        assert!(topology.contains("[[projects]]"), "{topology}");
        assert!(topology.contains("name = \"liberado\""), "{topology}");

        let tuning = fs::read_to_string(manifest.run_root.join("config/tuning.toml")).unwrap();
        assert!(tuning.contains("model = \"deepseek/test\""), "{tuning}");
        assert!(tuning.contains("max_turns = 400"), "{tuning}");
        assert!(tuning.contains("reasoning = \"high\""), "{tuning}");
        assert!(tuning.contains("timeout_secs = 1800"), "{tuning}");
        assert!(tuning.contains("shared_target_dir ="), "{tuning}");
        // task-aware context is off by default: no repo_map section.
        assert!(!tuning.contains("[coder.repo_map]"), "{tuning}");

        let mut args = run_args();
        args.task_aware_context = true;
        args.max_turns = 5;
        write_run_config(&manifest, &args).unwrap();
        let tuning = fs::read_to_string(manifest.run_root.join("config/tuning.toml")).unwrap();
        assert!(tuning.contains("[coder.repo_map]"), "{tuning}");
        assert!(tuning.contains("task_aware = true"), "{tuning}");
        assert!(tuning.contains("max_turns = 5"), "{tuning}");
        let _ = temp;
    }

    #[test]
    fn write_run_pins_records_the_overlay_hash() {
        let (temp, manifest) = compare_manifest();
        let args = run_args();
        write_run_pins(&manifest, &args, None).unwrap();
        let pins = fs::read_to_string(manifest.run_root.join("pins.txt")).unwrap();
        assert!(pins.contains("acceptance_overlay_hash=none"), "{pins}");

        let overlay = temp.path().join("overlay");
        fs::create_dir(&overlay).unwrap();
        fs::write(overlay.join("a.txt"), "a").unwrap();
        write_run_pins(&manifest, &args, Some(&overlay)).unwrap();
        let pins = fs::read_to_string(manifest.run_root.join("pins.txt")).unwrap();
        assert!(
            pins.contains("acceptance_overlay_hash=") && !pins.contains("=none"),
            "{pins}"
        );
        assert!(pins.contains("verifier_repair_attempts=0"), "{pins}");
        assert!(pins.contains("task_aware_context=false"), "{pins}");
        assert!(pins.contains("base_commit=abc123"), "{pins}");
    }

    #[test]
    fn git_worktree_add_creates_a_detached_worktree_and_rejects_bad_commits() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        fs::create_dir_all(&repository).unwrap();
        assert!(
            Command::new("git")
                .arg("init")
                .arg(&repository)
                .status()
                .unwrap()
                .success()
        );
        fs::write(repository.join("README.md"), "base\n").unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["add", "README.md"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args([
                    "-c",
                    "user.name=Liberado Test",
                    "-c",
                    "user.email=liberado@example.invalid",
                    "commit",
                    "-m",
                    "base",
                ])
                .status()
                .unwrap()
                .success()
        );
        let base = Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let base = String::from_utf8_lossy(&base.stdout).trim().to_string();

        let worktree = temp.path().join("worktree");
        git_worktree_add(&repository, &worktree, &base).unwrap();
        assert!(worktree.join("README.md").is_file());
        assert!(worktree.join(".git").is_file(), "detached worktree");

        // An unknown commit fails with a descriptive error.
        let err = git_worktree_add(
            &repository,
            &temp.path().join("missing"),
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        )
        .unwrap_err();
        assert!(err.to_string().contains("git worktree add"), "{err}");
    }

    #[test]
    fn path_helpers_are_predictable() {
        let temp = tempfile::tempdir().unwrap();
        let existing = temp.path().join("file.txt");
        fs::write(&existing, "x").unwrap();
        assert_eq!(
            absolute(&existing).unwrap(),
            existing.canonicalize().unwrap()
        );
        assert!(absolute(&PathBuf::from("missing-file-xyz")).is_err());

        let absolute_path = PathBuf::from("C:/absolute/path");
        assert_eq!(absolute_unchecked(&absolute_path).unwrap(), absolute_path);
        let relative = absolute_unchecked(&PathBuf::from("relative/path")).unwrap();
        assert!(relative.is_absolute());
        assert!(relative.ends_with("relative/path"));

        let args = vec!["--flag".to_string(), "value".to_string()];
        assert_eq!(value(&args, 1, "--flag").unwrap(), "value");
        let err = value(&args, 9, "--flag").unwrap_err().to_string();
        assert!(err.contains("--flag requires a value"), "{err}");

        assert_eq!(
            path_text(&PathBuf::from(r"C:\dir\file.txt")),
            "C:/dir/file.txt"
        );
        assert_eq!(toml_string("plain"), "\"plain\"");
        assert_eq!(toml_string(r"back\slash"), r#""back\\slash""#);
        assert_eq!(toml_string("say \"hi\""), r#""say \"hi\"""#);
    }

    #[test]
    fn run_slug_sanitizes_directory_names() {
        assert_eq!(
            run_slug(&PathBuf::from("C:/runs/comparison-01")),
            "comparison-01"
        );
        assert_eq!(run_slug(&PathBuf::from("a b")), "a-b");
        assert_eq!(run_slug(&PathBuf::from("naïve")), "na-ve");
        assert_eq!(run_slug(&PathBuf::from("")), "comparison");
        // A directory of nothing but punctuation is kept (it is already slug-safe).
        assert_eq!(run_slug(&PathBuf::from("---")), "---");
        // Only the final path component matters.
        assert_eq!(run_slug(&PathBuf::from("C:/runs/c.d_e")), "c.d_e");
    }
    fn temp_layout(name: &str) -> (tempfile::TempDir, HarnessLayout) {
        let temp = tempfile::tempdir().unwrap();
        let layout = HarnessLayout {
            worktree: temp.path().join("worktree"),
            target_dir: temp.path().join("target"),
            artifacts: temp.path().join("artifacts"),
        };
        fs::create_dir_all(&layout.worktree).unwrap();
        // Ensure worktree/target/artifacts each exist so the layout is a realistic tree.
        fs::create_dir_all(&layout.target_dir).unwrap();
        fs::create_dir_all(&layout.artifacts).unwrap();
        let _ = name;
        (temp, layout)
    }

    fn commit_tiny_repo(repository: &Path) -> String {
        fs::create_dir_all(repository.join("src")).unwrap();
        fs::write(
            repository.join("Cargo.toml"),
            "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(repository.join("src/lib.rs"), "").unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(repository)
                .args(["init"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(repository)
                .args(["add", "."])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(repository)
                .args([
                    "-c",
                    "user.name=Liberado Test",
                    "-c",
                    "user.email=liberado@example.invalid",
                    "commit",
                    "-m",
                    "base",
                ])
                .status()
                .unwrap()
                .success()
        );
        let out = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn manifest_with_worktree() -> (tempfile::TempDir, CompareManifest, String) {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        let base_commit = commit_tiny_repo(&repository);
        let worktree = temp.path().join("worktree");
        let base = base_commit.clone();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["worktree", "add", "--detach"])
                .arg(&worktree)
                .arg(&base)
                .status()
                .unwrap()
                .success()
        );
        let mut harnesses = BTreeMap::new();
        harnesses.insert(
            "liberado".to_string(),
            HarnessLayout {
                worktree: worktree.clone(),
                target_dir: temp.path().join("targets/liberado"),
                artifacts: temp.path().join("artifacts/liberado"),
            },
        );
        let manifest = CompareManifest {
            version: 1,
            source_root: repository,
            run_root: temp.path().to_path_buf(),
            base_revision: "main".to_string(),
            base_commit: base_commit.clone(),
            compile_timeout_secs: 1_800,
            harnesses,
        };
        (temp, manifest, base_commit)
    }

    #[test]
    fn save_result_records_clean_and_dirty_worktrees() {
        // Clean worktree: no add/commit; head and branch recorded; no status-before.
        let (_temp, manifest, base) = manifest_with_worktree();
        save_result(&manifest, "liberado", Some("sess"), Some(0), Some(0)).unwrap();
        let artifacts = manifest.harnesses["liberado"].artifacts.clone();
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(artifacts.join("result.json")).unwrap()).unwrap();
        assert_eq!(json["harness"], "liberado");
        assert_eq!(json["had_uncommitted_changes"], false);
        assert_eq!(json["exit_code"], 0);
        let status_before =
            fs::read_to_string(artifacts.join("git/status-before-save.txt")).unwrap();
        assert!(status_before.trim().is_empty());

        // Dirty worktree: an uncommitted file is preserved via add+commit and marked.
        let layout = &manifest.harnesses["liberado"];
        fs::write(layout.worktree.join("new.txt"), "dirty\n").unwrap();
        save_result(&manifest, "liberado", None, Some(1), Some(0)).unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(artifacts.join("result.json")).unwrap()).unwrap();
        assert_eq!(json["had_uncommitted_changes"], true);
        assert_eq!(json["exit_code"], 1);
        // Preserving the dirty state created a wip commit, so the head moved past the base and
        // the log names the preserve commit.
        let head = json["head_commit"].as_str().unwrap();
        assert_ne!(head, &base);
        let log = fs::read_to_string(artifacts.join("git/log.txt")).unwrap();
        assert!(log.contains("wip(compare)"), "{log}");
        assert_eq!(json["head_commit"].as_str().unwrap().len(), 40);
        // The head recorded is a real commit owned by the harness worktree.
        let head = json["head_commit"].as_str().unwrap();
        assert_eq!(head.len(), 40);
        // A branch was created pointing at the head.
        assert!(json["archive_branch"].as_str().unwrap().len() > 20);
        // Artifact git metadata was written.
        assert!(artifacts.join("git/diff.patch").is_file());
        assert!(artifacts.join("git/log.txt").is_file());
    }

    #[test]
    fn copy_traces_copies_only_matching_sessions() {
        let (_temp, layout) = temp_layout("noop");
        fs::create_dir_all(layout.worktree.join("coder-traces")).unwrap();
        fs::write(
            layout.worktree.join("coder-traces/run-liberado.json"),
            "[1]",
        )
        .unwrap();
        fs::write(layout.worktree.join("coder-traces/other.json"), "[2]").unwrap();
        fs::write(
            layout.worktree.join("coder-traces/ignore.txt"),
            "not a session",
        )
        .unwrap();
        // No session id: every file is copied.
        copy_traces(&layout, None).unwrap();
        assert!(layout.artifacts.join("traces/run-liberado.json").is_file());
        assert!(layout.artifacts.join("traces/other.json").is_file());
        // With a prefix, only that session's trace (and only .json, not .txt) is copied.
        fs::remove_dir_all(layout.artifacts.join("traces")).unwrap();
        copy_traces(&layout, Some("run-liberado")).unwrap();
        assert!(layout.artifacts.join("traces/run-liberado.json").is_file());
        assert!(!layout.artifacts.join("traces/other.json").exists());
        assert!(!layout.artifacts.join("traces/ignore.txt").exists());
        // Missing coder-traces dir is a no-op.
        copy_traces(&layout, None).unwrap();
    }

    #[test]
    fn git_capture_and_git_status_report_failures() {
        let temp = tempfile::tempdir().unwrap();
        let err = git_capture(temp.path(), &["rev-parse", "--definitely-not-a-ref"]).unwrap_err();
        assert!(err.to_string().contains("git rev-parse"), "{err}");
        let err = git_status(temp.path(), &["this-is-not-a-subcommand"]).unwrap_err();
        assert!(
            err.to_string().contains("git this-is-not-a-subcommand"),
            "{err}"
        );
    }

    fn sleeping_command() -> Command {
        #[cfg(windows)]
        {
            let mut cmd = Command::new("cmd");
            cmd.args(["/c", "ping -n 30 127.0.0.1 >nul"]);
            cmd
        }
        #[cfg(not(windows))]
        {
            Command::new("sleep").arg("30")
        }
    }

    #[test]
    fn execute_logged_kills_on_wall_clock_timeout() {
        let (_temp, layout) = temp_layout("noop");
        let mut cmd = sleeping_command();
        let err = execute_logged(&mut cmd, &layout, "sleep", 1, None).unwrap_err();
        assert!(err.to_string().contains("wall-clock limit"), "{err}");
        assert!(layout.artifacts.join("sleep.stdout.log").is_file());
    }

    #[cfg(windows)]
    #[test]
    fn run_or_record_launch_error_writes_the_artifact() {
        let (temp, manifest) = compare_manifest();
        fs::create_dir_all(manifest.harnesses["liberado"].artifacts.clone()).unwrap();
        let launch = || -> Result<i32, Box<dyn Error>> { Err("adapter refused to start".into()) };
        let exit = run_or_record_launch_error(&manifest, "liberado", launch);
        assert_eq!(exit, 127);
        let artifact = fs::read_to_string(
            manifest.harnesses["liberado"]
                .artifacts
                .join("launch-error.txt"),
        )
        .unwrap();
        assert!(artifact.contains("adapter refused to start"), "{artifact}");
        let _ = temp;
    }
}
