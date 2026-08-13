# Restore a compare worktree to HEAD. Tracked files only.
#
# Never `git clean`. Never `git worktree remove`. Never junction turbovault/
# or turbomcp — `git worktree remove --force` once followed a junction and
# deleted the originals.
#
# Usage:
#   powershell -File scripts/reset-compare-worktree.ps1 -Path C:\...\ws-liberado
#   powershell -File scripts/reset-compare-worktree.ps1 -Path C:\...\ws-liberado -Commit 0ac59ca

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $Path,
    [string] $Commit
)

$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $Path)) {
    throw "worktree does not exist: $Path"
}

if ($Commit) {
    git -C $Path checkout --detach $Commit
    if ($LASTEXITCODE -ne 0) { throw "git checkout --detach $Commit failed" }
}

git -C $Path restore --source=HEAD --worktree --staged .
if ($LASTEXITCODE -ne 0) { throw "git restore failed" }

git -C $Path status -sb
git -C $Path rev-parse --short HEAD
Write-Output "restored tracked files; untracked path-deps left in place"
