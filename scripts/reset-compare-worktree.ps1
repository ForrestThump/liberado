[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $Path,
    [string] $Commit
)

$ErrorActionPreference = 'Stop'
$Repo = Split-Path -Parent $PSScriptRoot
Set-Location $Repo
$args = @('run', '--locked', '-p', 'liberado-cli', '--', 'coder', 'compare', 'reset', $Path)
if ($Commit) { $args += @('--commit', $Commit) }
& cargo @args
exit $LASTEXITCODE
