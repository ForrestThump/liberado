# Liberado ship preflight (Windows) — mirrors .github/workflows/ci.yml host checks.
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
cargo run -p liberado-cli -- ci check
exit $LASTEXITCODE
