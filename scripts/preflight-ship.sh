#!/usr/bin/env bash
# Liberado ship preflight — same commands as .github/workflows/ci.yml (agent host).
# CI and liberado PreflightRunner should both invoke this (or the equivalent step list).
set -euo pipefail
cd "$(dirname "$0")/.."

cargo run -p liberado-cli -- ci check
