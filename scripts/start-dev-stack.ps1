# Start TurboVault + Liberado outside any agent/IDE job object so Windows doesn't
# tear them down when the parent shell exits.
#
# Usage (from repo root, in your own PowerShell window):
#   .\scripts\start-dev-stack.ps1
#   .\scripts\start-dev-stack.ps1 -Restart
#
# Vault: $env:LIBERADO_VAULT or C:\Users\Shiloh\Obsidian\Main
# Logs:  .liberado\logs\turbovault.err.log, liberado.err.log

param(
    [switch]$Restart,
    [switch]$Stop
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path $PSScriptRoot -Parent
if (-not (Test-Path (Join-Path $RepoRoot "Cargo.toml"))) {
    $RepoRoot = $PSScriptRoot
    if (-not (Test-Path (Join-Path $RepoRoot "Cargo.toml"))) {
        throw "Run from life-os repo (scripts/start-dev-stack.ps1)."
    }
}

$Vault = if ($env:LIBERADO_VAULT) { $env:LIBERADO_VAULT } else { "C:\Users\Shiloh\Obsidian\Main" }
$TvExe = Join-Path $RepoRoot "turbovault\target\release\turbovault.exe"
$LibExe = Join-Path $RepoRoot "target\debug\liberado.exe"
$LogDir = Join-Path $RepoRoot ".liberado\logs"
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null

function Stop-Stack {
    Get-Process turbovault, liberado -ErrorAction SilentlyContinue | ForEach-Object {
        Write-Host "Stopping $($_.ProcessName) pid=$($_.Id)"
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Seconds 1
}

function Start-Detached {
    param(
        [string]$FilePath,
        [string]$Arguments,
        [string]$WorkingDirectory,
        [string]$Name
    )
    if (-not (Test-Path $FilePath)) {
        throw "Missing binary: $FilePath"
    }
    # Win32_Process.Create starts outside the caller's Job Object (critical for agent shells).
    $cmd = if ($Arguments) { "`"$FilePath`" $Arguments" } else { "`"$FilePath`"" }
    $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{
        CommandLine      = $cmd
        CurrentDirectory = $WorkingDirectory
    }
    if ($r.ReturnValue -ne 0) {
        throw "Failed to start $Name (WMI ReturnValue=$($r.ReturnValue))"
    }
    Write-Host "Started $Name pid=$($r.ProcessId)"
    return $r.ProcessId
}

if ($Stop) {
    Stop-Stack
    Write-Host "Stopped."
    exit 0
}

if ($Restart) {
    Stop-Stack
}

if (-not (Test-Path $Vault)) {
    throw "Vault not found: $Vault (set LIBERADO_VAULT)"
}

# --- TurboVault (HTTP MCP on 3737) ---
$tv = Get-Process turbovault -ErrorAction SilentlyContinue
if (-not $tv) {
    if (-not (Test-Path $TvExe)) {
        throw "Build TurboVault first: cd turbovault; cargo build -p turbovault --release --features `"http,sql`""
    }
    # Redirect via cmd so WMI process has log files (Create has no redirect args).
    $tvOut = Join-Path $LogDir "turbovault.out.log"
    $tvErr = Join-Path $LogDir "turbovault.err.log"
    $tvCmd = "cmd.exe /c `"`"$TvExe`" --transport http --port 3737 --vault `"$Vault`" --init > `"$tvOut`" 2> `"$tvErr`"`""
    $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{
        CommandLine      = $tvCmd
        CurrentDirectory = (Join-Path $RepoRoot "turbovault")
    }
    if ($r.ReturnValue -ne 0) { throw "TurboVault WMI start failed: $($r.ReturnValue)" }
    Write-Host "Started turbovault via cmd pid=$($r.ProcessId) (vault=$Vault)"
    Start-Sleep -Seconds 4
} else {
    Write-Host "turbovault already running pid=$($tv.Id -join ',')"
}

# Probe MCP
try {
    $body = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"stack-probe","version":"0.1"}}}'
    $null = Invoke-WebRequest -Uri "http://127.0.0.1:3737/mcp" -Method POST -Body $body `
        -ContentType "application/json" -Headers @{ Accept = "application/json, text/event-stream" } `
        -UseBasicParsing -TimeoutSec 10
    Write-Host "TurboVault MCP http://127.0.0.1:3737/mcp OK"
} catch {
    Write-Warning "TurboVault MCP probe failed: $($_.Exception.Message)"
    Write-Warning "See $LogDir\turbovault.err.log"
}

# --- Liberado ---
$lib = Get-Process liberado -ErrorAction SilentlyContinue
if (-not $lib) {
    if (-not (Test-Path $LibExe)) {
        throw "Build liberado first: cargo build -p liberado"
    }
    $libOut = Join-Path $LogDir "liberado.out.log"
    $libErr = Join-Path $LogDir "liberado.err.log"
    $libCmd = "cmd.exe /c `"`"$LibExe`" serve `"$Vault`" > `"$libOut`" 2> `"$libErr`"`""
    $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{
        CommandLine      = $libCmd
        CurrentDirectory = $RepoRoot
    }
    if ($r.ReturnValue -ne 0) { throw "Liberado WMI start failed: $($r.ReturnValue)" }
    Write-Host "Started liberado via cmd pid=$($r.ProcessId)"
    Start-Sleep -Seconds 8
} else {
    Write-Host "liberado already running pid=$($lib.Id -join ',')"
}

try {
    $s = Invoke-RestMethod "http://127.0.0.1:4201/api/status" -TimeoutSec 10
    Write-Host "Liberado OK tools=[$($s.chat_tool_names -join ', ')] vault=$($s.vault_path)"
} catch {
    Write-Warning "Liberado status failed: $($_.Exception.Message)"
    Write-Warning "See $LogDir\liberado.err.log"
}

Write-Host ""
Write-Host "Stack ready. TUI: cargo run -p liberado-tui"
Write-Host "Stop:  .\scripts\start-dev-stack.ps1 -Stop"
