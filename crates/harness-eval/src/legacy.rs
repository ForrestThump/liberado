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
struct RunArgs {
    run_root: PathBuf,
    task: PathBuf,
    model: String,
    provider: String,
    base_url: String,
    api_key_env: String,
    thinking: String,
    max_turns: u32,
    run_timeout_secs: u64,
    verifier_repair_attempts: u32,
    task_aware_context: bool,
    acceptance_overlay: Option<PathBuf>,
    liberado_bin: Option<PathBuf>,
    pi_bin: Option<PathBuf>,
    cancel_file: Option<PathBuf>,
}

pub fn prepare(args: &[String]) -> Result<(), Box<dyn Error>> {
    if args == ["-h"] || args == ["--help"] {
        println!(
            "usage: liberado coder compare prepare <run-dir> [--source <repo>] [--commit <ref>] \
             [--compile-timeout-secs <n>]"
        );
        return Ok(());
    }

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

    let source_root = child_process_path(&match source_root {
        Some(path) => absolute(&path)?,
        None => absolute(&crate::repository_root()?)?,
    });
    let run_root = child_process_path(&absolute_unchecked(
        &run_root.ok_or("usage: liberado coder compare prepare <run-dir> [--commit <ref>]")?,
    )?);
    if run_root.exists() {
        return Err(format!(
            "comparison run directory already exists: {}",
            run_root.display()
        )
        .into());
    }
    let base_commit = git_capture(
        &source_root,
        &["rev-parse", &format!("{revision}^{{commit}}")],
    )?
    .trim()
    .to_string();

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

    let result = prepare_worktrees(&source_root, &base_commit, &harnesses);
    if let Err(error) = result {
        cleanup_prepared_worktrees(&source_root, &harnesses);
        let _ = fs::remove_dir_all(&run_root);
        return Err(error);
    }

    let manifest = CompareManifest {
        version: MANIFEST_VERSION,
        source_root,
        run_root: run_root.clone(),
        base_revision: revision,
        base_commit,
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

pub(crate) fn run_with_credential(
    args: &[String],
    credential: ResolvedCredential,
) -> Result<(), Box<dyn Error>> {
    run_parsed(parse_run_args(args)?, credential)
}

fn run_parsed(parsed: RunArgs, credential: ResolvedCredential) -> Result<(), Box<dyn Error>> {
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
    // Both adapters must be ready before the first paid model request.
    let liberado_preflight = liberado.preflight().map_err(|error| error.to_string());
    let pi_preflight = pi.preflight().map_err(|error| error.to_string());
    let (liberado_exit, liberado_verifier_exit) = run_with_verifier_repairs(
        &manifest,
        &parsed,
        "liberado",
        || match &liberado_preflight {
            Ok(_) => liberado.launch().map(|result| result.exit_code),
            Err(error) => Err(error.clone().into()),
        },
        |prompt, stem| {
            run_liberado(
                &manifest,
                &parsed,
                prompt,
                &liberado_session,
                &credential,
                stem,
            )
        },
        acceptance_overlay.as_deref(),
    );
    save_result(
        &manifest,
        "liberado",
        Some(&liberado_session),
        Some(liberado_exit),
        Some(liberado_verifier_exit),
    )?;

    let (pi_exit, pi_verifier_exit) = run_with_verifier_repairs(
        &manifest,
        &parsed,
        "pi",
        || match &pi_preflight {
            Ok(_) => pi.launch().map(|result| result.exit_code),
            Err(error) => Err(error.clone().into()),
        },
        |prompt, stem| {
            let path = manifest
                .run_root
                .join("artifacts")
                .join("pi")
                .join(format!("{stem}.prompt.txt"));
            fs::write(&path, prompt)?;
            let prompt_arg = format!("@{}", path.display());
            run_pi(
                &manifest,
                &parsed,
                &prompt_arg,
                &pi_session,
                &credential,
                stem,
            )
        },
        acceptance_overlay.as_deref(),
    );
    save_result(
        &manifest,
        "pi",
        Some(&pi_session),
        Some(pi_exit),
        Some(pi_verifier_exit),
    )?;

    println!("liberado exit: {liberado_exit}");
    println!("liberado verifier exit: {liberado_verifier_exit}");
    println!("pi exit: {pi_exit}");
    println!("pi verifier exit: {pi_verifier_exit}");
    println!(
        "artifacts: {}",
        manifest.run_root.join("artifacts").display()
    );
    if [
        liberado_exit,
        liberado_verifier_exit,
        pi_exit,
        pi_verifier_exit,
    ]
    .iter()
    .any(|code| *code != 0)
    {
        return Err(
            "one or more harnesses or common verifiers failed; work and artifacts were saved"
                .into(),
        );
    }
    Ok(())
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
    let mut run_root = None;
    let mut task = None;
    let mut model = DEFAULT_MODEL.to_string();
    let mut provider = DEFAULT_PROVIDER.to_string();
    let mut base_url = DEFAULT_BASE_URL.to_string();
    let mut api_key_env = DEFAULT_API_KEY_ENV.to_string();
    let mut thinking = DEFAULT_THINKING.to_string();
    let mut max_turns = DEFAULT_MAX_TURNS;
    let mut run_timeout_secs = DEFAULT_RUN_TIMEOUT_SECS;
    // Keep external verifier repair off for benchmark runs. Operators can opt in explicitly;
    // otherwise the comparison would measure the coordinator's recovery policy, not the harness.
    let mut verifier_repair_attempts = 0;
    let mut task_aware_context = false;
    let mut acceptance_overlay = None;
    let mut liberado_bin = None;
    let mut pi_bin = None;
    let mut cancel_file = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        match flag {
            "--task" => {
                index += 1;
                task = Some(PathBuf::from(value(args, index, flag)?));
            }
            "--model" => {
                index += 1;
                model = value(args, index, flag)?.to_string();
            }
            "--provider" => {
                index += 1;
                provider = value(args, index, flag)?.to_string();
            }
            "--base-url" => {
                index += 1;
                base_url = value(args, index, flag)?.to_string();
            }
            "--api-key-env" => {
                index += 1;
                api_key_env = value(args, index, flag)?.to_string();
            }
            "--thinking" => {
                index += 1;
                thinking = value(args, index, flag)?.to_string();
            }
            "--max-turns" => {
                index += 1;
                max_turns = value(args, index, flag)?
                    .parse()
                    .map_err(|_| "--max-turns must be a positive integer")?;
                if max_turns == 0 {
                    return Err("--max-turns must be a positive integer".into());
                }
            }
            "--run-timeout-secs" => {
                index += 1;
                run_timeout_secs = value(args, index, flag)?
                    .parse()
                    .map_err(|_| "--run-timeout-secs must be a positive integer")?;
                if run_timeout_secs == 0 {
                    return Err("--run-timeout-secs must be a positive integer".into());
                }
            }
            "--verifier-repair-attempts" => {
                index += 1;
                verifier_repair_attempts = value(args, index, flag)?
                    .parse()
                    .map_err(|_| "--verifier-repair-attempts must be a non-negative integer")?;
            }
            "--task-aware-context" => {
                task_aware_context = true;
            }
            "--acceptance-overlay" => {
                index += 1;
                acceptance_overlay = Some(absolute(&PathBuf::from(value(args, index, flag)?))?);
            }
            "--liberado-bin" => {
                index += 1;
                liberado_bin = Some(PathBuf::from(value(args, index, flag)?));
            }
            "--pi-bin" => {
                index += 1;
                pi_bin = Some(PathBuf::from(value(args, index, flag)?));
            }
            "--cancel-file" => {
                index += 1;
                cancel_file = Some(PathBuf::from(value(args, index, flag)?));
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown flag for coder compare run: {flag}").into());
            }
            path => {
                if run_root.is_some() {
                    return Err("coder compare run takes one run directory".into());
                }
                run_root = Some(PathBuf::from(path));
            }
        }
        index += 1;
    }
    Ok(RunArgs {
        run_root: absolute(&run_root.ok_or("coder compare run requires <run-dir>")?)?,
        task: absolute(&task.ok_or("coder compare run requires --task <file>")?)?,
        model,
        provider,
        base_url,
        api_key_env,
        thinking,
        max_turns,
        run_timeout_secs,
        verifier_repair_attempts,
        task_aware_context,
        acceptance_overlay,
        liberado_bin,
        pi_bin,
        cancel_file,
    })
}

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
            "[coder]\ntrace_dir = \"coder-traces\"\noffered_tools = [\"read_file\", \"write_file\", \"edit_file\", \"run_command\"]\n\n\
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
            "base_revision={}\nbase_commit={}\nprovider={}\nmodel={}\nthinking={}\nliberado_max_turns={}\npi_turn_cap=client default\ncompile_timeout_secs={}\nverifier_repair_attempts={}\ntask_aware_context={}\nacceptance_overlay_hash={}\nsampling=temperature omitted by both clients\n",
            manifest.base_revision,
            manifest.base_commit,
            args.provider,
            args.model,
            args.thinking,
            args.max_turns,
            manifest.compile_timeout_secs,
            args.verifier_repair_attempts,
            args.task_aware_context,
            overlay_hash,
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

    copy_traces(layout, session_id)?;
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
    if slug.is_empty() {
        "comparison".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HarnessLayout, bounded_feedback, copy_path_dependency_tree, liberado_runner_path,
        repairable_verifier_exit, run_async_command,
    };
    #[cfg(windows)]
    use super::{prepare, remove_job_worktrees};
    use liberado_common::process::command;
    use std::fs;
    use std::path::PathBuf;
    #[cfg(windows)]
    use std::process::Command;
    use std::time::Duration;

    fn layout() -> HarnessLayout {
        HarnessLayout {
            worktree: PathBuf::from("C:/comparison/worktree"),
            target_dir: PathBuf::from("C:/comparison/targets/liberado"),
            artifacts: PathBuf::from("C:/comparison/artifacts/liberado"),
        }
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
}
