//! Split from `legacy.rs` for module-health boundaries.

use super::external::deepagents_model;
use super::parse_prepare_args;
#[cfg(windows)]
use super::run_or_record_launch_error;
use super::{
    CompareManifest, DEFAULT_API_KEY_ENV, DEFAULT_BASE_URL, DEFAULT_MAX_TURNS, DEFAULT_MODEL,
    DEFAULT_PROVIDER, DEFAULT_RUN_TIMEOUT_SECS, DEFAULT_THINKING, DeepAgentsAdapter, HarnessLayout,
    HermesAdapter, RunArgs, SAMPLING_OMITTED, absolute, absolute_unchecked, bounded_feedback,
    capture_acceptance_overlay, copy_path_dependency_tree, copy_traces, copy_tree,
    default_run_order, ensure_install_target_is_safe, execute_logged, git_capture, git_status,
    git_worktree_add, liberado_runner_path, overlay_files, overlay_fingerprint, parse_run_args,
    path_text, repairable_verifier_exit, run_args_from_spec, run_async_command, run_slug,
    save_result, toml_string, value, verifier_feedback, write_run_config, write_run_pins,
};
#[cfg(windows)]
use super::{prepare, remove_job_worktrees};
use crate::adapter::HarnessAdapter;
use crate::contract::{
    AcceptanceBundle, HarnessRequest, JOB_SPEC_VERSION, JobId, JobSpec, ModelPins, ResourceLimits,
    TaskBundle, VerifierProfile,
};
use crate::preflight::ResolvedCredential;
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
        hermes_bin: None,
        deep_agents_bin: None,
        hermes_git_sha: None,
        deep_agents_git_sha: None,
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
    assert!(!pins.contains("hermes_turn_cap"));
    assert!(!pins.contains("deepagents_turn_cap"));
}

#[test]
fn four_way_pins_record_native_turn_caps_not_liberado_400() {
    let (_temp, manifest) = compare_manifest();
    let mut args = run_args();
    args.run_order = crate::contract::default_four_way_run_order();
    args.hermes_bin = Some(PathBuf::from("/opt/hermes"));
    args.deep_agents_bin = Some(PathBuf::from("/opt/dcode"));
    args.hermes_git_sha = Some("aaa111".into());
    args.deep_agents_git_sha = Some("bbb222".into());
    write_run_pins(&manifest, &args, None).unwrap();
    let pins = fs::read_to_string(manifest.run_root.join("pins.txt")).unwrap();
    assert!(pins.contains("hermes_turn_cap=unset"));
    assert!(pins.contains("deepagents_turn_cap=unset"));
    assert!(pins.contains("hermes_git_sha=aaa111"));
    assert!(pins.contains("deepagents_git_sha=bbb222"));
    assert!(!pins.contains("hermes_turn_cap=400"));
    assert!(!pins.contains("deepagents_turn_cap=400"));
}

#[test]
fn hermes_and_deepagents_preflight_fail_when_binary_is_missing() {
    let (_temp, manifest) = compare_manifest();
    let mut args = run_args();
    args.hermes_bin = Some(PathBuf::from("/definitely-missing-hermes"));
    args.deep_agents_bin = Some(PathBuf::from("/definitely-missing-dcode"));
    let hermes = HermesAdapter {
        manifest: &manifest,
        args: &args,
        session_id: "s-hermes".into(),
        credential: &ResolvedCredential::new("secret".into()),
    };
    let err = hermes.preflight().unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");
    let deep = DeepAgentsAdapter {
        manifest: &manifest,
        args: &args,
        session_id: "s-deep".into(),
        credential: &ResolvedCredential::new("secret".into()),
    };
    let err = deep.preflight().unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");
}

#[test]
fn known_external_ids_are_accepted_by_preflight_when_binary_exists() {
    let temp = tempfile::tempdir().unwrap();
    let hermes_bin = temp.path().join("hermes");
    let dcode_bin = temp.path().join("dcode");
    fs::write(&hermes_bin, "#!/bin/sh\n").unwrap();
    fs::write(&dcode_bin, "#!/bin/sh\n").unwrap();
    let (_temp, manifest) = compare_manifest();
    let mut args = run_args();
    args.hermes_bin = Some(hermes_bin.clone());
    args.deep_agents_bin = Some(dcode_bin.clone());
    let credential = ResolvedCredential::new("secret".into());
    let hermes = HermesAdapter {
        manifest: &manifest,
        args: &args,
        session_id: "s-hermes".into(),
        credential: &credential,
    };
    let report = hermes.preflight().unwrap();
    assert_eq!(report.harness, "hermes");
    assert!(report.executable.contains("hermes"));
    let deep = DeepAgentsAdapter {
        manifest: &manifest,
        args: &args,
        session_id: "s-deep".into(),
        credential: &credential,
    };
    let report = deep.preflight().unwrap();
    assert_eq!(report.harness, "deepagents");
    assert!(report.executable.contains("dcode"));
}

#[test]
fn deepagents_model_prefixes_provider_when_the_model_has_no_colon() {
    assert_eq!(
        deepagents_model("openrouter", "deepseek/deepseek-v4-flash"),
        "openrouter:deepseek/deepseek-v4-flash"
    );
    assert_eq!(
        deepagents_model("openrouter", "openrouter:deepseek/deepseek-v4-flash"),
        "openrouter:deepseek/deepseek-v4-flash"
    );
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
                git_sha: None,
            },
            HarnessRequest {
                id: "pi".to_string(),
                binary: Some(PathBuf::from("pi.exe")),
                git_sha: None,
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
    assert!(args.hermes_bin.is_none());
    assert!(args.deep_agents_bin.is_none());
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
        "--hermes-bin".to_string(),
        "hermes.exe".to_string(),
        "--deep-agents-bin".to_string(),
        "dcode.exe".to_string(),
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
    assert_eq!(args.hermes_bin, Some(PathBuf::from("hermes.exe")));
    assert_eq!(args.deep_agents_bin, Some(PathBuf::from("dcode.exe")));
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
    let err =
        parse_run_args(&["--task".to_string(), task.to_string_lossy().into_owned()]).unwrap_err();
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

    let absolute_path = if cfg!(windows) {
        PathBuf::from("C:/absolute/path")
    } else {
        PathBuf::from("/absolute/path")
    };
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
    let status_before = fs::read_to_string(artifacts.join("git/status-before-save.txt")).unwrap();
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
    // cfg! so both OS arms type-check on every runner.
    let mut cmd = if cfg!(windows) {
        Command::new("cmd")
    } else {
        Command::new("sleep")
    };
    if cfg!(windows) {
        cmd.args(["/c", "ping -n 30 127.0.0.1 >nul"]);
    } else {
        cmd.arg("30");
    }
    cmd
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
