//! Subprocess e2e for the binary's startup wiring (config load, vault_path gate, env-driven model
//! selection). The full success path — `run_stdio` — needs a real ONNX embedding model, which CI
//! does not have; every line of `main()` up to model construction is exercised here
//! deterministically: a bogus model name fails *before* any model download.

use std::process::Command;

/// Minimal valid topology — everything except `vault_path` defaults in code.
const TOPOLOGY: &str = "vault_path = '{}'\n";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_liberado-memory-mcp")
}

#[test]
fn missing_config_is_an_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap(); // empty -> no topology.toml anywhere
    let out = Command::new(bin())
        .env("LIBERADO_CONFIG_DIR", dir.path())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.stderr.is_empty());
}

#[test]
fn empty_vault_path_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("topology.toml"), "vault_path = ''\n").unwrap();
    let out = Command::new(bin())
        .env("LIBERADO_CONFIG_DIR", dir.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("topology.vault_path is required"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Unknown model name fails deterministically before any download — exercising config load, the
/// vault_path gate, Vault::open, and the LIBERADO_MEMORY_MODEL env read in one subprocess.
#[test]
fn unknown_model_fails_before_model_download() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("topology.toml"),
        TOPOLOGY.replace("{}", &dir.path().join("vault").to_string_lossy()),
    )
    .unwrap();
    let out = Command::new(bin())
        .env("LIBERADO_CONFIG_DIR", dir.path())
        .env("LIBERADO_MEMORY_MODEL", "not-a-real-model")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown model name"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
