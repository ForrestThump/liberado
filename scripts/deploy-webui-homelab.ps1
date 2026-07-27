#Requires -Version 5.1
<#
.SYNOPSIS
  Build the WASM WebUI here and ship it to the homelab daemon. No image rebuild, no restart.

.DESCRIPTION
  The WebUI is a pure browser app served by the daemon's static fallback. It is NOT baked into
  liberado:dev -- the deploy image carries no wasm32 toolchain -- so the bundle is built on this
  machine and mounted into the container from ~/homelab/services/liberado/webui.

  ServeDir reads that directory per request, so replacing its contents is the whole deploy: the
  running daemon picks up the new bundle immediately. That is why this script does not touch
  docker at all, and why it is separate from deploy/homelab/deploy.sh (which ships the *daemon*
  and takes 20-40 minutes rebuilding the image).

  Asset filenames are content-hashed, so a stale browser cache cannot mix old and new -- but
  index.html itself can be cached, hence the hard-refresh note at the end.

  Live at https://liberado.homelab.local (Traefik on the primary node -> 192.168.0.144:4201).

.PARAMETER SshHost
  SSH target running the daemon (default shiloh@homelab-node-ai).

.PARAMETER SkipBuild
  Ship the bundle already in target/ instead of rebuilding it.

.EXAMPLE
  .\scripts\deploy-webui-homelab.ps1
  .\scripts\deploy-webui-homelab.ps1 -SkipBuild
#>
[CmdletBinding()]
param(
    [string]$SshHost = "shiloh@homelab-node-ai",
    [string]$RemoteDist = "~/homelab/services/liberado/webui",
    [string]$Url = "https://liberado.homelab.local",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $root

function Write-Step([string]$msg) {
    Write-Host ""
    Write-Host "==> $msg" -ForegroundColor Cyan
}

$dist = Join-Path $root "target\dx\liberado-webui\release\web\public"

if (-not $SkipBuild) {
    # Wipe the output dir first: `dx build` writes content-hashed asset names but never removes the
    # previous ones, so successive builds pile up. Every stale .wasm then rides along in the tarball
    # and onto the box -- unreferenced by index.html, but doubling the payload each time.
    #
    # It also turns a failed build into an honest failure: with the dir gone there is no leftover
    # index.html, so the check below fires instead of silently reshipping the last good bundle.
    if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }

    Write-Step "Build WebUI (release wasm)"
    # `dx` must run under the rustup-managed cargo: the standalone MSI Rust that shadows it on PATH
    # has no wasm32 std, and the failure is the misleading "can't find crate for `core`".
    $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
    & dx build -r -p liberado-webui --web
    # dx exits 0 even when the cargo build inside it fails, so trust the artifact, not $LASTEXITCODE.
}

$index = Join-Path $dist "index.html"
if (-not (Test-Path $index)) {
    throw "No bundle at $dist - the build failed. Read the dx output above for the cargo errors."
}

Write-Step "Pack bundle"
$tgz = Join-Path $env:TEMP "liberado-webui-dist.tgz"
if (Test-Path $tgz) { Remove-Item $tgz -Force }
# Must be the Windows bsdtar, NOT whichever tar is first on PATH. Git for Windows ships a GNU tar
# that reads "C:\..." as a remote host spec ("Cannot connect to C: resolve failed") -- and the
# rustup/cargo PATH prepend above makes which one wins depend on the machine.
$tarExe = Join-Path $env:SystemRoot "System32\tar.exe"
if (-not (Test-Path $tarExe)) { $tarExe = "tar" }
& $tarExe -czf $tgz -C $dist .
if ($LASTEXITCODE -ne 0 -or -not (Test-Path $tgz)) { throw "tar pack failed (exit $LASTEXITCODE)" }
$sizeKb = [math]::Round((Get-Item $tgz).Length / 1KB)
Write-Host "  $tgz ($sizeKb KB gzipped)" -ForegroundColor DarkGray

Write-Step "Upload to ${SshHost}:$RemoteDist"
& scp -o BatchMode=yes -o ConnectTimeout=15 $tgz "${SshHost}:~/liberado-webui-dist.tgz"
if ($LASTEXITCODE -ne 0) { throw "scp failed (exit $LASTEXITCODE)" }
Remove-Item $tgz -Force -ErrorAction SilentlyContinue

# Unpack to a staging dir first, so a half-extracted tree is never the one being served, then
# replace the CONTENTS of the mounted dir.
#
# Do NOT `mv` the directory itself, however tempting the atomicity is: the container bind-mounts
# this path and Docker resolves the bind to an inode at container start. Renaming the directory
# leaves the container mounted on the old, now-unlinked inode -- the daemon goes on serving a tree
# nobody can update, or 404s. (Learned the hard way, 2026-07-26.) rsync --delete keeps the inode
# and still removes assets dropped from the new build.
$remote = @"
set -euo pipefail
mkdir -p $RemoteDist
rm -rf $RemoteDist.incoming
mkdir -p $RemoteDist.incoming
tar xzf ~/liberado-webui-dist.tgz -C $RemoteDist.incoming
test -f $RemoteDist.incoming/index.html
rsync -a --delete $RemoteDist.incoming/ $RemoteDist/
rm -rf $RemoteDist.incoming
rm -f ~/liberado-webui-dist.tgz
echo swap-ok
"@
Write-Step "Refresh bundle contents in place (keeps the bind-mount inode)"
& ssh -o BatchMode=yes -o ConnectTimeout=15 $SshHost "bash -lc '$($remote -replace "'", "'\''")'"
if ($LASTEXITCODE -ne 0) { throw "remote swap failed (exit $LASTEXITCODE)" }

Write-Step "Verify"
try {
    # -SkipCertificateCheck does not exist on PS 5.1; the homelab CA is not in the Windows store,
    # so trust every cert for the life of this process only.
    Add-Type @"
using System.Net;
using System.Security.Cryptography.X509Certificates;
public class TrustHomelabCert : ICertificatePolicy {
    public bool CheckValidationResult(ServicePoint sp, X509Certificate c, WebRequest r, int p) { return true; }
}
"@ -ErrorAction SilentlyContinue
    [System.Net.ServicePointManager]::CertificatePolicy = New-Object TrustHomelabCert
    [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12

    $page = Invoke-WebRequest -Uri "$Url/" -TimeoutSec 20
    $api = Invoke-WebRequest -Uri "$Url/api/status" -TimeoutSec 20
    Write-Host "  GET $Url/            -> $($page.StatusCode)" -ForegroundColor Green
    Write-Host "  GET $Url/api/status  -> $($api.StatusCode)" -ForegroundColor Green
    Write-Host ""
    Write-Host "WebUI deployed: $Url" -ForegroundColor Green
    Write-Host "  Hard-refresh (Ctrl+Shift+R) if the page looks unchanged - index.html can be cached." -ForegroundColor DarkGray
} catch {
    Write-Host "Verify failed: $_" -ForegroundColor Yellow
    Write-Host "  Bundle shipped, but the site did not answer. Check: ssh $SshHost 'docker logs liberado --tail 40'" -ForegroundColor Yellow
    exit 2
}
