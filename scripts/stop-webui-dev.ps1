# stop-webui-dev.ps1 — Stop the detached `dx serve` dev server (and its child build
# processes) started by start-webui-dev.ps1.
#
# Usage:
#   .\scripts\stop-webui-dev.ps1

$ErrorActionPreference = "SilentlyContinue"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$pidFile = Join-Path $root ".liberado\webui-dev.pid"

if (-not (Test-Path $pidFile)) {
    Write-Host "No .liberado\webui-dev.pid — nothing to stop." -ForegroundColor Yellow
    exit 0
}

$procId = (Get-Content $pidFile | Select-Object -First 1)
if (Get-Process -Id $procId -ErrorAction SilentlyContinue) {
    # dx spawns child cargo/build processes — kill the whole tree.
    & taskkill /PID $procId /T /F | Out-Null
    Write-Host "Stopped dx serve (PID $procId) and children." -ForegroundColor Green
} else {
    Write-Host "Process $procId is not running (stale pidfile)." -ForegroundColor Yellow
}
Remove-Item $pidFile -ErrorAction SilentlyContinue
