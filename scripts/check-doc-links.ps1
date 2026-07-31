# check-doc-links.ps1 — regex-based markdown link checker for docs/.
#
# Extracts `[text](target)` links from every *.md file under the docs root
# (default: <repo>/docs) using the same simple regex the docs audit used, and
# verifies that every relative target resolves to a real file on disk,
# resolved from the directory of the file that links to it.
#
# Skipped (never requires network access):
#   - http://, https://, and other external/protocol URLs
#   - .secret files (both as files to lint and as link targets)
#   - anchor-only links (#section) and links inside code blocks / code spans
#
# Exits with a non-zero status if any link is broken, so it can gate CI.
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check-doc-links.ps1
#   just check-links
#   (CI: .github/workflows/ci.yml -> "doc-links" job)

param(
    # Root directory to walk for markdown files. Defaults to <repo>/docs.
    [string]$DocsRoot = (Join-Path (Split-Path -Parent $PSScriptRoot) 'docs')
)

$ErrorActionPreference = 'Stop'

# [text](target) — capture group 1 is the target. Same shape as the audit regex.
$LinkRegex = '\[[^\]\r\n]*\]\(([^\r\n)]+)\)'
# Links that need network access (or are not file paths) are never checked.
$ExternalLinkRegex = '^(https?://|//|mailto:|ftp:|tel:|data:|news:|javascript:)'

function Test-LinkTarget {
    param([string]$Path)
    if (Test-Path -LiteralPath $Path -PathType Any) { return $true }
    # Case-insensitive fallback: catches case-only mismatches on the
    # case-sensitive filesystems used by Linux/macOS CI.
    $leaf = [System.IO.Path]::GetFileName($Path)
    $parent = [System.IO.Path]::GetDirectoryName($Path)
    if ([string]::IsNullOrEmpty($leaf)) { return $false }
    if (-not (Test-Path -LiteralPath $parent -PathType Container)) { return $false }
    return @(Get-ChildItem -LiteralPath $parent -Force | Where-Object { $_.Name -ieq $leaf }).Count -gt 0
}

function ConvertTo-RepoRelative {
    param([string]$FullPath, [string]$RepoRoot)
    if ($FullPath.StartsWith($RepoRoot)) {
        return $FullPath.Substring($RepoRoot.Length).TrimStart('/', '\')
    }
    return $FullPath
}

$docsDir = [System.IO.Path]::GetFullPath($DocsRoot)
if (-not (Test-Path -LiteralPath $docsDir -PathType Container)) {
    Write-Error "Docs root not found: $docsDir"
    exit 2
}

$repoRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))

# Only *.md files; .secret files are never linted.
$files = @(Get-ChildItem -LiteralPath $docsDir -Recurse -File | Where-Object {
    $_.Extension -ieq '.md' -and $_.Name -notmatch '\.secret$'
} | Sort-Object FullName)

$broken = [System.Collections.Generic.List[string]]::new()
$linkCount = 0

foreach ($file in $files) {
    $lines = @(Get-Content -LiteralPath $file.FullName -Encoding UTF8)
    $inFence = $null
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        $lineNo = $i + 1

        # Skip fenced code blocks (``` / ~~~) — sample links inside them are
        # examples, not real links.
        if ($inFence) {
            if ($line -match '^\s*(```|~~~)\s*$') { $inFence = $null }
            continue
        }
        if ($line -match '^\s*(```|~~~)') { $inFence = $Matches[1]; continue }

        # Strip inline code spans and HTML comments so doc examples don't count.
        $stripped = [regex]::Replace($line, '`[^`\r\n]*`', '')
        $stripped = [regex]::Replace($stripped, '(?s)<!--.*?-->', '')

        foreach ($m in [regex]::Matches($stripped, $LinkRegex)) {
            $target = $m.Groups[1].Value

            # Drop an optional title: [text](target "title") or [text](target 'title').
            $target = $target -replace '\s+("[^"]*"|''[^'']*'')\s*$', ''
            $target = $target.Trim()
            # Angle-bracket form: [text](<target>).
            if ($target.StartsWith('<') -and $target.EndsWith('>')) {
                $target = $target.Substring(1, $target.Length - 2).Trim()
            }
            if ($target -eq '') { continue }

            # External URLs, protocol-relative URLs, and non-file schemes.
            if ($target -match $ExternalLinkRegex) { continue }
            # Anchor-only links within the same file.
            if ($target.StartsWith('#')) { continue }

            # Split off any #fragment; only the path part must exist.
            $hashIndex = $target.IndexOf('#')
            if ($hashIndex -ge 0) { $target = $target.Substring(0, $hashIndex) }
            $target = $target.Trim()
            if ($target -eq '') { continue }
            # Never verify links into .secret files.
            if ($target -match '\.secret$') { continue }

            $resolved = [System.IO.Path]::GetFullPath((Join-Path $file.DirectoryName $target))
            $linkCount++
            if (-not (Test-LinkTarget $resolved)) {
                $relFile = ConvertTo-RepoRelative $file.FullName $repoRoot
                $relResolved = ConvertTo-RepoRelative $resolved $repoRoot
                $broken.Add(('{0}:{1}: broken link `{2}` (resolves to {3})' -f $relFile, $lineNo, $target, $relResolved))
            }
        }
    }
}

$relDocs = ConvertTo-RepoRelative $docsDir $repoRoot
Write-Host ''
Write-Host "Docs link check: $($files.Count) file(s), $linkCount link(s) checked under $relDocs"
if ($broken.Count -gt 0) {
    Write-Host ''
    Write-Host 'Broken links:'
    foreach ($b in $broken) { Write-Host "  $b" }
    Write-Host ''
    Write-Host "FAILED: $($broken.Count) broken link(s)." -ForegroundColor Red
    exit 1
}
Write-Host "PASS: all $linkCount link(s) resolve." -ForegroundColor Green
exit 0
