# run-tui.ps1 — Start the Liberado daemon, wait for it, then launch the TUI.
# Usage:
#   .\scripts\run-tui.ps1 [vault-path]
#   $env:LIBERADO_VAULT = "C:\path\to\vault"; .\scripts\run-tui.ps1
#   $env:DEEPSEEK_API_KEY = "sk-..."; .\scripts\run-tui.ps1

param(
    [string]$VaultPath = ""
)

$ErrorActionPreference = "Stop"
$DAEMON_PORT = 4201
$DAEMON_URL = "http://127.0.0.1:$DAEMON_PORT"
$MAX_WAIT_SECS = 60

# Resolve vault path: CLI arg > LIBERADO_VAULT env var
$resolvedVault = ""
if ($VaultPath) {
    $resolvedVault = $VaultPath
} elseif ($env:LIBERADO_VAULT) {
    $resolvedVault = $env:LIBERADO_VAULT
}

if (-not $resolvedVault) {
    Write-Host "No vault path provided. Checking topology.toml..." -ForegroundColor Yellow
    $configDir = if ($env:LIBERADO_CONFIG_DIR) { $env:LIBERADO_CONFIG_DIR } else { "$env:APPDATA\liberado" }
    $topoFile = Join-Path $configDir "topology.toml"
    if (Test-Path $topoFile) {
        $topoContent = Get-Content $topoFile -Raw
        if ($topoContent -match 'vault_path\s*=\s*"([^"]+)"') {
            $resolvedVault = $matches[1]
            Write-Host "Using vault_path from topology.toml: $resolvedVault" -ForegroundColor Green
        }
    }
    if (-not $resolvedVault) {
        Write-Host "No vault path found. Set LIBERADO_VAULT, pass a path, or configure topology.toml." -ForegroundColor Red
        exit 1
    }
}

Write-Host "Vault: $resolvedVault" -ForegroundColor Cyan
Write-Host "Daemon URL: $DAEMON_URL" -ForegroundColor Cyan

# ── Check if daemon is already running ──
try {
    $check = Invoke-WebRequest -Uri "$DAEMON_URL/api/status" -TimeoutSec 2 -ErrorAction SilentlyContinue
    if ($check.StatusCode -eq 200) {
        Write-Host "Daemon is already running at $DAEMON_URL — attaching TUI directly." -ForegroundColor Green
        $daemonRunning = $true
    }
} catch {
    $daemonRunning = $false
}

$daemonProcess = $null

if (-not $daemonRunning) {
    # Build the daemon command.
    # Pass DEEPSEEK_API_KEY through if it's set.
    $daemonArgs = @("run", "--bin", "liberado", "--", "serve", $resolvedVault)

    Write-Host "Starting daemon: cargo $daemonArgs" -ForegroundColor Cyan

    $procInfo = New-Object System.Diagnostics.ProcessStartInfo
    $procInfo.FileName = "cargo"
    $procInfo.Arguments = $daemonArgs -join ' '
    $procInfo.UseShellExecute = $false
    $procInfo.WorkingDirectory = $PSScriptRoot\..
    # stderr goes to parent console; no redirection needed
    $procInfo.RedirectStandardOutput = $false
    $procInfo.RedirectStandardError = $false
    # Forward relevant env vars
    if ($env:DEEPSEEK_API_KEY) { $procInfo.EnvironmentVariables["DEEPSEEK_API_KEY"] = $env:DEEPSEEK_API_KEY }
    if ($env:LIBERADO_PORT) { $procInfo.EnvironmentVariables["LIBERADO_PORT"] = $env:LIBERADO_PORT }
    if ($env:LIBERADO_DATA_DIR) { $procInfo.EnvironmentVariables["LIBERADO_DATA_DIR"] = $env:LIBERADO_DATA_DIR }
    if ($env:LIBERADO_CONFIG_DIR) { $procInfo.EnvironmentVariables["LIBERADO_CONFIG_DIR"] = $env:LIBERADO_CONFIG_DIR }
    if ($env:RUST_LOG) { $procInfo.EnvironmentVariables["RUST_LOG"] = $env:RUST_LOG }

    $daemonProcess = [System.Diagnostics.Process]::Start($procInfo)

    # ── Wait for the daemon to become ready ──
    Write-Host "Waiting for daemon to become ready..." -ForegroundColor Yellow
    $waited = 0
    $ready = $false
    while ($waited -lt $MAX_WAIT_SECS) {
        try {
            $resp = Invoke-WebRequest -Uri "$DAEMON_URL/api/status" -TimeoutSec 2 -ErrorAction SilentlyContinue
            if ($resp.StatusCode -eq 200) {
                $ready = $true
                break
            }
        } catch {}
        Start-Sleep -Seconds 1
        $waited++
        if ($waited % 5 -eq 0) {
            Write-Host "  Still waiting... ($waited s)" -ForegroundColor DarkGray
        }
    }

    if (-not $ready) {
        Write-Host "Daemon did not become ready within $MAX_WAIT_SECS seconds." -ForegroundColor Red
        if ($daemonProcess -and -not $daemonProcess.HasExited) {
            $daemonProcess.Kill()
        }
        exit 1
    }

    Write-Host "Daemon is ready. Launching TUI..." -ForegroundColor Green
} else {
    Write-Host "Launching TUI..." -ForegroundColor Green
}

# ── Run the TUI ──
$env:LIBERADO_SERVER = $DAEMON_URL
$tuiExit = 0
cargo run -p liberado-tui
$tuiExit = $LASTEXITCODE

# ── Cleanup ──
if ($daemonProcess -and -not $daemonProcess.HasExited) {
    Write-Host "Stopping daemon..." -ForegroundColor Yellow
    $daemonProcess.Kill()
    $daemonProcess.WaitForExit(5000) | Out-Null
    Write-Host "Daemon stopped." -ForegroundColor Green
}

exit $tuiExit
