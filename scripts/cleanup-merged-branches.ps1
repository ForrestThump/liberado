# cleanup-merged-branches.ps1 — delete local branches that are fully on main.
#
# A branch is deleted only when it cannot introduce anything new:
#   1. Every commit is already an ancestor of main, or
#   2. Merging it into main would not change main's tree (squash / equivalent
#      patches). This is the "content" check — unique SHAs after a GitHub
#      squash merge do not keep the branch alive.
#
# A branch is kept when it has any content that is not on main. Worktree
# checkouts and the current branch are never deleted (git would refuse anyway).
#
# Default is a dry run. Pass -Apply to delete.
#
# Usage:
#   powershell -NoProfile -File scripts/cleanup-merged-branches.ps1
#   powershell -NoProfile -File scripts/cleanup-merged-branches.ps1 -Apply
#   powershell -NoProfile -File scripts/cleanup-merged-branches.ps1 -NoFetch

param(
    [switch]$Apply,
    [switch]$NoFetch,
    [string]$Base = ""
)

$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

function Invoke-Git {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]]$GitArgs)
    & git @GitArgs
    if ($LASTEXITCODE -ne 0) {
        throw "git $($GitArgs -join ' ') failed (exit $LASTEXITCODE)"
    }
}

function Test-GitAncestor {
    param([string]$Commit, [string]$Of)
    & git merge-base --is-ancestor $Commit $Of 2>$null | Out-Null
    return ($LASTEXITCODE -eq 0)
}

function Get-TreeOid {
    param([string]$Rev)
    # Concatenate: PowerShell parses `{tree}` as a script block inside some strings.
    $spec = $Rev + '^{tree}'
    $oid = & git rev-parse --verify $spec
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($oid)) {
        return $null
    }
    return $oid.Trim()
}

# True when merging $Branch into $BaseRef would not change $BaseRef's tree.
function Test-ContentAlreadyOnBase {
    param([string]$Branch, [string]$BaseRef)
    $baseTree = Get-TreeOid $BaseRef
    if (-not $baseTree) { return $false }
    $merged = & git merge-tree --write-tree $BaseRef $Branch
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($merged)) {
        return $false
    }
    $mergedOid = ($merged | Select-Object -First 1).Trim()
    return ($mergedOid -eq $baseTree)
}

function Get-AheadCount {
    param([string]$Branch, [string]$BaseRef)
    $range = $BaseRef + ".." + $Branch
    $n = & git rev-list --count $range
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($n)) { return "?" }
    return ($n | Select-Object -First 1).Trim()
}

if (-not $NoFetch) {
    Write-Host "Fetching origin (main)..."
    Invoke-Git fetch origin main --prune
}

if ([string]::IsNullOrWhiteSpace($Base)) {
    $hasOriginMain = $false
    & git show-ref --verify --quiet refs/remotes/origin/main
    if ($LASTEXITCODE -eq 0) { $hasOriginMain = $true }
    if ($hasOriginMain) {
        $Base = "origin/main"
    } else {
        $Base = "main"
    }
}

& git rev-parse --verify "${Base}^{commit}" 2>$null | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw "Base ref '$Base' does not exist. Fetch, or pass -Base."
}

$current = (Invoke-Git rev-parse --abbrev-ref HEAD).Trim()

$worktreeBranches = @{}
$wtList = & git worktree list --porcelain
$wtPath = $null
foreach ($line in $wtList) {
    if ($line -like "worktree *") {
        $wtPath = $line.Substring(9)
    } elseif ($line -like "branch refs/heads/*") {
        $name = $line.Substring("branch refs/heads/".Length)
        $worktreeBranches[$name] = $wtPath
    }
}

$protected = @{
    "main"   = $true
    "master" = $true
}

$local = @(Invoke-Git for-each-ref --format="%(refname:short)" refs/heads)
$deleteAncestor = New-Object System.Collections.Generic.List[string]
$deleteContent = New-Object System.Collections.Generic.List[string]
$keep = New-Object System.Collections.Generic.List[string]
$skip = New-Object System.Collections.Generic.List[string]

foreach ($branch in $local) {
    if ($protected.ContainsKey($branch)) {
        $skip.Add("${branch}`tprotected")
        continue
    }
    if ($branch -eq $current) {
        $skip.Add("${branch}`tcurrent branch")
        continue
    }
    if ($worktreeBranches.ContainsKey($branch)) {
        $skip.Add("${branch}`tchecked out in worktree $($worktreeBranches[$branch])")
        continue
    }

    if (Test-GitAncestor -Commit $branch -Of $Base) {
        $deleteAncestor.Add($branch)
        continue
    }
    if (Test-ContentAlreadyOnBase -Branch $branch -BaseRef $Base) {
        $deleteContent.Add($branch)
        continue
    }

    $ahead = Get-AheadCount -Branch $branch -BaseRef $Base
    $keep.Add("${branch}`t${ahead} commit(s) not on ${Base}")
}

function Write-Section {
    param([string]$Title, [System.Collections.IEnumerable]$Items)
    if (-not $Items -or @($Items).Count -eq 0) { return }
    Write-Host ""
    Write-Host $Title
    foreach ($item in $Items) {
        if ($item -match "`t") {
            $parts = $item -split "`t", 2
            Write-Host ("  {0,-48} {1}" -f $parts[0], $parts[1])
        } else {
            Write-Host "  $item"
        }
    }
}

Write-Host "Base: $Base"
if (-not $Apply) {
    Write-Host "Dry run. Pass -Apply to delete."
}

Write-Section "Would delete (every commit already on ${Base}):" $deleteAncestor
Write-Section "Would delete (content already on ${Base}, squash or equivalent):" $deleteContent
Write-Section "Keep (has content not on ${Base}):" $keep
Write-Section "Skip:" $skip

$toDelete = @($deleteAncestor) + @($deleteContent)
if ($toDelete.Count -eq 0) {
    Write-Host ""
    Write-Host "Nothing to delete."
    exit 0
}

if (-not $Apply) {
    Write-Host ""
    Write-Host ("{0} branch(es) would be deleted. Re-run with -Apply." -f $toDelete.Count)
    exit 0
}

Write-Host ""
$failed = 0
foreach ($branch in $toDelete) {
    & git branch -D $branch
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED to delete $branch"
        $failed++
    }
}

if ($failed -gt 0) {
    exit 1
}
Write-Host ("Deleted {0} branch(es)." -f $toDelete.Count)
exit 0
