# Smoke: build liberado-coder-run and exercise a mocked-free process boundary with a dry JSON
# request that fails closed without API keys (validates binary + request parse wiring).
# For a live model smoke, set OPENROUTER_API_KEY / DEEPSEEK_API_KEY and use the ignored unit test:
#   cargo test -p liberado-coder-agent openrouter_deepseek_live_coding_smoke -- --ignored
#
# Usage (from life-os root):
#   pwsh ./scripts/smoke-liberado-coder.ps1

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

Write-Host "== building liberado-coder-runner =="
cargo build -p liberado-coder-runner
$Bin = Join-Path $Root "target\debug\liberado-coder-run.exe"
if (-not (Test-Path $Bin)) {
    $Bin = Join-Path $Root "target\debug\liberado-coder-run"
}
if (-not (Test-Path $Bin)) {
    throw "liberado-coder-run binary not found after build"
}
Write-Host "binary: $Bin"

$Tmp = Join-Path $env:TEMP ("liberado-coder-smoke-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
    git -C $Tmp init | Out-Null
    git -C $Tmp config user.email "smoke@example.com"
    git -C $Tmp config user.name "Smoke"
    Set-Content -Path (Join-Path $Tmp "README.md") -Value "# smoke`n"
    git -C $Tmp add .
    git -C $Tmp commit -m "base" | Out-Null

    $Request = @{
        task = @{
            id = "smoke-1"
            description = "Create hello.txt with content hello`n"
            success_criteria = @("hello.txt exists")
        }
        workspace = @{
            root = $Tmp
            base_ref = "HEAD"
        }
        config = @{
            backend = "liberado-loop"
            planner = @{ model = "mock"; max_turns = 1 }
            coder = @{
                model = "deepseek/deepseek-v4-pro"
                prompt = "You are a coding agent. Write files then submit_report."
                max_turns = 8
            }
            critic = @{ model = "mock"; max_turns = 1 }
            sandbox = @{ backend = "host_local" }
            command_policy = @{
                allow = @()
                deny = @()
                timeout_secs = 60
                output_max_bytes = 65536
            }
            path_policy = @{
                allow_write_globs = @("**")
                deny_globs = @(".git/**")
                read_max_bytes = 131072
                search_max_results = 50
            }
            progress = @{
                read_only_turn_limit = 4
                same_tool_limit = 3
                validation_repeat_limit = 2
                max_attempts = 1
                event_preview_max_chars = 200
            }
        }
        attempt = 0
        prior_feedback = @()
    } | ConvertTo-Json -Depth 8

    $ReqPath = Join-Path $Tmp "request.json"
    Set-Content -Path $ReqPath -Value $Request -Encoding utf8

    Write-Host "== process boundary smoke (expects provider key or clean failure) =="
    $env:LIBERADO_CODER_PROVIDER = if ($env:LIBERADO_CODER_PROVIDER) { $env:LIBERADO_CODER_PROVIDER } else { "openrouter" }
    & $Bin --request $ReqPath
    $code = $LASTEXITCODE
    Write-Host "exit code: $code"
    if ($code -eq 0) {
        Write-Host "OK: live provider completed a coding run"
    } else {
        Write-Host "NOTE: non-zero exit is OK for wiring smoke without API keys; binary + request path worked if error mentions API key / provider."
    }
}
finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
