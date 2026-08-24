# Liberado â€” command runner (`just`). Cross-platform: plain `cargo â€¦` lines run under
# both `sh` (CI/Linux/macOS) and PowerShell (Windows). PowerShell-only recipes are
# tagged `[windows]`.

set dotenv-load := true
set quiet := true

default:
    @just --list

# â”€â”€ Build â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

# Build the full native workspace (webui is WASM-only and excluded).
build:
    cargo build --locked --workspace

# Release build of the `liberado` binary.
build-release:
    cargo build --locked --release --bin liberado

# â”€â”€ Test â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

# Run the whole workspace test suite (includes the layer-rules gate).
test:
    cargo test --locked --workspace --no-fail-fast

# Run tests for one crate: `just test-p dispatcher`
test-p name:
    cargo test --locked -p liberado-{{name}}

# Run just the Tier-1 conformance suite (server, MockProvider, no network).
test-t1:
    cargo test --locked -p liberado-server -- t1_conformance

# â”€â”€ Quality gates (what CI runs) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

# CI gate: fmt + clippy. Green is required before every commit.
check:
    cargo fmt --check
    cargo clippy --locked --workspace --exclude liberado-webui --all-targets -- -D warnings -D clippy::cognitive_complexity

# Full local CI. Includes the host-stable module-health ratchet on every OS.
# Linux also runs the same per-function CRAP compare GitHub runs
# (a score that went up fails and is named â€” fix it before you push).
# Windows checks the 150 ceiling only; coverage is host-sensitive.
# The baseline is not rewritten while that check is red. On Linux success,
# rewrite `crap-baseline.json`. If the tree is otherwise clean, a Linux
# rewrite is amended onto HEAD. GitHub never writes that file.
# Console: log path, one ok/FAILED per gate, extracted errors on red.
# Full child output: `.liberado/ci.log`.
ci:
    cargo run --locked --quiet -p liberado-cli -- ci

# Auto-format the whole workspace.
fmt:
    cargo fmt --all

# Dependency security + license gate.
deny:
    cargo deny --locked check

# Resolve and inspect dependencies without compiling them.
dependency-security:
    cargo metadata --locked --format-version=1
    cargo deny --locked check
    cargo vet --locked

# Full local ship preflight. Runs through the native Liberado CLI on every host OS.
preflight:
    cargo run --locked --quiet -p liberado-cli -- ci check

# Fast cross-platform pre-push gate. Writes a receipt bound to HEAD and the tree.
ready:
    cargo run --locked --quiet -p liberado-cli -- ci ready

# Refuse when HEAD or any tracked/untracked source changed after `just ready`.
verify-ready:
    cargo run --locked --quiet -p liberado-cli -- ci verify-ready

# Push the current branch only when its readiness receipt is current.
push: verify-ready
    git push

# Install the committed cross-platform pre-push receipt verifier.
setup-hooks:
    git config core.hooksPath .githooks

# Exact Debian CRAP comparison: native on Debian/Linux, Debian under WSL on Windows.
crap-linux:
    cargo run --locked --quiet -p liberado-cli -- ci crap-linux

# Host-stable per-function cyclomatic-complexity ratchet.
function-complexity:
    cargo run --locked --quiet -p liberado-cli -- ci complexity

function-complexity-ratchet:
    cargo run --locked --quiet -p liberado-cli -- ci complexity-ratchet

# Compare production Rust files with the committed structural-health baseline.
module-health:
    cargo run --locked --quiet -p liberado-cli -- ci modules

# Check first, then replace the structural-health baseline with current values.
module-health-ratchet:
    cargo run --locked --quiet -p liberado-cli -- ci modules-ratchet

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

# Launch the interactive system map (native window).
sysmap:
    cargo run --locked -p liberado-sysmap-cli

# Write the generated system-map graph as JSON (headless; proves the map is data-driven).
sysmap-json path:
    cargo run --locked -p liberado-sysmap-cli -- --write-json "{{path}}"

# Run the native documentation metadata self-test.
docs-meta-test:
    cargo run --locked -p liberado-cli -- docs metadata self-test

# Validate document metadata, generated indexes, and stale Rust doc paths.
docs-meta-check:
    cargo run --locked -p liberado-cli -- docs metadata lint
    cargo run --locked -p liberado-cli -- docs metadata check-stale-rs

# Check source-to-doc contracts, vocabulary, and opt-in executable examples.
docs-audit:
    cargo run --locked -p liberado-cli -- docs audit

# Require docs review for contract-bearing source changes since a git revision.
docs-impact base:
    cargo run --locked -p liberado-cli -- docs audit --base "{{base}}"

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

# â”€â”€ Mutation testing â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
#
# Playbook: Skills/mutants-campaign.md â€” cold-start assessment, run, record, fix survivors.
# Ledger: mutants-ledger.json (append-only). Health: just mutants-report / just mutants-next.
#
# Run cargo-mutants on one crate and append results to mutants-ledger.json.
# Example: `just mutants executor`
#
# CARGO_TARGET_DIR keeps the invoke binary out of target/debug/liberado.exe so
# `cargo mutants -p liberado-cli` can rebuild that path without Access denied.
# Mutants builds go to target/mutants/ (see mutants_cmd.rs).
# The env comes from the recipe-scoped [env] attribute, not a shell prefix:
# `set "X=Y"&&` is cmd.exe syntax, which neither sh nor PowerShell runs.
[env('CARGO_TARGET_DIR', 'target/liberado-invoke')]
mutants name:
    cargo run --locked --quiet -p liberado-cli -- mutants run {{name}}

# coder-agent: lib tests only (mock_intake_e2e hangs in cargo-mutants temp env).
[env('CARGO_TARGET_DIR', 'target/liberado-invoke')]
mutants-agent:
    cargo run --locked --quiet -p liberado-cli -- mutants run --lib-only coder-agent

# Ingest an existing mutants.out without re-running cargo mutants.
[env('CARGO_TARGET_DIR', 'target/liberado-invoke')]
mutants-record name:
    cargo run --locked --quiet -p liberado-cli -- mutants record {{name}}

# Health report: never campaigned, historical-only, most drift since last SHA run.
[env('CARGO_TARGET_DIR', 'target/liberado-invoke')]
mutants-report:
    cargo run --locked --quiet -p liberado-cli -- mutants report

# Print one crate directory name to mutation-test next (see Skills/mutants-campaign.md).
[env('CARGO_TARGET_DIR', 'target/liberado-invoke')]
mutants-next:
    cargo run --locked --quiet -p liberado-cli -- mutants next

# â”€â”€ Run â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

# Serve the daemon + HTTP API on the vault at `LIBERADO_VAULT` (or $1).
serve vault:
    cargo run --bin liberado -- serve {{vault}}

# Validate the resolved config without starting the daemon.
config-check:
    cargo run --bin liberado -- config check

# â”€â”€ Windows dev-stack helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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
