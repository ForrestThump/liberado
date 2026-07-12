# CI-safe mock coder curriculum (scratchpad slice D).
# No OPENROUTER_API_KEY. Fails the process if any smoke/core mock scenario regresses.
$ErrorActionPreference = "Stop"
cargo test -p liberado-heuristics-tuner --lib mock_curriculum -- --nocapture
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Host "mock curriculum green"
