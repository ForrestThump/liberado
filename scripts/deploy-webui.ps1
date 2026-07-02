param([switch]$Background, [string]$VaultPath)
$ErrorActionPreference = "Continue"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$stateDir = Join-Path $root ".liberado"
$pidFile  = Join-Path $stateDir "webui.pid"
$deployLog = Join-Path $stateDir "deploy.log"
New-Item -ItemType Directory -Force -Path $stateDir | Out-Null

$vault = if ($VaultPath) { $VaultPath } else { $env:LIBERADO_VAULT }
if (-not $vault) {
    $configDir = if ($env:LIBERADO_CONFIG_DIR) { $env:LIBERADO_CONFIG_DIR } else { "$env:APPDATA\liberado" }
    $topoFile = Join-Path $configDir "topology.toml"
    if (Test-Path $topoFile) {
        if ((Get-Content $topoFile -Raw) -match 'vault_path\s*=\s*"([^"]+)"') { $vault = $matches[1] }
    }
}

if (-not $Background) {
    if (-not $vault) {
        Write-Host "No vault path. Set LIBERADO_VAULT or pass -VaultPath." -ForegroundColor Red
        exit 1
    }
    if (Test-Path $pidFile) {
        try { $oldId = Get-Content $pidFile | Select-Object -First 1; Stop-Process -Id $oldId -Force -ErrorAction SilentlyContinue } catch {}
        Remove-Item $pidFile -Force -ErrorAction SilentlyContinue
    }
    "" | Out-File $deployLog -Encoding ASCII
    $myScript = $MyInvocation.MyCommand.Path
    Start-Process -FilePath "powershell.exe" `
        -ArgumentList @("-NoProfile", "-WindowStyle", "Hidden", "-File", $myScript, "-Background", "-VaultPath", $vault) `
        -WindowStyle Hidden
    Write-Host "Deploy started in background." -ForegroundColor Cyan
    Write-Host "  Log: $deployLog" -ForegroundColor DarkGray
    exit 0
}

$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
Set-Location $root

function Log($msg) {
    $line = "$(Get-Date -Format 'HH:mm:ss') $msg"
    $line | Out-File $deployLog -Append -Encoding ASCII
}

Log "=== Deploy started ==="
Log "Vault: $vault"
Log "Root: $root"

if (Test-Path $pidFile) {
    try {
        $oldId = Get-Content $pidFile | Select-Object -First 1
        Stop-Process -Id $oldId -Force -ErrorAction SilentlyContinue
        Log "Stopped old daemon PID $oldId"
    } catch {}
    Remove-Item $pidFile -Force -ErrorAction SilentlyContinue
}

Log "Building daemon..."
try {
    $result = cmd /c "cargo build --bin liberado 2>&1"
    $result | Out-File $deployLog -Append -Encoding ASCII
    if ($LASTEXITCODE -ne 0) { Log "ERROR: daemon build failed (exit $LASTEXITCODE)"; exit 1 }
} catch {
    $err = $_.Exception.Message
    Log "ERROR: daemon build crashed: $err"
    exit 1
}

Log "Building WebUI WASM..."
try {
    $result = cmd /c "dx build -p liberado-webui --web 2>&1"
    $result | Out-File $deployLog -Append -Encoding ASCII
    if ($LASTEXITCODE -ne 0) { Log "ERROR: WASM build failed (exit $LASTEXITCODE)"; exit 1 }
} catch {
    $err = $_.Exception.Message
    Log "ERROR: WASM build crashed: $err"
    exit 1
}

$src  = Join-Path $root "target\dx\liberado-webui\debug\web\public"
$dest = Join-Path $root "target\dx\liberado-webui\release\web\public"
New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item "$src\*" -Destination $dest -Recurse -Force -ErrorAction SilentlyContinue

$bin = Join-Path $root "target\debug\liberado.exe"
$webuiLog = Join-Path $stateDir "webui.log"
$port = if ($env:LIBERADO_PORT) { $env:LIBERADO_PORT } else { "4201" }

Log "Starting daemon..."
$proc = Start-Process -FilePath $bin `
    -ArgumentList @("serve", $vault) `
    -WorkingDirectory $root `
    -RedirectStandardOutput $webuiLog `
    -RedirectStandardError "$webuiLog.err" `
    -WindowStyle Hidden -PassThru
$proc.Id | Out-File -FilePath $pidFile -Encoding ascii

Log "Waiting for daemon (max 30s)..."
for ($i = 0; $i -lt 30; $i++) {
    try {
        $r = Invoke-WebRequest -Uri "http://127.0.0.1:$port/api/status" -TimeoutSec 2 -ErrorAction SilentlyContinue
        if ($r.StatusCode -eq 200) {
            Log "SUCCESS: daemon ready at http://127.0.0.1:$port (PID $($proc.Id))"
            exit 0
        }
    } catch {}
    if ($proc.HasExited) { Log "ERROR: daemon exited early"; exit 1 }
    Start-Sleep -Seconds 1
}
Log "ERROR: daemon not ready within 30s"
exit 1
