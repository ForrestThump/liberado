#!/usr/bin/env bash
# Liberado ship preflight — same commands as .github/workflows/ci.yml (agent host).
# CI and liberado PreflightRunner should both invoke this (or the equivalent step list).
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --check
cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings
cargo test --workspace
cargo deny check
