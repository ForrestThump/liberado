# Compatibility wrapper for the native Rust documentation link checker.
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
cargo run -p liberado-cli -- docs check-links
exit $LASTEXITCODE
