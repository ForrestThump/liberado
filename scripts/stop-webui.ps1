$ErrorActionPreference = "SilentlyContinue"
$pidFile = Join-Path (Resolve-Path (Join-Path $PSScriptRoot "..")).Path ".liberado\webui.pid"
if (-not (Test-Path $pidFile)) { exit 0 }
$procId = (Get-Content $pidFile | Select-Object -First 1)
$proc = Get-Process -Id $procId -ErrorAction SilentlyContinue
if ($proc) { Stop-Process -Id $procId -Force; Write-Host "Stopped daemon (PID $procId)." }
Remove-Item $pidFile -ErrorAction SilentlyContinue
