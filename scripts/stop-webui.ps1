# stop-webui.ps1 — Stop the detached Liberado WebUI daemon started by start-webui.ps1.
#
# Usage:
#   .\scripts\stop-webui.ps1

$ErrorActionPreference = "SilentlyContinue"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$pidFile = Join-Path $root ".liberado\webui.pid"

if (-not (Test-Path $pidFile)) {
    Write-Host "No .liberado\webui.pid — nothing to stop." -ForegroundColor Yellow
    exit 0
}

$procId = (Get-Content $pidFile | Select-Object -First 1)
$proc = Get-Process -Id $procId -ErrorAction SilentlyContinue
if ($proc) {
    Stop-Process -Id $procId -Force
    Write-Host "Stopped WebUI daemon (PID $procId)." -ForegroundColor Green
} else {
    Write-Host "Process $procId is not running (stale pidfile)." -ForegroundColor Yellow
}
Remove-Item $pidFile -ErrorAction SilentlyContinue
