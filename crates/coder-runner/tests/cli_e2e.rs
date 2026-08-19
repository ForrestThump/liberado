//! Subprocess e2e for the runner binary's CLI surface.
//!
//! Every path that does not need a live provider is exercised: arg parsing, usage errors, the
//! JSON-bridge request path (file and stdin), topology-driven provider selection, and the
//! headless path's deterministic api-key gate. The backend itself talks to a real provider API,
//! so runs stop where the provider would be contacted — the api-key gates fire first, and are
//! pinned exactly so a regression in the wiring shows up as a changed message, not a network call.

use std::process::{Command, Stdio};

use liberado_coder_agent::assemble::entry::runner_surface;
use liberado_coder_agent::assemble_production_run;
use liberado_coder_core::{CoderTask, CoderTuning};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_liberado-coder-run")
}

/// A fully-valid CoderRunRequest, produced by the same production assembler the binary uses, so
/// the e2e never hand-builds a request that serde would reject.
fn valid_request_json() -> String {
    let tuning = CoderTuning::default();
    let assembled = assemble_production_run(
        &tuning,
        runner_surface(
            CoderTask::new("e2e-1", "do the thing"),
            "/tmp/ws".into(),
            None,
            Some(30),
        ),
    );
    serde_json::to_string(&assembled.request).unwrap()
}

/// The bridge is only reachable when no API key is set — otherwise it would call the provider.
fn run_bridge(args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .args(args)
        .env_remove("LIBERADO_CODER_PROVIDER")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .output()
        .unwrap()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// No args means the JSON bridge in stdin mode (`--request -`); an empty stdin is an EOF parse
/// error, proving the default command shape without ever touching a provider.
#[test]
fn no_args_reads_stdin_request_and_rejects_eof() {
    let out = run_bridge(&[]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("parse CoderRunRequest JSON"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn unknown_flag_is_an_error() {
    let out = run_bridge(&["--wat"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("unknown argument"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn request_missing_path_is_an_error() {
    let out = run_bridge(&["--request"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("--request requires a path or '-'"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn request_missing_file_is_an_error() {
    let out = run_bridge(&["--request", r"C:\definitely\missing\request.json"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("read request"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn request_bad_json_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("request.json");
    std::fs::write(&path, "{{{nope").unwrap();
    let out = run_bridge(&["--request", path.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("parse CoderRunRequest"),
        "stderr: {}",
        stderr(&out)
    );
}

/// A valid request file: read_request succeeds, the provider resolves to the default deepseek
/// profile, and the gate is the missing api key — proving the whole bridge wiring up to the
/// provider boundary.
#[test]
fn request_valid_json_stops_at_the_api_key_gate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("request.json");
    std::fs::write(&path, valid_request_json()).unwrap();
    let out = run_bridge(&["--request", path.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("DEEPSEEK_API_KEY is required for provider 'deepseek'"),
        "stderr: {}",
        stderr(&out)
    );
}

/// `--request -` reads the request from stdin; the same gate proves the stdin path parsed it.
#[test]
fn request_from_stdin_is_read_and_reaches_the_api_key_gate() {
    let mut child = Command::new(bin())
        .args(["--request", "-"])
        .env_remove("LIBERADO_CODER_PROVIDER")
        .env_remove("DEEPSEEK_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(valid_request_json().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("DEEPSEEK_API_KEY is required"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `--config-dir` drives provider selection from the topology file, not the defaults.
#[test]
fn request_with_config_dir_uses_topology_provider() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("topology.toml"),
        r#"
provider = "homelab"
[[providers]]
name = "homelab"
base_url = "https://llm.internal/v1"
default_model = "m"
api_key_env = "HOMELAB_API_KEY"
"#,
    )
    .unwrap();
    let req = dir.path().join("request.json");
    std::fs::write(&req, valid_request_json()).unwrap();

    let out = run_bridge(&[
        "--request",
        req.to_str().unwrap(),
        "--config-dir",
        dir.path().to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("HOMELAB_API_KEY is required for provider 'homelab'"),
        "stderr: {}",
        stderr(&out)
    );
}

// ── headless (task run) ───────────────────────────────────────────────────────

#[test]
fn task_run_missing_prompt_is_an_error() {
    let out = run_bridge(&["task", "run", "--workspace", r"C:\tmp\ws"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("--prompt is required"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn task_run_bad_subcommand_is_an_error() {
    let out = run_bridge(&["task", "wat"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("expected 'run'"),
        "stderr: {}",
        stderr(&out)
    );
}

/// The headless path's first deterministic gate: the api key env must exist. Fires before any
/// git/repo-map/network work, so the defaults (DEFAULT_API_KEY_ENV etc.) are pinned here.
#[test]
fn task_run_without_api_key_env_is_an_error() {
    let out = run_bridge(&[
        "task",
        "run",
        "--prompt",
        "write hello.txt",
        "--workspace",
        r"C:\tmp\ws",
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("DEEPSEEK_API_KEY is required for headless task runner"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn task_run_custom_api_key_env_is_an_error_when_unset() {
    let out = run_bridge(&[
        "task",
        "run",
        "--prompt",
        "write hello.txt",
        "--workspace",
        r"C:\tmp\ws",
        "--api-key-env",
        "CONFORMANCE_UNSET_KEY_99",
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("CONFORMANCE_UNSET_KEY_99 is required for headless task runner"),
        "stderr: {}",
        stderr(&out)
    );
}
