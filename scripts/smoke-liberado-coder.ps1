# Compatibility wrapper for the native coder process-boundary smoke.
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root
& cargo run --locked -p liberado-cli -- coder smoke
exit $LASTEXITCODE
