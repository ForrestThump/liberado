# stop-daemon.ps1 -- Stop the daemon started by start-daemon.ps1 or
# start-webui.ps1 (they share the same PID file, since both start the same
# `liberado serve` process on the same port).
#
# Usage:
#   .\scripts\stop-daemon.ps1

$ErrorActionPreference = "SilentlyContinue"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$pidFile = Join-Path $root ".liberado\webui.pid"

if (-not (Test-Path $pidFile)) {
    Write-Host "No .liberado\webui.pid -- nothing to stop." -ForegroundColor Yellow
    exit 0
}

$procId = (Get-Content $pidFile | Select-Object -First 1)
$proc = Get-Process -Id $procId -ErrorAction SilentlyContinue
if ($proc -and $proc.ProcessName -eq "liberado") {
    Stop-Process -Id $procId -Force
    Write-Host "Stopped daemon (PID $procId)." -ForegroundColor Green
} elseif ($proc) {
    Write-Host "PID $procId is running but isn't the daemon (process name: $($proc.ProcessName)) -- leaving it alone." -ForegroundColor Yellow
} else {
    Write-Host "Process $procId is not running (stale pidfile)." -ForegroundColor Yellow
}
Remove-Item $pidFile -ErrorAction SilentlyContinue
