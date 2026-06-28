# start-webui-dev.ps1 — Run the WebUI via `dx serve` (HOT RELOAD) as a DETACHED
# process, decoupled from the daemon. Edit RSX/Rust under crates/webui and the
# browser live-reloads — the fast inner loop for UX polish.
#
# The WASM talks to the daemon's API on port 4201 (api_base targets <same-host>:4201),
# so the daemon must be running separately for API calls to work:
#   .\scripts\start-webui.ps1 -VaultPath <path>      (daemon serves API + a static UI)
# The dev front here is a *separate* process on its own port.
#
# Usage:
#   .\scripts\start-webui-dev.ps1 [-Port 8080]
# Stop with: .\scripts\stop-webui-dev.ps1

param([int]$Port = 8080)
$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$stateDir = Join-Path $root ".liberado"
$pidFile = Join-Path $stateDir "webui-dev.pid"
$logFile = Join-Path $stateDir "webui-dev.log"
New-Item -ItemType Directory -Force -Path $stateDir | Out-Null

# ── Already running? ──
if (Test-Path $pidFile) {
    $existing = (Get-Content $pidFile -ErrorAction SilentlyContinue | Select-Object -First 1)
    if ($existing -and (Get-Process -Id $existing -ErrorAction SilentlyContinue)) {
        Write-Host "dx serve already running (PID $existing). Run stop-webui-dev.ps1 first." -ForegroundColor Yellow
        exit 0
    }
}

# dx needs the rustup-managed cargo (it has the wasm32 std); the standalone cargo
# on PATH does not. Prefer it for this process.
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

# ── Warn if the daemon (API) isn't up — dx serves only the frontend ──
try {
    Invoke-WebRequest -Uri "http://127.0.0.1:4201/api/status" -TimeoutSec 2 -ErrorAction Stop | Out-Null
    Write-Host "Daemon API detected on :4201." -ForegroundColor Green
} catch {
    Write-Host "WARNING: no daemon on :4201 — the UI will load but API calls will fail." -ForegroundColor Yellow
    Write-Host "         Start it first:  .\scripts\start-webui.ps1 -VaultPath <path>" -ForegroundColor Yellow
}

Write-Host "Starting dx serve (hot reload) on 0.0.0.0:$Port ..." -ForegroundColor Cyan
$proc = Start-Process -FilePath "dx" `
    -ArgumentList @("serve", "-p", "liberado-webui", "--platform", "web", "--addr", "0.0.0.0", "--port", "$Port") `
    -WorkingDirectory $root `
    -RedirectStandardOutput $logFile -RedirectStandardError "$logFile.err" `
    -WindowStyle Hidden -PassThru
$proc.Id | Out-File -FilePath $pidFile -Encoding ascii

# ── Wait for the dev server (first build takes a bit) ──
$ready = $false
for ($i = 0; $i -lt 180; $i++) {
    try {
        $r = Invoke-WebRequest -Uri "http://127.0.0.1:$Port/" -TimeoutSec 2 -ErrorAction SilentlyContinue
        if ($r.StatusCode -eq 200) { $ready = $true; break }
    } catch {}
    if ($proc.HasExited) { Write-Host "dx serve exited early (see $logFile.err)." -ForegroundColor Red; exit 1 }
    Start-Sleep -Seconds 1
}

$lan = (Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue |
    Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' } |
    Select-Object -First 1 -ExpandProperty IPAddress)
Write-Host ""
if ($ready) {
    Write-Host "dx serve up (PID $($proc.Id)) — hot reload on file changes." -ForegroundColor Green
    Write-Host "  Local: http://127.0.0.1:$Port" -ForegroundColor Green
    if ($lan) { Write-Host "  LAN:   http://${lan}:$Port" -ForegroundColor Green }
} else {
    Write-Host "dx serve started (PID $($proc.Id)) but not ready yet — check $logFile." -ForegroundColor Yellow
}
Write-Host "  Logs:  $logFile" -ForegroundColor DarkGray
Write-Host "  Stop:  .\scripts\stop-webui-dev.ps1" -ForegroundColor DarkGray
