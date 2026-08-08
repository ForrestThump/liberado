# Regenerate docs/spec/reference/crate-map.md from the crate manifests.
#
# Single source of truth: each crate's Cargo.toml provides `description` and
# `[package.metadata.liberado] role`. The same role tags drive the layer-rules test
# (crates/test-support/tests/layer_rules.rs), so this map cannot drift from the graph
# without CI noticing the graph side, and re-running this script fixes the doc side.
#
# Usage:  powershell -File scripts/gen-crate-map.ps1

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$cratesDir = Join-Path $root 'crates'
# Must match where the map actually lives. This said 'docs/reference/' while the file
# was at 'docs/spec/reference/', so running the script created a second, stray copy instead
# of refreshing the real one -- and the real one silently went stale (43 crates vs 46).
$outPath = Join-Path $root 'docs/spec/reference/crate-map.md'

$roleOrder = @('foundation', 'client', 'kernel', 'store', 'pack', 'service', 'surface', 'root', 'tooling', 'testing')
$roleBlurbs = @{
    foundation = 'The bottom layer: vocabulary and narrow-waist traits. Depends on nothing above itself.'
    client     = 'Front-end building blocks, liftable into any UI without dragging the system along.'
    kernel     = 'The orchestration engine: decide/act loops, sessions, capability plumbing.'
    store      = 'Persistent and shared information: vault, conversations, memory, search.'
    pack       = 'Domain packs (coding first). Never sit beneath kernel/config/store layers.'
    service    = 'Out-of-process adapters: MCP servers, bots, the forge.'
    surface    = 'UIs. Clients of the wire contract only - enforced by layer_rules.rs.'
    root       = 'Composition roots: the only crates allowed to see everything.'
    tooling    = 'Meta tooling (evals, heuristics tuner). Not build dependencies of the system.'
    testing    = 'Dev-dependency-only test support.'
}

$crates = @()
Get-ChildItem $cratesDir -Directory | ForEach-Object {
    $manifest = Join-Path $_.FullName 'Cargo.toml'
    if (-not (Test-Path $manifest)) { return }
    $name = ''; $desc = ''; $role = ''; $deps = @(); $section = ''
    foreach ($line in (Get-Content $manifest -Encoding UTF8)) {
        if ($line -match '^\[(.+)\]\s*$') { $section = $Matches[1]; continue }
        if ($section -eq 'package') {
            if ($line -match '^name\s*=\s*"(.+)"') { $name = $Matches[1] }
            if ($line -match '^description\s*=\s*"(.+)"') { $desc = $Matches[1] }
        }
        if ($section -eq 'package.metadata.liberado' -and $line -match '^role\s*=\s*"(.+)"') { $role = $Matches[1] }
        if ($section -eq 'dependencies' -and $line -match '^(liberado-[a-z0-9-]+|chat-client-contract)\s*=') {
            $deps += $Matches[1]
        }
    }
    if ($name) {
        $crates += [pscustomobject]@{ Name = $name; Dir = $_.Name; Desc = $desc; Role = $role; Deps = $deps }
    }
}

$sb = New-Object System.Text.StringBuilder
[void]$sb.AppendLine('# Crate map')
[void]$sb.AppendLine()
[void]$sb.AppendLine('> **Generated file - do not edit.** Regenerate with `powershell -File scripts/gen-crate-map.ps1`.')
[void]$sb.AppendLine('> Source of truth: each crate''s `Cargo.toml` (`description` + `[package.metadata.liberado] role`).')
[void]$sb.AppendLine('> Layer semantics and dependency rules: [contracts.md](../architecture/contracts.md) and')
[void]$sb.AppendLine('> `crates/test-support/tests/layer_rules.rs` (the same role tags, mechanically enforced).')
[void]$sb.AppendLine()
[void]$sb.AppendLine("$($crates.Count) workspace crates as of $(Get-Date -Format 'yyyy-MM-dd').")

foreach ($role in $roleOrder) {
    $group = @($crates | Where-Object { $_.Role -eq $role } | Sort-Object Name)
    if ($group.Count -eq 0) { continue }
    [void]$sb.AppendLine()
    [void]$sb.AppendLine("## $role")
    [void]$sb.AppendLine()
    [void]$sb.AppendLine($roleBlurbs[$role])
    [void]$sb.AppendLine()
    [void]$sb.AppendLine('| Crate | Internal deps | Description |')
    [void]$sb.AppendLine('|---|---|---|')
    foreach ($c in $group) {
        $depsStr = if ($c.Deps.Count -gt 0) { ($c.Deps | ForEach-Object { '`{0}`' -f $_ }) -join ', ' } else { '*none*' }
        $descStr = if ($c.Desc) { $c.Desc } else { '*(no description in Cargo.toml)*' }
        [void]$sb.AppendLine(('| [`{0}`](../../../crates/{1}/) | {2} | {3} |' -f $c.Name, $c.Dir, $depsStr, $descStr))
    }
}

$untagged = @($crates | Where-Object { -not $_.Role })
if ($untagged.Count -gt 0) {
    [void]$sb.AppendLine()
    [void]$sb.AppendLine('## ⚠ untagged (fix these — layer_rules.rs will fail)')
    foreach ($c in $untagged) { [void]$sb.AppendLine("- $($c.Name)") }
}

[System.IO.File]::WriteAllText($outPath, $sb.ToString().Replace("`r`n", "`n"), (New-Object System.Text.UTF8Encoding $false))
Write-Host "Wrote $outPath ($($crates.Count) crates)"
