# Install liberado-acp and register it as a Paseo ACP provider (Windows).
#
# Usage (from life-os repo root):
#   powershell -ExecutionPolicy Bypass -File scripts/install-paseo-liberado.ps1
#
# What it does:
#   1. cargo install --path crates/acp-bridge --force  → liberado-acp on PATH
#   2. Merge Liberado into %USERPROFILE%\.paseo\config.json (or $env:PASEO_HOME)
#   3. Smoke-test initialize over stdio

$ErrorActionPreference = "Stop"

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $repoRoot

Write-Host "==> Building + installing liberado-acp"
cargo install --path crates/acp-bridge --force
if ($LASTEXITCODE -ne 0) { throw "cargo install failed" }

$acp = Get-Command liberado-acp -ErrorAction SilentlyContinue
if (-not $acp) {
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin\liberado-acp.exe"
    if (Test-Path $cargoBin) {
        Write-Host "liberado-acp is at $cargoBin (ensure ~/.cargo/bin is on PATH)"
        $command = @($cargoBin)
    } else {
        throw "liberado-acp not found after install"
    }
} else {
    Write-Host "liberado-acp: $($acp.Source)"
    $command = @("liberado-acp")
}

$paseoHome = if ($env:PASEO_HOME) { $env:PASEO_HOME } else { Join-Path $env:USERPROFILE ".paseo" }
New-Item -ItemType Directory -Force -Path $paseoHome | Out-Null
$configPath = Join-Path $paseoHome "config.json"

# Do NOT embed API keys in config.json — Paseo inherits the launching process env for
# spawned agents. Keep only non-secret model selection here.
$provider = [ordered]@{
    extends      = "acp"
    label        = "Liberado"
    description  = "Liberado coding agent over ACP (liberado-acp)"
    command      = $command
    env          = [ordered]@{
        LIBERADO_ACP_MODEL = if ($env:LIBERADO_ACP_MODEL) { $env:LIBERADO_ACP_MODEL } else { "deepseek-chat" }
    }
    params       = [ordered]@{
        supportsMcpServers = $false
    }
}

if (Test-Path $configPath) {
    Write-Host "==> Merging into existing $configPath"
    $raw = Get-Content $configPath -Raw -Encoding utf8
    $cfg = if ($raw.Trim()) { $raw | ConvertFrom-Json } else { [pscustomobject]@{} }
} else {
    Write-Host "==> Creating $configPath"
    $cfg = [pscustomobject]@{}
}

# Ensure nested objects exist (PowerShell-friendly).
if (-not $cfg.agents) {
    $cfg | Add-Member -NotePropertyName agents -NotePropertyValue ([pscustomobject]@{}) -Force
}
if (-not $cfg.agents.providers) {
    $cfg.agents | Add-Member -NotePropertyName providers -NotePropertyValue ([pscustomobject]@{}) -Force
}

# Convert provider hashtable to PSCustomObject for JSON.
$providerObj = [pscustomobject]$provider
$providerObj.env = [pscustomobject]$provider.env
$providerObj.params = [pscustomobject]$provider.params
$providerObj.command = @($command)

$cfg.agents.providers | Add-Member -NotePropertyName liberado -NotePropertyValue $providerObj -Force

$json = $cfg | ConvertTo-Json -Depth 12
# utf8NoBOM so parsers that reject BOM (Python json, some tools) stay happy.
$utf8NoBom = New-Object System.Text.UTF8Encoding $false
[System.IO.File]::WriteAllText($configPath, $json, $utf8NoBom)
Write-Host "Wrote provider 'liberado' to $configPath"

Write-Host "==> Smoke test (initialize)"
$init = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientInfo":{"name":"install-script","version":"0"},"clientCapabilities":{}}}'
$out = $init | & liberado-acp 2>$null
if ($out -notmatch 'Liberado') {
    Write-Warning "Smoke output unexpected: $out"
} else {
    Write-Host "OK: $out"
}

Write-Host ""
Write-Host "Done. Restart Paseo and pick provider 'Liberado'."
Write-Host "Docs: docs/impl/paseo-integration.md"
if (-not $env:DEEPSEEK_API_KEY -and -not $env:OPENROUTER_API_KEY -and -not $env:OPENAI_API_KEY) {
    Write-Warning "No LLM API key in this shell. Set DEEPSEEK_API_KEY (or peer) before prompting."
}
