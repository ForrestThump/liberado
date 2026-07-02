# start-daemon.ps1 -- Start the Liberado daemon (API + vault watcher on
# 0.0.0.0:PORT) as a DETACHED background process, WITHOUT requiring a built
# WebUI wasm bundle.
#
# This is the dev-workflow companion to start-webui-dev.ps1: run this for the
# API, then start-webui-dev.ps1 for the hot-reload frontend (dx serve on its
# own port). It shares its PID/log files with start-webui.ps1, since both
# ultimately start the same `liberado serve` process on the same port -- if
# you want the daemon to *also* serve a built static frontend, use
# start-webui.ps1 -Build instead.
#
# Usage:
#   .\scripts\start-daemon.ps1 [-VaultPath C:\path\to\vault]
#   $env:LIBERADO_VAULT = "C:\vault"; .\scripts\start-daemon.ps1
#   $env:DEEPSEEK_API_KEY = "sk-..."; .\scripts\start-daemon.ps1   # enables chat
#
# Stop with: .\scripts\stop-daemon.ps1

param([string]$VaultPath = "")
$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$port = if ($env:LIBERADO_PORT) { $env:LIBERADO_PORT } else { "4201" }
$stateDir = Join-Path $root ".liberado"
$pidFile = Join-Path $stateDir "webui.pid"
$logFile = Join-Path $stateDir "webui.log"
New-Item -ItemType Directory -Force -Path $stateDir | Out-Null

# -- Already running? --
if (Test-Path $pidFile) {
    $existing = (Get-Content $pidFile -ErrorAction SilentlyContinue | Select-Object -First 1)
    $existingProc = Get-Process -Id $existing -ErrorAction SilentlyContinue
    if ($existingProc -and $existingProc.ProcessName -eq "liberado") {
        Write-Host "Daemon already running (PID $existing). Run stop-daemon.ps1 first." -ForegroundColor Yellow
        exit 0
    }
}

# -- Resolve vault: -VaultPath > LIBERADO_VAULT > topology.toml --
$resolvedVault = $VaultPath
if (-not $resolvedVault -and $env:LIBERADO_VAULT) { $resolvedVault = $env:LIBERADO_VAULT }
if (-not $resolvedVault) {
    $configDir = if ($env:LIBERADO_CONFIG_DIR) { $env:LIBERADO_CONFIG_DIR } else { "$env:APPDATA\liberado" }
    $topoFile = Join-Path $configDir "topology.toml"
    if (Test-Path $topoFile) {
        if ((Get-Content $topoFile -Raw) -match 'vault_path\s*=\s*"([^"]+)"') { $resolvedVault = $matches[1] }
    }
}
if (-not $resolvedVault) {
    Write-Host "No vault path. Pass -VaultPath, set LIBERADO_VAULT, or configure topology.toml." -ForegroundColor Red
    exit 1
}

# -- Ensure the daemon binary exists (build once; run the binary, not `cargo
#    run`, so the PID we track is the daemon itself, not a cargo wrapper). --
$bin = Join-Path $root "target\debug\liberado.exe"
Write-Host "Ensuring daemon binary is built..." -ForegroundColor Cyan
& cargo build --bin liberado
if ($LASTEXITCODE -ne 0) { Write-Host "Daemon build failed." -ForegroundColor Red; exit 1 }

# -- Start the daemon detached, logging to file --
Write-Host "Vault:  $resolvedVault" -ForegroundColor Cyan
Write-Host "Serving API on 0.0.0.0:$port (no static frontend -- pair with start-webui-dev.ps1) ..." -ForegroundColor Cyan
$proc = Start-Process -FilePath $bin `
    -ArgumentList @("serve", $resolvedVault) `
    -WorkingDirectory $root `
    -RedirectStandardOutput $logFile `
    -RedirectStandardError "$logFile.err" `
    -WindowStyle Hidden -PassThru
$proc.Id | Out-File -FilePath $pidFile -Encoding ascii

# -- Wait for readiness --
$ready = $false
for ($i = 0; $i -lt 90; $i++) {
    try {
        $r = Invoke-WebRequest -Uri "http://127.0.0.1:$port/api/status" -TimeoutSec 2 -ErrorAction SilentlyContinue
        if ($r.StatusCode -eq 200) { $ready = $true; break }
    } catch {}
    if ($proc.HasExited) { Write-Host "Daemon exited early (see $logFile.err)." -ForegroundColor Red; exit 1 }
    Start-Sleep -Seconds 1
}
if (-not $ready) {
    Write-Host "Daemon did not become ready in 90s (see $logFile)." -ForegroundColor Red
    exit 1
}

Write-Host ""
Write-Host "Daemon is up (PID $($proc.Id)) -- API on http://127.0.0.1:$port" -ForegroundColor Green
Write-Host "  Logs:  $logFile" -ForegroundColor DarkGray
Write-Host "  Stop:  .\scripts\stop-daemon.ps1" -ForegroundColor DarkGray
Write-Host "  Now start the hot-reload frontend: .\scripts\start-webui-dev.ps1" -ForegroundColor DarkGray
