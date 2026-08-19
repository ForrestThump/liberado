//! `liberado coder …` — harness observability over durable coding traces.
//!
//! Thin adapter: resolve path → call pure functions in `liberado_coder_core::trace_view` → print.
//! Domain logic stays in the pack contract crate so unit tests drive the same path the binary uses.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use liberado_coder_core::{
    ForeignTraceFormat, compare_traces, diverge, format_comparison, format_divergence,
    import_foreign_file, load_run_view, load_trace, render_transcript, resolve_trace_path,
    write_messages_export,
};
use liberado_common::process::std_command;
use serde_json::json;

/// Default directories searched when resolving a session id (cwd-relative + common local path).
fn default_trace_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("coder-traces")];
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("coder-traces"));
    }
    dirs
}

pub fn run(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    match args.next().as_deref() {
        Some("trace") => cmd_trace(&mut args),
        Some("compare") => cmd_compare(&mut args),
        Some("diff") => cmd_diff(&mut args),
        Some("import") => cmd_import(&mut args),
        Some("summarize") => crate::summarize_cmd::run(args),
        Some("smoke") => cmd_smoke(&mut args),
        Some(other) => Err(format!("unknown coder subcommand '{other}'\n{}", usage()).into()),
        None => Err(usage().into()),
    }
}

fn usage() -> &'static str {
    "usage:\n  \
     liberado coder trace <session-id|path> [--dir <trace-dir>] [--path <file>]\n  \
     liberado coder compare <trace-a> <trace-b> [--dir <trace-dir>] [--json]\n  \
     liberado coder compare prepare <run-dir> [--commit <ref>]   create pinned worktrees\n  \
     liberado coder compare run <run-dir> --task <file>          run and preserve both harnesses\n  \
     liberado coder compare save <run-dir> <liberado|pi>         preserve one result\n  \
     liberado coder compare submit --task <file>                 submit and dispatch a comparison job\n  \
     liberado coder compare doctor --task <file>                 check prerequisites without running\n  \
     liberado coder compare status|await|cancel|report <job-id>  inspect a comparison job\n  \
     liberado coder compare reset <workspace> [--commit <ref>]   restore tracked files\n  \
     liberado coder diff <run-a> <run-b> [--json]   cross-harness: where two runs parted\n  \
     liberado coder import <foreign.json> [-o <out.messages.json>] [--format kilo|kilo-cli|openhands|auto] [--session-id <id>]
  liberado coder smoke              validate the coder runner process boundary"
}

/// Handle the `-h`/`--help`/unknown-arg cases of `coder smoke`.
fn smoke_arg_check(
    args: &mut dyn Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(arg) = args.next() {
        if arg == "-h" || arg == "--help" {
            println!("usage: liberado coder smoke");
            return Ok(());
        }
        return Err("usage: liberado coder smoke".into());
    }
    Ok(())
}

fn cmd_smoke(args: &mut dyn Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    smoke_arg_check(args)?;

    let root = crate::crate_map_cmd::repository_root()?;
    println!("== building liberado-coder-runner ==");
    let build = std_command("cargo")
        .args(["build", "--locked", "-p", "liberado-coder-runner"])
        .current_dir(&root)
        .status()?;
    if !build.success() {
        return Err("liberado-coder-runner build failed".into());
    }

    let runner = root.join("target").join("debug").join(if cfg!(windows) {
        "liberado-coder-run.exe"
    } else {
        "liberado-coder-run"
    });
    if !runner.is_file() {
        return Err(format!(
            "liberado-coder-runner binary not found: {}",
            runner.display()
        )
        .into());
    }
    println!("binary: {}", runner.display());

    let temp = tempfile::tempdir()?;
    initialize_smoke_repository(temp.path())?;
    let request_path = temp.path().join("request.json");
    fs::write(
        &request_path,
        serde_json::to_vec_pretty(&smoke_request(temp.path()))?,
    )?;

    println!("== process boundary smoke (expects provider key or clean failure) ==");
    let provider =
        std::env::var("LIBERADO_CODER_PROVIDER").unwrap_or_else(|_| "openrouter".to_owned());
    let output = std_command(&runner)
        .args([
            "--request",
            request_path.to_str().ok_or("request path is not UTF-8")?,
        ])
        .env("LIBERADO_CODER_PROVIDER", provider)
        .current_dir(&root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        println!("OK: live provider completed a coding run");
        return Ok(());
    }

    if smoke_boundary_reached(&stdout, &stderr) {
        println!("OK: runner reached the provider boundary without credentials");
        println!("  exit status: {}", output.status);
        return Ok(());
    }

    let combined = format!("{stdout}\n{stderr}")
        .to_lowercase()
        .trim()
        .to_string();
    Err(format!("coder smoke failed with {}:\n{}", output.status, combined).into())
}

