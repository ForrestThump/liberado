//! End-to-end CLI tests: spawn the real `liberado-mcp-forge` binary and check its exit codes and
//! messages. These are subprocess tests rather than in-process unit tests because `main` reads
//! `std::env::args()` and installs a global tracing subscriber — neither is safe to drive from
//! inside a test binary. Env vars are set per-child with `Command::env`, never on the test
//! process, so parallel execution stays race-free.

use std::path::Path;
use std::process::Command;

fn forge() -> Command {
    Command::new(env!("CARGO_BIN_EXE_liberado-mcp-forge"))
}

fn scaffold_project(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    )
    .unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    let status = Command::new("cargo")
        .current_dir(dir)
        .arg("generate-lockfile")
        .status()
        .expect("cargo runs");
    assert!(status.success(), "cargo generate-lockfile failed");
}

#[test]
fn no_args_prints_usage_and_fails() {
    let out = forge().output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("usage: liberado-mcp-forge sync"),
        "stderr: {stderr}"
    );
}

#[test]
fn unknown_command_prints_usage_and_fails() {
    let out = forge().arg("frobnicate").output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("usage: liberado-mcp-forge sync"),
        "stderr: {stderr}"
    );
}

#[test]
fn sync_only_without_a_value_fails() {
    let out = forge().args(["sync", "--only"]).output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--only requires a source name"),
        "stderr: {stderr}"
    );
}

#[test]
fn sync_unknown_flag_fails() {
    let out = forge().args(["sync", "--bogus"]).output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown flag"), "stderr: {stderr}");
}

#[test]
fn sync_only_with_a_name_is_parsed() {
    let config = tempfile::tempdir().unwrap();
    let install = tempfile::tempdir().unwrap();
    // An empty sources file parses fine, so the run reaches the "no sources to sync" check.
    std::fs::write(config.path().join("mcp-sources.toml"), "# nothing\n").unwrap();
    let out = forge()
        .args(["sync", "--only", "nonexistent"])
        .env("LIBERADO_CONFIG_DIR", config.path())
        .env("LIBERADO_MCP_INSTALL_DIR", install.path())
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no sources to sync"), "stderr: {stderr}");
}

#[test]
fn sync_installs_a_path_source_end_to_end() {
    let config = tempfile::tempdir().unwrap();
    let install = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    scaffold_project(project.path(), "hello");
    std::fs::create_dir_all(config.path()).unwrap();
    std::fs::write(
        config.path().join("mcp-sources.toml"),
        // Forward slashes: a raw Windows `\\` inside a TOML basic string is an escape sequence.
        format!(
            "[[source]]\nname = \"hello\"\npath = \"{}\"\n",
            project.path().display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();

    let out = forge()
        .arg("sync")
        .env("LIBERADO_CONFIG_DIR", config.path())
        .env("LIBERADO_MCP_INSTALL_DIR", install.path())
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "sync failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[hello] built"), "stdout: {stdout}");
    assert!(
        liberado_config::managed_binary_path(install.path(), "hello").is_file(),
        "the managed binary must exist where the daemon looks for it"
    );
}
