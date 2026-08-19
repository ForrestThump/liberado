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
    cargo build --locked --workspace

# Release build of the `liberado` binary.
build-release:
    cargo build --locked --release --bin liberado

# ── Test ─────────────────────────────────────────────────────────────────────

# Run the whole workspace test suite (includes the layer-rules gate).
test:
    cargo test --locked --workspace --no-fail-fast

# Run tests for one crate: `just test-p dispatcher`
test-p name:
    cargo test --locked -p liberado-{{name}}

# Run just the Tier-1 conformance suite (server, MockProvider, no network).
test-t1:
    cargo test --locked -p liberado-server -- t1_conformance

# ── Quality gates (what CI runs) ─────────────────────────────────────────────

# CI gate: fmt + clippy. Green is required before every commit.
check:
    cargo fmt --all -- --check
    cargo clippy --locked --workspace --exclude liberado-webui --all-targets -- -D warnings -D clippy::cognitive_complexity

# Full local CI. Same CRAP compare GitHub runs: a function whose score went
# up fails here and is named in the cargo-crap table — fix it before you push.
# The baseline is not rewritten while that check is red. On success, rewrite
# `crap-baseline.json` (the ratchet). If the tree is otherwise clean, the new
# baseline is amended onto HEAD so a later push includes it. If other files
# are dirty, it is only staged. GitHub never writes that file.
ci:
    cargo run --locked -p liberado-cli -- ci

# Auto-format the whole workspace.
fmt:
    cargo fmt --all

# Dependency security + license gate.
deny:
    cargo deny check

# Full local ship preflight. Runs through the native Liberado CLI on every host OS.
preflight:
    cargo run --locked -p liberado-cli -- ci check

# Validate the Rust-native PR shepherd's failure-identity and state-machine guards.
shepherd-self-test:
    cargo run --locked -p liberado-cli -- shepherd --self-test

# Print the resolved forge and daemon policy before running the shepherd.
shepherd-config:
    cargo run --locked -p liberado-cli -- shepherd config check

# Inspect all open PRs without changing GitHub labels, rerunning CI, or starting coder goals.
shepherd-dry-run:
    cargo run --locked -p liberado-cli -- shepherd --dry-run --once

# Validate the coder-runner process boundary.
coder-smoke:
    cargo run --locked -p liberado-cli -- coder smoke

# CI-safe mock coder curriculum. No model key or network access.
curriculum-mock:
    cargo test --locked -p liberado-heuristics-tuner --lib mock_curriculum -- --nocapture

# Run the heuristics tuner. It reads OPENROUTER_API_KEY from the environment.
tuner:
    cargo run --locked --quiet -p liberado-heuristics-tuner

# Verify every relative link in docs/ resolves to a real file.
# Skips http(s) URLs and .secret files; CI gates on it (doc-links job).
check-links:
    cargo run --locked -p liberado-cli -- docs check-links

# Verify that the checked-in crate map matches Cargo manifests.
check-crate-map:
    cargo run --locked -p liberado-cli -- docs crate-map

# Regenerate the crate map from Cargo manifests.
gen-crate-map:
    cargo run --locked -p liberado-cli -- docs crate-map --write

# Run the native documentation metadata self-test.
docs-meta-test:
    cargo run --locked -p liberado-cli -- docs metadata self-test

# Validate document metadata, generated indexes, and stale Rust doc paths.
docs-meta-check:
    cargo run --locked -p liberado-cli -- docs metadata lint
    cargo run --locked -p liberado-cli -- docs metadata check-stale-rs

# Generate the searchable documentation site.
docs-site out="":
    cargo run --locked -p liberado-cli -- docs site {{if out == "" { "" } else { "--out " + out }}}

# Summarize a Liberado, MVL, pi, or compare-run artifact.
summarize path:
    cargo run --locked -p liberado-cli -- coder summarize {{path}}

# Print the pinned, non-running MVL comparison preparation plan.
compare-prepare:
    cargo run --locked -p liberado-cli -- coder compare prepare

# Restore tracked files in a compare workspace; preserves untracked path dependencies.
compare-reset path commit="":
    cargo run --locked -p liberado-cli -- coder compare reset {{path}} {{if commit == "" { "" } else { "--commit " + commit }}}

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

[windows]
webui: # Start the detached daemon that serves the built WebUI.
    powershell -ExecutionPolicy Bypass -File scripts/start-webui.ps1

[windows]
webui-build: # Build the WebUI, then start the detached WebUI daemon.
    powershell -ExecutionPolicy Bypass -File scripts/start-webui.ps1 -Build

[windows]
webui-dev: # Start Dioxus WebUI hot reload; requires a daemon on port 4201.
    powershell -ExecutionPolicy Bypass -File scripts/start-webui-dev.ps1

[windows]
stop-webui: # Stop the detached WebUI daemon.
    powershell -ExecutionPolicy Bypass -File scripts/stop-webui.ps1

[windows]
stop-webui-dev: # Stop the Dioxus WebUI development server.
    powershell -ExecutionPolicy Bypass -File scripts/stop-webui-dev.ps1

[windows]
paseo-install: # Install liberado-acp and register its Paseo provider.
    powershell -ExecutionPolicy Bypass -File scripts/install-paseo-liberado.ps1

[windows]
deploy-webui-homelab: # Build and ship the WebUI bundle to the homelab.
    powershell -ExecutionPolicy Bypass -File scripts/deploy-webui-homelab.ps1

[windows]
deploy-homelab: # Build and deploy the daemon image to the homelab.
    powershell -ExecutionPolicy Bypass -File scripts/deploy-homelab.ps1

# Dispatch one ACP coding run through the same Node stdio boundary that Paseo uses.
acp-dispatch *args:
    node scripts/dispatch-acp-run.js {{args}}
