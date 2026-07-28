#Requires -Version 5.1
<#
.SYNOPSIS
  One-shot: stream Liberado source to the homelab, docker-build liberado:dev, recreate the
  container, optionally sync deploy config, and health-check the API.

.DESCRIPTION
  The homelab build tree (~/liberado-build) is NOT a git clone — Windows is the source of truth.
  This script packs a lean context (Cargo.toml/lock, Dockerfile, crates, config, turbomcp when
  present), scp's it, extracts while preserving the remote turbovault/ tree if already cloned,
  builds the image on the box, force-recreates the compose service, and curls /api/status.

  Sibling turbovault on the host is left alone unless -RefreshTurbovault is set (then develop
  is re-cloned). turbomcp is re-sent from this machine when the local path exists.

.PARAMETER Host
  SSH target (default shiloh@homelab-node-ai).

.PARAMETER SkipBuild
  Only sync config + recreate container (use after a successful image build).

.PARAMETER SkipConfig
  Do not scp deploy/homelab/config/* onto the service config dir.

.PARAMETER RefreshTurbovault
  Re-clone turbovault develop into liberado-build/turbovault before build.

.PARAMETER WaitMinutes
  Max minutes to wait for docker build (default 45).

.EXAMPLE
  .\scripts\deploy-homelab.ps1
  .\scripts\deploy-homelab.ps1 -SkipBuild   # config + recreate only
#>
[CmdletBinding()]
param(
    [string]$SshHost = "shiloh@homelab-node-ai",
    [string]$RemoteBuild = "~/liberado-build",
    [string]$RemoteService = "~/homelab/services/liberado",
    [string]$Image = "liberado:dev",
    [string]$ApiUrl = "http://192.168.0.144:4201",
    [switch]$SkipBuild,
    [switch]$SkipConfig,
    [switch]$RefreshTurbovault,
    [int]$WaitMinutes = 45
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $root

function Write-Step([string]$msg) {
    Write-Host ""
    Write-Host "==> $msg" -ForegroundColor Cyan
}

# Run a native executable and return its exit code, WITHOUT letting anything it prints to stderr
# abort the script.
#
# Windows PowerShell 5.1 turns each stderr line from a native command into an ErrorRecord, and with
# `$ErrorActionPreference = "Stop"` that record terminates — so a command that succeeded, exit code
# 0 and all, kills the run because it was chatty. `docker compose` announces "Container liberado
# Recreate" on stderr, which meant every deploy died immediately after recreating the container,
# leaving it in `Created` and never started. Two deploys in a row had to be finished by hand.
#
# `$ErrorActionPreference` set here is function-scoped and restored on return, so cmdlet errors
# everywhere else in the script still stop the run. Exit codes remain the only success signal for
# native commands, which is what they were supposed to be all along.
#
# Args are passed as an array rather than a scriptblock on purpose: a scriptblock would resolve its
# variables against whatever scope happened to be on the call stack, which is exactly the kind of
# spooky action this function exists to remove.
# Output goes straight to the host rather than down the pipeline, so the caller's `$code` is the
# exit code and nothing else. Returning both meant `$code` was an array whose first elements were
# the remote command's stdout, and every `-ne 0` comparison against it was true — the preflight
# failed with "ssh failed (exit ok 26.1.5+dfsg1 compose-ok 0)", which at least named its own bug.
function Invoke-Native([string]$Exe, [string[]]$Arguments) {
    $ErrorActionPreference = "Continue"
    & $Exe @Arguments | Out-Host
    return $LASTEXITCODE
}

function Invoke-Ssh([string]$cmd) {
    $code = Invoke-Native "ssh" @("-o", "BatchMode=yes", "-o", "ConnectTimeout=15", $SshHost, $cmd)
    if ($code -ne 0) {
        throw "ssh failed (exit $code): $cmd"
    }
}

function Invoke-SshAllowFail([string]$cmd) {
    return Invoke-Native "ssh" @("-o", "BatchMode=yes", "-o", "ConnectTimeout=15", $SshHost, $cmd)
}

# Docker's own word for what the container is doing: `running`, `created`, `exited`, or `missing`
# when there is no such container.
# Not routed through `Invoke-Native`: this is the one call whose *output* is the answer, and that
# helper deliberately sends output to the host. Same local `$ErrorActionPreference` trick.
function Get-ContainerState {
    $ErrorActionPreference = "Continue"
    $out = & ssh -o BatchMode=yes -o ConnectTimeout=15 $SshHost `
        "docker inspect -f '{{.State.Status}}' liberado 2>/dev/null || echo missing"
    $lines = @($out)
    if ($lines.Count -eq 0) { return "unknown" }
    return ([string]$lines[-1]).Trim()
}

function ConvertTo-SshSingleQuoted([string]$script) {
    # Wrap a multi-line bash script for: ssh host "bash -lc '…'"
    $escaped = $script -replace "'", "'\''"
    return "'$escaped'"
}

# --- 0. Preflight ---
Write-Step "Preflight SSH $SshHost"
Invoke-Ssh "echo ok && docker version --format '{{.Server.Version}}' && test -f $RemoteService/docker-compose.yml && echo compose-ok"

if (-not $SkipBuild) {
    # --- 1. Pack lean source (tar on Windows needs a temp dir; use .tgz via tar if available) ---
    Write-Step "Pack lean build context from $root"
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $tgz = Join-Path $env:TEMP "liberado-src-$stamp.tgz"
    if (Test-Path $tgz) { Remove-Item $tgz -Force }

    # Prefer system tar (Windows 10+). Exclude host targets and huge trees.
    $packList = @(
        "Cargo.toml",
        "Cargo.lock",
        "Dockerfile",
        ".dockerignore",
        "crates",
        "config",
        "prompts"
    )
    foreach ($p in $packList) {
        if (-not (Test-Path (Join-Path $root $p))) {
            if ($p -eq "prompts") { continue }
            throw "Missing required path for pack: $p"
        }
    }
    if (Test-Path (Join-Path $root "turbomcp")) {
        $packList += "turbomcp"
    }

    # GNU/Windows tar: -czf archive -C root paths; exclude target dirs inside members.
    $tarArgs = @(
        "-czf", $tgz,
        "--exclude=target",
        "--exclude=*/target",
        "--exclude=**/target",
        "--exclude=.git",
        "--exclude=*/.git",
        "-C", $root
    ) + $packList
    $code = Invoke-Native "tar" $tarArgs
    if ($code -ne 0 -or -not (Test-Path $tgz)) {
        throw "tar pack failed (exit $code)"
    }
    $sizeMb = [math]::Round((Get-Item $tgz).Length / 1MB, 1)
    Write-Host "  packed $tgz ($sizeMb MB)" -ForegroundColor DarkGray

    # --- 2. Upload ---
    Write-Step "Upload to ${SshHost}:~/liberado-src.tgz"
    $code = Invoke-Native "scp" @("-o", "BatchMode=yes", "-o", "ConnectTimeout=15", $tgz, "${SshHost}:~/liberado-src.tgz")
    if ($code -ne 0) { throw "scp failed (exit $code)" }
    Remove-Item $tgz -Force -ErrorAction SilentlyContinue

    # --- 3. Extract on host (preserve turbovault unless refresh) ---
    Write-Step "Extract into $RemoteBuild (preserve remote turbovault unless -RefreshTurbovault)"
    $refreshTv = if ($RefreshTurbovault) { "1" } else { "0" }
    $extract = @"