/// Classify a failed runner invocation: did it reach the provider boundary without
/// credentials (a pass for smoke), or die earlier (a real failure)?
fn smoke_boundary_reached(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}").to_lowercase();
    ["api key", "required for", "provider"]
        .iter()
        .any(|marker| combined.contains(marker))
}

fn initialize_smoke_repository(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    run_git(path, &["init"])?;
    fs::write(path.join("README.md"), "# smoke\n")?;
    run_git(path, &["config", "user.email", "smoke@example.com"])?;
    run_git(path, &["config", "user.name", "Smoke"])?;
    run_git(path, &["add", "."])?;
    run_git(path, &["commit", "-m", "base"])?;
    Ok(())
}

fn run_git(path: &Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let status = std_command("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git smoke setup failed: {status}").into())
    }
}

fn smoke_request(workspace: &Path) -> serde_json::Value {
    json!({
        "task": {
            "id": "smoke-1",
            "description": "Create hello.txt with content hello",
            "success_criteria": ["hello.txt exists"]
        },
        "workspace": {"root": workspace.to_string_lossy(), "base_ref": "HEAD"},
        "config": {
            "backend": "liberado-loop",
            "planner": {"model": "mock", "max_turns": 1},
            "coder": {
                "model": "deepseek/deepseek-v4-pro",
                "prompt": "You are a coding agent. Write files then submit_report.",
                "max_turns": 8
            },
            "critic": {"model": "mock", "max_turns": 1},
            "sandbox": {"backend": "host_local"},
            "command_policy": {"allow": [], "deny": [], "timeout_secs": 60, "output_max_bytes": 65536},
            "path_policy": {"allow_write_globs": ["**"], "deny_globs": [".git/**"], "read_max_bytes": 131072, "search_max_results": 50},
            "progress": {"read_only_turn_limit": 4, "same_tool_limit": 3, "validation_repeat_limit": 2, "max_attempts": 1, "event_preview_max_chars": 200}
        },
        "attempt": 0,
        "prior_feedback": []
    })
}

fn cmd_trace(args: &mut dyn Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut id_or_path: Option<String> = None;
    let mut explicit_path: Option<PathBuf> = None;
    let mut dirs: Vec<PathBuf> = default_trace_dirs();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--path" => {
                let p = args.next().ok_or("--path requires a value")?;
                explicit_path = Some(PathBuf::from(p));
            }
            "--dir" => {
                let d = args.next().ok_or("--dir requires a value")?;
                dirs.insert(0, PathBuf::from(d));
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag for coder trace: {other}").into());
            }
            other => {
                if id_or_path.is_some() {
                    return Err("coder trace takes a single session-id or path".into());
                }
                id_or_path = Some(other.to_string());
            }
        }
    }

    let path = if let Some(p) = explicit_path {
        p
    } else {
        let id = id_or_path.ok_or("usage: liberado coder trace <session-id|path>")?;
        let dir_refs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
        resolve_trace_path(&id, &dir_refs)?
    };

    let trace = load_trace(&path)?;
    print!("{}", render_transcript(&trace));
    Ok(())
}

