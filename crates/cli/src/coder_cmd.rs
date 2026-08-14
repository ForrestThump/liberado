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
     liberado coder compare prepare              print the pinned comparison plan\n  \
     liberado coder diff <run-a> <run-b> [--json]   cross-harness: where two runs parted\n  \
     liberado coder diff <run-a> <run-b> [--json]   cross-harness: where two runs parted\n  \
     liberado coder import <foreign.json> [-o <out.messages.json>] [--format kilo|kilo-cli|openhands|auto] [--session-id <id>]
  liberado coder smoke              validate the coder runner process boundary"
}

fn cmd_smoke(args: &mut dyn Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(arg) = args.next() {
        if arg == "-h" || arg == "--help" {
            println!("usage: liberado coder smoke");
            return Ok(());
        }
        return Err("usage: liberado coder smoke".into());
    }

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

    let combined = format!("{stdout}\n{stderr}").to_lowercase();
    if ["api key", "required for", "provider"]
        .iter()
        .any(|marker| combined.contains(marker))
    {
        println!("OK: runner reached the provider boundary without credentials");
        println!("  exit status: {}", output.status);
        return Ok(());
    }

    Err(format!(
        "coder smoke failed with {}:\n{}",
        output.status,
        combined.trim()
    )
    .into())
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
        return cmd_compare_prepare(&all[1..]);
    }
    if all.first().map(String::as_str) == Some("reset") {
        return cmd_compare_reset(&all[1..]);
    }

    let mut paths: Vec<String> = Vec::new();
    let mut dirs: Vec<PathBuf> = default_trace_dirs();
    let mut as_json = false;

    for arg in all {
        match arg.as_str() {
            "--dir" => {
                let d = args.next().ok_or("--dir requires a value")?;
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

fn cmd_compare_prepare(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if !args.is_empty() {
        if args == ["-h"] || args == ["--help"] {
            println!("usage: liberado coder compare prepare");
            return Ok(());
        }
        return Err("usage: liberado coder compare prepare".into());
    }

    let root = crate::crate_map_cmd::repository_root()?;
    let pin = "69933c9a8c8c5d64a35ac3d0a10bf1c0465adc1c";
    let model = "deepseek/deepseek-v4-pro";
    let provider = "openrouter";
    let temperature = "0.1";
    let max_turns = 30;
    let timeout_min = 45;

    println!("MVL live comparison PREP - print only. No harness started.");
    println!("Item: backlog 0.6 / roadmap 4b (emit joined MVL + execution logs)");
    println!("Commit: {pin}");
    println!("Provider: {provider}");
    println!("Model: {model}");
    println!("Sampling: temperature={temperature} max_tokens=unset");
    println!("Caps: max_turns={max_turns} timeout_min={timeout_min}");
    println!();
    println!(
        "See docs/future-work/mvl-live-comparison-prep.md for the shared prompt and output paths."
    );
    println!();
    println!("--- Liberado (ACP; print only, do not run) ---");
    println!(
        "node \"{}\" --cwd \"{}\" --config-dir \"{}\" --mode coding --timeout-min {timeout_min} --prompt TASK.txt",
        root.join("scripts").join("dispatch-acp-run.js").display(),
        root.display(),
        root.join("config").display()
    );
    println!();
    println!("--- pi (print only, do not run) ---");
    println!("pi --provider {provider} --model {model} --mode json -p TASK.txt");
    println!();
    println!("--- deepagents (print only, do not run) ---");
    println!("uv run python run_0_6.py   # create_deep_agent, native prompt/tools, same model");
    println!();
    println!("--- After a future run, judge any MVL with the Liberado oracle ---");
    println!(
        "cargo run -p liberado-test-support --bin mvl-conformance -- --mvl $OUT/run.mvl.jsonl --execution $OUT/run.execution.jsonl"
    );
    println!();
    println!(
        "Blocker: Liberado has no production MVL until 0.6; deepagents has no MVL writer here."
    );
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
    use super::smoke_request;
    use std::path::Path;

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
}