set -euo pipefail
mkdir -p $RemoteBuild
cd $RemoteBuild
if [ -d turbovault ] && [ '$refreshTv' != '1' ]; then
  rm -rf /tmp/liberado-turbovault-preserve
  mv turbovault /tmp/liberado-turbovault-preserve
fi
rm -rf crates config prompts turbomcp Dockerfile .dockerignore Cargo.toml Cargo.lock 2>/dev/null || true
tar xzf ~/liberado-src.tgz -C $RemoteBuild
if [ -d /tmp/liberado-turbovault-preserve ]; then
  rm -rf turbovault
  mv /tmp/liberado-turbovault-preserve turbovault
  echo restored preserved turbovault
fi
if [ '$refreshTv' = '1' ] || [ ! -d turbovault ]; then
  rm -rf turbovault
  git clone --depth 1 -b develop git@github.com:ForrestThump/turbovault.git turbovault
  echo cloned turbovault develop
fi
test -f Cargo.toml && test -d crates && test -d turbovault && test -d turbomcp
echo extract-ok
"@
    Invoke-Ssh "bash -lc $(ConvertTo-SshSingleQuoted $extract)"

    # --- 4. Docker build (foreground; long) ---
    Write-Step "docker build -t $Image (on homelab; may take a while)"
    $buildCmd = @"
