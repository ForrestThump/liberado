# Liberado — command runner (`just`). Cross-platform: plain `cargo …` lines run under
# both `sh` (CI/Linux/macOS) and PowerShell (Windows). PowerShell-only recipes are
# tagged `[windows]`.

set dotenv-load := true
set quiet := true

default:
    @just --list

# ── Build ────────────────────────────────────────────────────────────────────

# Build the full native workspace (webui is WASM-only and excluded).
build:
    cargo build --workspace

# Release build of the `liberado` binary.
build-release:
    cargo build --release --bin liberado

# ── Test ─────────────────────────────────────────────────────────────────────

# Run the whole workspace test suite (includes the layer-rules gate).
test:
    cargo test --workspace

# Run tests for one crate: `just test-p dispatcher`
test-p name:
    cargo test -p liberado-{{name}}

# Run just the Tier-1 conformance suite (server, MockProvider, no network).
test-t1:
    cargo test -p liberado-server -- t1_conformance

# ── Quality gates (what CI runs) ─────────────────────────────────────────────

# CI gate: fmt + clippy. Green is required before every commit.
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings

# Auto-format the whole workspace.
fmt:
    cargo fmt --all

# Dependency security + license gate.
deny:
    cargo deny check

# Full local ship preflight. Runs through the native Liberado CLI on every host OS.
preflight:
    cargo run -p liberado-cli -- ci check

# Verify every relative link in docs/ resolves to a real file.
# Skips http(s) URLs and .secret files; CI gates on it (doc-links job).
check-links:
    {{ if os() == "windows" { "powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check-doc-links.ps1" } else { "pwsh -NoProfile -File scripts/check-doc-links.ps1" } }}

# ── Mutation testing ─────────────────────────────────────────────────────────

# Mutation-test one crate with hung-test protection:
#   `just mutants daemon`        → per-mutant timeout 60s floor
#   `just mutants coder-agent`   → short floor, lib tests only (integration tests hang)
mutants name:
    cargo mutants -p liberado-{{name}} --cap-lints true --timeout 3.0 --minimum-test-timeout 30

# coder-agent: run lib tests only (mock_intake_e2e hangs in cargo-mutants temp env).
mutants-agent:
    cargo mutants -p liberado-coder-agent --cap-lints true --timeout 3.0 --minimum-test-timeout 90 -- --lib

# ── Run ──────────────────────────────────────────────────────────────────────

# Serve the daemon + HTTP API on the vault at `LIBERADO_VAULT` (or $1).
serve vault:
    cargo run --bin liberado -- serve {{vault}}

# Validate the resolved config without starting the daemon.
config-check:
    cargo run --bin liberado -- config check

# ── Windows dev-stack helpers ────────────────────────────────────────────────

[windows]
dev-stack: # Rebuild + restart the whole dev stack (daemon, server, webui).
    powershell -ExecutionPolicy Bypass -File scripts/start-dev-stack.ps1 -Restart

[windows]
tui: # Run the ratatui TUI against the dev stack.
    powershell -ExecutionPolicy Bypass -File scripts/run-tui.ps1

[windows]
stop-daemon: # Stop the running daemon.
    powershell -ExecutionPolicy Bypass -File scripts/stop-daemon.ps1
