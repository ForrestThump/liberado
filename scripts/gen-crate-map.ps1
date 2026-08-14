# Compatibility wrapper for the native Rust crate-map generator.
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
cargo run --locked -p liberado-cli -- docs crate-map --write
exit $LASTEXITCODE
