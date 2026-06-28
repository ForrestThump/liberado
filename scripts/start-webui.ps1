# start-webui.ps1 — Start the Liberado daemon (which serves the WebUI + API on
# 0.0.0.0:PORT) as a DETACHED background process, so this terminal stays free.
#
# The WebUI is a WASM bundle the daemon serves from its static fallback
# (DIST_DIR = target/dx/liberado-webui/release/web/public). Pass -Build to
# (re)build that bundle first (uses the rustup cargo, which has the wasm32 std).
#
# Usage:
#   .\scripts\start-webui.ps1 [-VaultPath C:\path\to\vault] [-Build]
#   $env:LIBERADO_VAULT = "C:\vault"; .\scripts\start-webui.ps1
#   $env:DEEPSEEK_API_KEY = "sk-..."; .\scripts\start-webui.ps1   # enables chat
#
# Stop with: .\scripts\stop-webui.ps1

param(
    [string]$VaultPath = "",
    [switch]$Build
)
$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$port = if ($env:LIBERADO_PORT) { $env:LIBERADO_PORT } else { "4201" }
$stateDir = Join-Path $root ".liberado"
$pidFile = Join-Path $stateDir "webui.pid"
$logFile = Join-Path $stateDir "webui.log"
New-Item -ItemType Directory -Force -Path $stateDir | Out-Null

# ── Already running? ──
if (Test-Path $pidFile) {
    $existing = (Get-Content $pidFile -ErrorAction SilentlyContinue | Select-Object -First 1)
    if ($existing -and (Get-Process -Id $existing -ErrorAction SilentlyContinue)) {
        Write-Host "WebUI daemon already running (PID $existing). Run stop-webui.ps1 first." -ForegroundColor Yellow
        exit 0
    }
}

# ── Resolve vault: -VaultPath > LIBERADO_VAULT > topology.toml ──
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

# ── Optionally (re)build the WebUI wasm bundle (release, to match DIST_DIR) ──
if ($Build) {
    Write-Host "Building WebUI (release wasm)..." -ForegroundColor Cyan
    # The PATH `cargo` may be a standalone install without the wasm32 std; the
    # rustup-managed cargo at ~/.cargo/bin has it. Prefer it for the dx build.
    $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
    & dx build -r -p liberado-webui --web
    if ($LASTEXITCODE -ne 0) { Write-Host "WebUI build failed." -ForegroundColor Red; exit 1 }
}

$dist = Join-Path $root "target\dx\liberado-webui\release\web\public"
if (-not (Test-Path (Join-Path $dist "index.html"))) {
    Write-Host "No built WebUI at $dist — run again with -Build." -ForegroundColor Red
    exit 1
}

# ── Ensure the daemon binary exists (build once; run the binary, not `cargo run`,
#    so the PID we track is the daemon itself, not a cargo wrapper). ──
$bin = Join-Path $root "target\debug\liberado.exe"
Write-Host "Ensuring daemon binary is built..." -ForegroundColor Cyan
& cargo build --bin liberado
if ($LASTEXITCODE -ne 0) { Write-Host "Daemon build failed." -ForegroundColor Red; exit 1 }

# ── Start the daemon detached, logging to file ──
Write-Host "Vault:  $resolvedVault" -ForegroundColor Cyan
Write-Host "Serving WebUI + API on 0.0.0.0:$port ..." -ForegroundColor Cyan
$proc = Start-Process -FilePath $bin `
    -ArgumentList @("serve", $resolvedVault) `
    -WorkingDirectory $root `
    -RedirectStandardOutput $logFile `
    -RedirectStandardError "$logFile.err" `
    -WindowStyle Hidden -PassThru
$proc.Id | Out-File -FilePath $pidFile -Encoding ascii

# ── Wait for readiness ──
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

# ── Report URLs ──
$lan = (Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue |
    Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' } |
    Select-Object -First 1 -ExpandProperty IPAddress)
Write-Host ""
Write-Host "WebUI is up (PID $($proc.Id))." -ForegroundColor Green
Write-Host "  Local: http://127.0.0.1:$port" -ForegroundColor Green
if ($lan) { Write-Host "  LAN:   http://${lan}:$port" -ForegroundColor Green }
Write-Host "  Logs:  $logFile" -ForegroundColor DarkGray
Write-Host "  Stop:  .\scripts\stop-webui.ps1" -ForegroundColor DarkGray
