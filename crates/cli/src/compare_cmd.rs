//! Durable, repeatable cross-harness comparison runs.
//!
//! The comparison owns its worktrees, build caches, logs, sessions, traces, and saved Git refs.
//! This keeps orchestration policy in compiled code. Shell wrappers only need to pass arguments.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::Utc;
use liberado_coder_core::DispatchWriteScope;
use liberado_common::process::{command, output_within, std_command};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MANIFEST_VERSION: u32 = 1;
const DEFAULT_COMPILE_TIMEOUT_SECS: u64 = 1_800;
const DEFAULT_MODEL: &str = "deepseek/deepseek-v4-flash";
const DEFAULT_PROVIDER: &str = "openrouter";
const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
const DEFAULT_API_KEY_ENV: &str = "OPENROUTER_API_KEY";
const DEFAULT_THINKING: &str = "high";
const DEFAULT_MAX_TURNS: u32 = 400;

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
    task_aware_context: bool,
    write_scope: DispatchWriteScope,
    acceptance_overlay: Option<PathBuf>,
    liberado_bin: Option<PathBuf>,
    pi_bin: Option<PathBuf>,
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

    let source_root = match source_root {
        Some(path) => absolute(&path)?,
        None => absolute(&crate::crate_map_cmd::repository_root()?)?,
    };
    let run_root = absolute_unchecked(
        &run_root.ok_or("usage: liberado coder compare prepare <run-dir> [--commit <ref>]")?,
    )?;
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
             [--thinking <level>] [--max-turns <n>] [--task-aware-context] \
             [--allow-change <path-or-prefix>] [--deny-change <path-or-prefix>] \
             [--acceptance-overlay <dir>] \
             [--liberado-bin <path>] [--pi-bin <path>]"
        );
        return Ok(());
    }
    let parsed = parse_run_args(args)?;
    let manifest = load_manifest(&parsed.run_root)?;
    let task = fs::read_to_string(&parsed.task)?;
    if task.trim().is_empty() {
        return Err("comparison task file is empty".into());
    }
    fs::write(manifest.run_root.join("task.txt"), &task)?;
    let acceptance_overlay = capture_acceptance_overlay(&manifest, &parsed)?;
    write_run_config(&manifest, &parsed)?;
    write_run_pins(&manifest, &parsed, acceptance_overlay.as_deref())?;

    if std::env::var_os(&parsed.api_key_env).is_none() {
        return Err(format!("{} is not set in this process", parsed.api_key_env).into());
    }

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
    let liberado_exit = run_or_record_launch_error(&manifest, "liberado", || {
        run_liberado(&manifest, &parsed, &task, &liberado_session)
    });
    let liberado_verifier_exit = verify_harness(
        &manifest,
        "liberado",
        &parsed.write_scope,
        acceptance_overlay.as_deref(),
    );
    save_result(
        &manifest,
        "liberado",
        Some(&liberado_session),
        Some(liberado_exit),
        Some(liberado_verifier_exit),
    )?;

    let pi_exit = run_or_record_launch_error(&manifest, "pi", || {
        run_pi(&manifest, &parsed, &task, &pi_session)
    });
    let pi_verifier_exit = verify_harness(
        &manifest,
        "pi",
        &parsed.write_scope,
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
    let mut task_aware_context = false;
    let mut write_scope = DispatchWriteScope::default();
    let mut acceptance_overlay = None;
    let mut liberado_bin = None;
    let mut pi_bin = None;
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
            "--task-aware-context" => {
                task_aware_context = true;
            }
            "--allow-change" => {
                index += 1;
                write_scope
                    .allow_globs
                    .push(change_scope_pattern(value(args, index, flag)?)?);
            }
            "--deny-change" => {
                index += 1;
                write_scope
                    .deny_globs
                    .push(change_scope_pattern(value(args, index, flag)?)?);
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
        task_aware_context,
        write_scope,
        acceptance_overlay,
        liberado_bin,
        pi_bin,
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
            copy_tree(&source, &layout.worktree.join(sibling))?;
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
    let write_scope = if args.write_scope.is_active() {
        format!(
            "\n[coder.path_policy.write_scope]\nallow_globs = {}\ndeny_globs = {}\n",
            toml_string_array(&args.write_scope.allow_globs),
            toml_string_array(&args.write_scope.deny_globs),
        )
    } else {
        String::new()
    };
    fs::write(
        config.join("tuning.toml"),
        format!(
            "[coder]\ntrace_dir = \"coder-traces\"\noffered_tools = [\"read_file\", \"write_file\", \"edit_file\", \"run_command\"]\n\n\
             [coder.coder]\nmodel = {}\nmax_turns = {}\nreasoning = {}\n\n\
             [coder.command_policy]\ntimeout_secs = {}\noutput_max_bytes = 65536\ndeny = [\"git\"]\n\n\
             [coder.workspace]\nshared_target_dir = {}\nwarmup = false\nwarmup_timeout_secs = {}\n{}{}",
            toml_string(&args.model),
            args.max_turns,
            toml_string(&args.thinking),
            manifest.compile_timeout_secs,
            toml_string(&path_text(&liberado.target_dir)),
            manifest.compile_timeout_secs,
            repo_map,
            write_scope,
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
            "base_revision={}\nbase_commit={}\nprovider={}\nmodel={}\nthinking={}\nliberado_max_turns={}\npi_turn_cap=client default\ncompile_timeout_secs={}\ntask_aware_context={}\nwrite_scope_allow={}\nwrite_scope_deny={}\nacceptance_overlay_hash={}\nsampling=temperature omitted by both clients\n",
            manifest.base_revision,
            manifest.base_commit,
            args.provider,
            args.model,
            args.thinking,
            args.max_turns,
            manifest.compile_timeout_secs,
            args.task_aware_context,
            args.write_scope.allow_globs.join(","),
            args.write_scope.deny_globs.join(","),
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
    let output = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(output_within(
            &mut cmd,
            "cargo check --workspace --locked",
            Duration::from_secs(manifest.compile_timeout_secs),
        ))
    })?;
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
    write_scope: &DispatchWriteScope,
    acceptance_overlay: Option<&Path>,
) -> i32 {
    let layout = match harness(manifest, name) {
        Ok(layout) => layout,
        Err(error) => {
            eprintln!("{name} verifier setup failed: {error}");
            return 125;
        }
    };
    if let Err(error) = verify_changed_paths(manifest, &layout.worktree, write_scope) {
        let message = format!("{name} change-scope verification failed: {error}\n");
        eprint!("{message}");
        let _ = fs::write(layout.artifacts.join("verifier.stdout.log"), b"");
        let _ = fs::write(layout.artifacts.join("verifier.stderr.log"), &message);
        let now = Utc::now();
        let _ = fs::write(
            layout.artifacts.join("verifier-status.txt"),
            format!(
                "started={}\nfinished={}\nexit=126\n",
                now.to_rfc3339(),
                now.to_rfc3339()
            ),
        );
        return 126;
    }
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
    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(output_within(
            &mut cmd,
            "cargo test --workspace --no-fail-fast",
            Duration::from_secs(manifest.compile_timeout_secs),
        ))
    });
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