fn cmd_compare(args: &mut dyn Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let all: Vec<String> = args.collect();
    if all.first().map(String::as_str) == Some("prepare") {
        return crate::compare_cmd::prepare(&all[1..]);
    }
    if all.first().map(String::as_str) == Some("run") {
        return crate::compare_cmd::run(&all[1..]);
    }
    if all.first().map(String::as_str) == Some("save") {
        return crate::compare_cmd::save(&all[1..]);
    }
    if matches!(
        all.first().map(String::as_str),
        Some("submit" | "doctor" | "status" | "await" | "cancel" | "report" | "worker")
    ) {
        return liberado_harness_eval::job_cli::run(&all);
    }
    if all.first().map(String::as_str) == Some("reset") {
        return cmd_compare_reset(&all[1..]);
    }

    let mut paths: Vec<String> = Vec::new();
    let mut dirs: Vec<PathBuf> = default_trace_dirs();
    let mut as_json = false;

    let mut all = all.into_iter();
    while let Some(arg) = all.next() {
        match arg.as_str() {
            "--dir" => {
                let d = all.next().ok_or("--dir requires a value")?;
                dirs.insert(0, PathBuf::from(d));
            }
            "--json" => as_json = true,
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag for coder compare: {other}").into());
            }
            other => paths.push(other.to_string()),
        }
    }

    if paths.len() != 2 {
        return Err("usage: liberado coder compare <trace-a> <trace-b>".into());
    }

    let dir_refs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
    let path_a = resolve_trace_path(&paths[0], &dir_refs)?;
    let path_b = resolve_trace_path(&paths[1], &dir_refs)?;
    let a = load_trace(&path_a)?;
    let b = load_trace(&path_b)?;
    let comparison = compare_traces(&a, &b);

    if as_json {
        println!("{}", serde_json::to_string_pretty(&comparison)?);
    } else {
        print!("{}", format_comparison(&comparison));
    }
    Ok(())
}

fn cmd_compare_reset(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut path = None;
    let mut commit = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => {
                println!("usage: liberado coder compare reset <path> [--commit <sha>]");
                return Ok(());
            }
            "--commit" => {
                index += 1;
                commit = Some(args.get(index).ok_or("--commit requires a value")?.clone());
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown flag for coder compare reset: {value}").into());
            }
            value => {
                if path.is_some() {
                    return Err("coder compare reset takes one workspace path".into());
                }
                path = Some(PathBuf::from(value));
            }
        }
        index += 1;
    }

    let path = path.ok_or("usage: liberado coder compare reset <path> [--commit <sha>]")?;
    if !path.is_dir() {
        return Err(format!("compare workspace does not exist: {}", path.display()).into());
    }
    reject_sibling_links(&path)?;

    if let Some(commit) = commit {
        run_git_inherit(&path, &["checkout", "--detach", &commit])?;
    }
    run_git_inherit(
        &path,
        &["restore", "--source=HEAD", "--worktree", "--staged", "."],
    )?;
    run_git_inherit(&path, &["status", "-sb"])?;
    let short = run_git_capture(&path, &["rev-parse", "--short", "HEAD"])?;
    println!("{}", short.trim());
    println!("restored tracked files; untracked path-deps left in place");
    Ok(())
}

fn reject_sibling_links(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    for name in ["turbovault", "turbomcp"] {
        let sibling = path.join(name);
        if sibling.exists() && fs::symlink_metadata(&sibling)?.file_type().is_symlink() {
            return Err(format!(
                "refusing linked sibling path dependency {name}; copy it into the workspace instead"
            )
            .into());
        }
    }
    Ok(())
}

fn run_git_inherit(path: &Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let status = std_command("git").arg("-C").arg(path).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git {} failed with {status}", args.join(" ")).into())
    }
}

fn run_git_capture(path: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = std_command("git").arg("-C").arg(path).args(args).output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!("git {} failed with {}", args.join(" "), output.status).into())
    }
}

/// `liberado coder diff <a> <b>` — the cross-harness question: same task, two harnesses, where did
/// they stop doing the same thing and what did each do next.
///
/// Takes anything either side writes: a native `coder-traces/*.json`, our `.messages.json`, a
/// `kilo export`, a Kilo extension `api_conversation_history.json`, an OpenHands trajectory. Paths
/// are used as given — unlike `compare`, there is no session-id resolution, because the foreign
/// side does not live in `coder-traces/`.
fn cmd_diff(args: &mut dyn Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut as_json = false;

    for arg in &mut *args {
        match arg.as_str() {
            "--json" => as_json = true,
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag for coder diff: {other}").into());
            }
            other => paths.push(PathBuf::from(other)),
        }
    }

    if paths.len() != 2 {
        return Err("usage: liberado coder diff <run-a> <run-b>".into());
    }

    let a = load_run_view(&paths[0])?;
    let b = load_run_view(&paths[1])?;

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "a": a,
                "b": b,
                "divergence": diverge(&a, &b),
            }))?
        );
    } else {
        print!("{}", format_divergence(&a, &b));
    }
    Ok(())
}