set -euo pipefail
cd $RemoteBuild
: > ~/liberado-build.log
docker build -t $Image . 2>&1 | tee ~/liberado-build.log
"@
    # `docker build` writes its entire progress stream to stderr, so this is the call that most
    # obviously needs `Invoke-Native` — it was only ever surviving because the build's own `tee`
    # merged the streams first.
    $code = Invoke-Native "ssh" @(
        "-o", "BatchMode=yes", "-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=30",
        $SshHost, "bash -lc $(ConvertTo-SshSingleQuoted $buildCmd)"
    )
    if ($code -ne 0) {
        Write-Host "Build failed. Last log lines:" -ForegroundColor Red
        $null = Invoke-SshAllowFail "tail -n 40 ~/liberado-build.log"
        throw "docker build failed (exit $code)"
    }
    Write-Host "  image $Image built" -ForegroundColor Green
}

# --- 5. Sync deploy config (topology timezone etc.) ---
if (-not $SkipConfig) {
    Write-Step "Sync deploy/homelab/config → ${SshHost}:$RemoteService/config"
    $cfgLocal = Join-Path $root "deploy\homelab\config"
    if (-not (Test-Path $cfgLocal)) { throw "missing $cfgLocal" }
    $code = Invoke-Native "scp" @(
        "-o", "BatchMode=yes",
        (Join-Path $cfgLocal "topology.toml"),
        (Join-Path $cfgLocal "policy.toml"),
        "${SshHost}:${RemoteService}/config/"
    )
    if ($code -ne 0) { throw "config scp failed (exit $code)" }

    $composeLocal = Join-Path $root "deploy\homelab\docker-compose.yml"
    if (Test-Path $composeLocal) {
        $code = Invoke-Native "scp" @("-o", "BatchMode=yes", $composeLocal, "${SshHost}:${RemoteService}/docker-compose.yml")
        if ($code -ne 0) { throw "compose scp failed (exit $code)" }
    }
}

# --- 6. Recreate service ---
Write-Step "Recreate liberado container"
Invoke-Ssh "docker compose -f $RemoteService/docker-compose.yml up -d --force-recreate"
Start-Sleep -Seconds 5

# `up -d` returning 0 is not the same as the container running: a compose run interrupted between
# `Recreated` and `Started` leaves it in `Created`, which looks like nothing happened. That is
# exactly the state two aborted deploys left behind, and nothing here noticed — the health check
# below then reported a perfectly healthy API belonging to the *previous* container and called the
# deploy a success. Assert the state rather than infer it from an exit code.
#
# NOTE: keep string literals in this file ASCII. It is UTF-8 with no BOM, so Windows PowerShell 5.1
# decodes it as the ANSI codepage — and an em dash's third UTF-8 byte (0x94) lands on CP1252's
# closing smart double-quote, which PS accepts as a string delimiter. One em dash inside a
# double-quoted string therefore ends that string early and swallows everything up to the next
# quote, braces included. Comments are safe (they end at the newline); strings are not.
if ((Get-ContainerState) -ne "running") {
    $state = Get-ContainerState
    Write-Host "  container is '$state', not running - starting it" -ForegroundColor Yellow
    Invoke-Ssh "docker compose -f $RemoteService/docker-compose.yml up -d"
    Start-Sleep -Seconds 8
    $state = Get-ContainerState
    if ($state -ne "running") {
        throw "liberado container is '$state' after recreate - check: ssh $SshHost 'docker logs liberado --tail 80'"
    }
}
Write-Host "  container running" -ForegroundColor Green

# --- 7. Health ---
Write-Step "Health checks"
Invoke-Ssh "docker ps --filter name=^liberado`$ --format 'table {{.Names}}\t{{.Status}}\t{{.Image}}'; docker logs liberado --tail 30"
Write-Host ""
Write-Host "GET $ApiUrl/api/status" -ForegroundColor DarkGray
try {
    $status = Invoke-RestMethod -Uri "$ApiUrl/api/status" -TimeoutSec 15
    $status | ConvertTo-Json -Depth 6 | Write-Host
    Write-Host ""
    Write-Host "Deploy OK" -ForegroundColor Green
} catch {
    Write-Host "API health failed: $_" -ForegroundColor Yellow
    Write-Host "Container may still be starting; check: ssh $SshHost 'docker logs liberado --tail 80'" -ForegroundColor Yellow
    exit 2
}
