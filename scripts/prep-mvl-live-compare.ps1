# Compatibility wrapper for the native compare preparation command.
$ErrorActionPreference = 'Stop'
$Repo = Split-Path -Parent $PSScriptRoot
Set-Location $Repo
& cargo run --locked -p liberado-cli -- coder compare prepare
exit $LASTEXITCODE