fn cmd_import(args: &mut dyn Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut format: Option<ForeignTraceFormat> = None;
    let mut session_id: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-o" | "--output" => {
                let p = args.next().ok_or("-o requires a value")?;
                output = Some(PathBuf::from(p));
            }
            "--format" => {
                let f = args.next().ok_or("--format requires kilo|openhands|auto")?;
                format = match f.as_str() {
                    // `kilo` is the VS Code extension's api_conversation_history.json;
                    // `kilo-cli` is `kilo export` from the CLI. Different products, same name.
                    "kilo" => Some(ForeignTraceFormat::Kilo),
                    "kilo-cli" | "kilocli" => Some(ForeignTraceFormat::KiloCli),
                    "openhands" | "oh" => Some(ForeignTraceFormat::OpenHands),
                    "auto" => None,
                    other => {
                        return Err(format!(
                            "unknown --format '{other}' (expected kilo|kilo-cli|openhands|auto)"
                        )
                        .into());
                    }
                };
            }
            "--session-id" => {
                session_id = Some(args.next().ok_or("--session-id requires a value")?);
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag for coder import: {other}").into());
            }
            other => {
                if input.is_some() {
                    return Err("coder import takes a single input path".into());
                }
                input = Some(PathBuf::from(other));
            }
        }
    }

    let input =
        input.ok_or("usage: liberado coder import <foreign.json> [-o out.messages.json]")?;
    let (detected, export) = import_foreign_file(&input, format, session_id)?;

    let out_path = output.unwrap_or_else(|| {
        let stem = input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("imported");
        PathBuf::from(format!("{stem}.messages.json"))
    });

    write_messages_export(&out_path, &export)?;
    eprintln!(
        "imported {:?} → {} ({} messages, session_id={})",
        detected,
        out_path.display(),
        export.messages.len(),
        export.session_id
    );
    // Also print the JSON so pipes/scripts can consume it without opening the file.
    println!("{}", serde_json::to_string_pretty(&export)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        cmd_compare, cmd_compare_reset, cmd_diff, cmd_import, cmd_trace, default_trace_dirs,
        reject_sibling_links, run_git_capture, smoke_arg_check, smoke_boundary_reached,
        smoke_request,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn smoke_request_contains_a_git_backed_task_and_fail_closed_config() {
        let request = smoke_request(Path::new("C:/smoke-workspace"));
        assert_eq!(request["task"]["id"], "smoke-1");
        assert_eq!(request["workspace"]["base_ref"], "HEAD");
        assert_eq!(request["config"]["coder"]["max_turns"], 8);
        assert_eq!(
            request["config"]["command_policy"]["allow"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(request["config"]["path_policy"]["deny_globs"][0], ".git/**");
    }

    #[test]
    fn smoke_arg_check_accepts_help_and_rejects_unknown_args() {
        assert!(smoke_arg_check(&mut vec!["-h".to_owned()].into_iter()).is_ok());
        assert!(smoke_arg_check(&mut vec!["--help".to_owned()].into_iter()).is_ok());
        assert!(smoke_arg_check(&mut vec!["extra".to_owned()].into_iter()).is_err());
        assert!(smoke_arg_check(&mut vec![].into_iter()).is_ok());
    }

    #[test]
    fn smoke_boundary_reached_flags_provider_credential_messages() {
        assert!(smoke_boundary_reached("", "API key required for provider"));
        assert!(smoke_boundary_reached("no provider key found", ""));
        assert!(smoke_boundary_reached(
            "Please configure a Provider first",
            ""
        ));
    }

    #[test]
    fn smoke_boundary_reached_is_case_insensitive() {
        assert!(smoke_boundary_reached("", "Api Key"));
        assert!(smoke_boundary_reached("PROVIDER", ""));
    }

    #[test]
    fn smoke_boundary_reached_ignores_unrelated_failures() {
        assert!(!smoke_boundary_reached("", "segmentation fault"));
        assert!(!smoke_boundary_reached("", ""));
        assert!(!smoke_boundary_reached("build failed", ""));
    }

    #[test]
    fn compare_consumes_dir_value_before_resolving_traces() {
        let mut args = vec![
            "--dir".to_owned(),
            "custom-traces".to_owned(),
            "missing-a".to_owned(),
            "missing-b".to_owned(),
        ]
        .into_iter();
        let error = cmd_compare(&mut args).unwrap_err().to_string();
        assert!(!error.contains("--dir requires a value"), "{error}");
    }

    // ── cmd_trace arg parsing ───────────────────────────────────────────

    #[test]
    fn trace_help_prints_usage() {
        let mut args = vec!["-h".to_owned()].into_iter();
        assert!(cmd_trace(&mut args).is_ok());
    }

    #[test]
    fn trace_rejects_unknown_flag() {
        let mut args = vec!["--bogus".to_owned()].into_iter();
        let err = cmd_trace(&mut args).unwrap_err().to_string();
        assert!(err.contains("unknown flag for coder trace"), "{err}");
    }

    #[test]
    fn trace_rejects_two_positionals() {
        let mut args = vec!["id1".to_owned(), "id2".to_owned()].into_iter();
        let err = cmd_trace(&mut args).unwrap_err().to_string();
        assert!(err.contains("takes a single session-id"), "{err}");
    }

    #[test]
    fn trace_requires_a_value_after_flag() {
        let mut args = vec!["--path".to_owned()].into_iter();
        let err = cmd_trace(&mut args).unwrap_err().to_string();
        assert!(err.contains("--path requires a value"), "{err}");
        let mut args = vec!["--dir".to_owned()].into_iter();
        let err = cmd_trace(&mut args).unwrap_err().to_string();
        assert!(err.contains("--dir requires a value"), "{err}");
    }

    /// A trace with neither an explicit path nor a positional id reaches the "usage" error.
    #[test]
    fn trace_without_id_reaches_usage() {
        let mut args = Vec::<String>::new().into_iter();
        let err = cmd_trace(&mut args).unwrap_err().to_string();
        assert!(err.contains("usage: liberado coder trace"), "{err}");
    }

    // ── cmd_diff arg parsing ────────────────────────────────────────────

    #[test]
    fn diff_help_prints_usage() {
        let mut args = vec!["-h".to_owned()].into_iter();
        assert!(cmd_diff(&mut args).is_ok());
    }

    #[test]
    fn diff_rejects_unknown_flag() {
        let mut args = vec!["--bogus".to_owned()].into_iter();
        let err = cmd_diff(&mut args).unwrap_err().to_string();
        assert!(err.contains("unknown flag for coder diff"), "{err}");
    }

    #[test]
    fn diff_requires_two_paths() {
        let mut args = vec!["one".to_owned()].into_iter();
        let err = cmd_diff(&mut args).unwrap_err().to_string();
        assert!(err.contains("usage: liberado coder diff"), "{err}");
    }

    // ── cmd_import arg parsing ──────────────────────────────────────────

    #[test]
    fn import_help_prints_usage() {
        let mut args = vec!["-h".to_owned()].into_iter();
        assert!(cmd_import(&mut args).is_ok());
    }

    #[test]
    fn import_rejects_unknown_flag() {
        let mut args = vec!["--bogus".to_owned()].into_iter();
        let err = cmd_import(&mut args).unwrap_err().to_string();
        assert!(err.contains("unknown flag for coder import"), "{err}");
    }

    #[test]
    fn import_rejects_bad_format() {
        let mut args = vec!["--format".into(), "bogus".into(), "in.json".into()].into_iter();
        let err = cmd_import(&mut args).unwrap_err().to_string();
        assert!(err.contains("unknown --format"), "{err}");
    }

    #[test]
    fn import_requires_an_input_path() {
        let mut args = vec!["--format".into(), "kilo".into()].into_iter();
        let err = cmd_import(&mut args).unwrap_err().to_string();
        assert!(err.contains("usage: liberado coder import"), "{err}");
    }

    // ── cmd_compare_reset arg parsing ───────────────────────────────────

    #[test]
    fn compare_reset_help_prints_usage() {
        let args = vec!["-h".to_owned()];
        assert!(cmd_compare_reset(&args).is_ok());
    }

    #[test]
    fn compare_reset_rejects_unknown_flag() {
        let args = vec!["--bogus".to_owned()];
        let err = cmd_compare_reset(&args).unwrap_err().to_string();
        assert!(
            err.contains("unknown flag for coder compare reset"),
            "{err}"
        );
    }

    #[test]
    fn compare_reset_requires_a_value_after_flag() {
        let args = vec!["--commit".to_owned()];
        let err = cmd_compare_reset(&args).unwrap_err().to_string();
        assert!(err.contains("--commit requires a value"), "{err}");
    }

    #[test]
    fn compare_reset_requires_a_path() {
        let args = vec!["--commit".into(), "abc123".into()];
        let err = cmd_compare_reset(&args).unwrap_err().to_string();
        assert!(err.contains("usage: liberado coder compare reset"), "{err}");
    }

    #[test]
    fn compare_reset_rejects_unknown_positional() {
        let args = vec!["a".into(), "b".into()];
        let err = cmd_compare_reset(&args).unwrap_err().to_string();
        assert!(err.contains("takes one workspace path"), "{err}");
    }

    // ── default_trace_dirs ──────────────────────────────────────────────

    #[test]
    fn trace_dirs_include_cwd_and_a_relative_fallback() {
        let dirs = default_trace_dirs();
        assert!(dirs.contains(&PathBuf::from("coder-traces")));
        let cwd = std::env::current_dir().unwrap().join("coder-traces");
        assert!(dirs.contains(&cwd), "{dirs:?}");
    }

    // ── cmd_compare / cmd_import flag guards ────────────────────────────

    /// A flag-looking positional is refused as an unknown flag, not silently treated as a path.
    #[test]
    fn compare_rejects_unknown_flags() {
        let mut args = vec!["--bogus".to_owned()].into_iter();
        let err = cmd_compare(&mut args).unwrap_err().to_string();
        assert!(err.contains("unknown flag for coder compare"), "{err}");
    }

    /// Two plain positionals reach the trace resolver, not the flag handler.
    #[test]
    fn compare_treats_plain_positionals_as_paths() {
        let mut args = vec!["missing-a".to_owned(), "missing-b".to_owned()].into_iter();
        let err = cmd_compare(&mut args).unwrap_err().to_string();
        assert!(!err.contains("unknown flag"), "{err}");
    }

    #[test]
    fn compare_requires_two_paths() {
        let mut args = vec!["only-one".to_owned()].into_iter();
        let err = cmd_compare(&mut args).unwrap_err().to_string();
        assert!(err.contains("usage: liberado coder compare"), "{err}");
    }

    /// Same flag/positional split for the import subcommand.
    #[test]
    fn import_rejects_unknown_flags() {
        let mut args = vec!["--bogus".to_owned()].into_iter();
        let err = cmd_import(&mut args).unwrap_err().to_string();
        assert!(err.contains("unknown flag for coder import"), "{err}");
    }

    #[test]
    fn import_treats_plain_positionals_as_paths() {
        let mut args = vec!["missing.json".to_owned()].into_iter();
        let err = cmd_import(&mut args).unwrap_err().to_string();
        assert!(!err.contains("unknown flag"), "{err}");
    }

    // ── run_git_capture ────────────────────────────────────────────────

    /// `run_git_capture` returns the trimmed stdout of a successful git call.
    #[test]
    fn run_git_capture_reads_stdout() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "t@example.com"]);
        git(dir.path(), &["config", "user.name", "Test"]);
        fs::write(dir.path().join("f.txt"), "x").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-m", "base"]);
        let out = run_git_capture(dir.path(), &["rev-parse", "--short", "HEAD"]).unwrap();
        assert_eq!(out.trim().len(), 7, "short sha: {out}");
    }

    #[test]
    fn run_git_capture_fails_in_a_non_repository() {
        let dir = tempfile::tempdir().unwrap();
        assert!(run_git_capture(dir.path(), &["rev-parse", "--short", "HEAD"]).is_err());
    }

    fn git(path: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    // ── reject_sibling_links ────────────────────────────────────────────

    /// A regular directory named turbovault is not a symlink — it must not be rejected.
    #[test]
    fn reject_sibling_links_accepts_regular_dirs() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("turbovault")).unwrap();
        assert!(reject_sibling_links(dir.path()).is_ok());
    }

    /// A missing sibling directory is not an error.
    #[test]
    fn reject_sibling_links_accepts_absent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(reject_sibling_links(dir.path()).is_ok());
    }
}