fn verify_changed_paths(
    manifest: &CompareManifest,
    worktree: &Path,
    write_scope: &DispatchWriteScope,
) -> Result<(), Box<dyn Error>> {
    if !write_scope.is_active() {
        return Ok(());
    }
    let mut changed = BTreeSet::new();
    for arguments in [
        vec![
            "diff",
            "--name-only",
            "--no-renames",
            manifest.base_commit.as_str(),
        ],
        vec!["ls-files", "--others", "--exclude-standard"],
    ] {
        for path in git_capture(worktree, &arguments)?
            .lines()
            .filter(|path| !path.is_empty())
        {
            changed.insert(path.replace('\\', "/"));
        }
    }
    let rejected: Vec<_> = changed
        .iter()
        .filter(|path| !write_scope.permits(path))
        .cloned()
        .collect();
    if rejected.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "changed path(s) outside the dispatch write scope: {}",
            rejected.join(", ")
        )
        .into())
    }
}

fn run_liberado(
    manifest: &CompareManifest,
    args: &RunArgs,
    task: &str,
    session_id: &str,
) -> Result<i32, Box<dyn Error>> {
    let layout = harness(manifest, "liberado")?;
    let binary = args.liberado_bin.clone().unwrap_or_else(|| {
        manifest
            .source_root
            .join("target")
            .join("debug")
            .join(if cfg!(windows) {
                "liberado-coder-run.exe"
            } else {
                "liberado-coder-run"
            })
    });
    if !binary.is_file() {
        return Err(format!("Liberado runner does not exist: {}", binary.display()).into());
    }
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
        .env("LIBERADO_CODER_PROVIDER", &args.provider);
    execute_logged(&mut cmd, layout, "session")
}

fn run_pi(
    manifest: &CompareManifest,
    args: &RunArgs,
    _task: &str,
    session_id: &str,
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
        .arg(format!("@{}", manifest.run_root.join("task.txt").display()))
        .current_dir(&layout.worktree)
        .env("CARGO_TARGET_DIR", &layout.target_dir);
    execute_logged(&mut cmd, layout, "session")
}

fn execute_logged(
    command: &mut Command,
    layout: &HarnessLayout,
    stem: &str,
) -> Result<i32, Box<dyn Error>> {
    let stdout_path = layout.artifacts.join(format!("{stem}.stdout.log"));
    let stderr_path = layout.artifacts.join(format!("{stem}.stderr.log"));
    command
        .stdout(Stdio::from(File::create(&stdout_path)?))
        .stderr(Stdio::from(File::create(&stderr_path)?));
    let started = Utc::now();
    let status = command.status()?;
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

fn toml_string_array(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| toml_string(value))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn change_scope_pattern(value: &str) -> Result<String, Box<dyn Error>> {
    let normalized = value.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.split('/').any(|component| component == "..")
    {
        return Err(format!(
            "change-scope path must be a non-empty workspace-relative path or prefix: {value}"
        )
        .into());
    }
    Ok(normalized)
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
