# stop-webui-dev.ps1 -- Stop the detached `dx serve` dev server (and its child build
# processes) started by start-webui-dev.ps1.
#
# The pidfile alone isn't reliable: if it goes missing or points at a PID that has
# already exited (e.g. an earlier stop attempt removed the file without actually
# killing the process), a `dx serve` for this project can keep running orphaned.
# So this also scans for any `dx.exe` process whose command line mentions
# liberado-webui and kills it, regardless of what the pidfile says.
#
# Usage:
#   .\scripts\stop-webui-dev.ps1

$ErrorActionPreference = "SilentlyContinue"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$pidFile = Join-Path $root ".liberado\webui-dev.pid"

$stoppedAny = $false

if (Test-Path $pidFile) {
    $procId = (Get-Content $pidFile | Select-Object -First 1)
    if ($procId -and (Get-Process -Id $procId -ErrorAction SilentlyContinue)) {
        & taskkill /PID $procId /T /F | Out-Null
        Write-Host "Stopped dx serve (PID $procId) and children." -ForegroundColor Green
        $stoppedAny = $true
    }
    Remove-Item $pidFile -ErrorAction SilentlyContinue
}

# Fallback: find any dx.exe still serving this project by command line, in case
# the pidfile was stale, missing, or didn't match the process actually running.
$orphans = Get-CimInstance Win32_Process -Filter "Name='dx.exe'" -ErrorAction SilentlyContinue |
    Where-Object { $_.CommandLine -match 'liberado-webui' }
foreach ($p in $orphans) {
    & taskkill /PID $p.ProcessId /T /F | Out-Null
    Write-Host "Stopped orphaned dx serve (PID $($p.ProcessId))." -ForegroundColor Green
    $stoppedAny = $true
}

if (-not $stoppedAny) {
    Write-Host "No dx serve process found -- nothing to stop." -ForegroundColor Yellow
}
