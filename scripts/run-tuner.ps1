# run-tuner.ps1 -- Run one liberado-heuristics-tuner session end to end: builds if needed, runs
# the beam-search-with-restarts loop over liberado-eval's scenarios, and prints where every
# generation's rubric (and the final winner) got saved. Designed for a remote/SSH session (e.g.
# from Termux) where you can't easily babysit a live terminal -- one command, then read the
# output files at your leisure.
#
# Reads OPENROUTER_API_KEY from the environment -- it is never accepted as a script argument, so
# it never ends up in shell history or a process listing. Set it once per session before running:
#   $env:OPENROUTER_API_KEY = "sk-or-..."
#   .\scripts\run-tuner.ps1
#
# Every other tunable (model choice, beam width, call budget, generation count) comes from
# config.example/tuner.toml's real copy in your config dir, or an env var override
# (TUNER_CALL_BUDGET, etc. -- see crates/heuristics-tuner/src/config.rs) -- nothing here needs
# editing or recompiling between runs.

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

if (-not $env:OPENROUTER_API_KEY) {
    Write-Host "OPENROUTER_API_KEY is not set in this session." -ForegroundColor Red
    Write-Host '  $env:OPENROUTER_API_KEY = "sk-or-..."' -ForegroundColor DarkGray
    Write-Host "  .\scripts\run-tuner.ps1" -ForegroundColor DarkGray
    exit 1
}

Write-Host "Running liberado-heuristics-tuner (this makes real OpenRouter API calls)..." -ForegroundColor Cyan
Push-Location $root
try {
    & cargo run --quiet -p liberado-heuristics-tuner
    $exitCode = $LASTEXITCODE
} finally {
    Pop-Location
}

if ($exitCode -ne 0) {
    Write-Host "Tuner run failed (exit $exitCode) -- see output above." -ForegroundColor Red
    exit $exitCode
}

Write-Host "`nDone. Review the files above, then let's look at them together." -ForegroundColor Green
