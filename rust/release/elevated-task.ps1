# Started by the ButterpolloReleaseElevated task (see elevation.ps1) with
# highest privileges. Reads the request release.ps1 wrote and runs the
# elevated step for it. Only paths inside release.ps1's default work folder
# are accepted.
$ErrorActionPreference = 'Stop'
$root = 'C:\src\butterpollo-release\'
$request = Get-Content (Join-Path $root 'request.json') -Raw | ConvertFrom-Json
$version = [string]$request.version
if ($version -notmatch '^\d+\.\d+\.\d+(-rc\.\d+)?$') { throw "invalid version $version" }
$work = [IO.Path]::GetFullPath((Join-Path $root $version))
$package = [IO.Path]::GetFullPath("$work\release\butterpollo-rust-release")
$installer = [IO.Path]::GetFullPath("$work\release\rubylight-setup-$version.exe")
if (-not (Test-Path -LiteralPath $installer)) {
    # Releases before rc.30 carry only the Butterpollo name.
    $installer = [IO.Path]::GetFullPath("$work\release\butterpollo-setup-$version.exe")
}
foreach ($path in $work, $package, $installer) {
    if (-not $path.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) { throw "$path is outside $root" }
}
& (Join-Path $PSScriptRoot 'elevated.ps1') -Package $package -Installer $installer -Version $version -Work $work
