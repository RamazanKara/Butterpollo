[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('Install','Start','Stop','Uninstall','Status')][string]$Action,
    [string]$ConfigSource
)
$ErrorActionPreference = 'Stop'
$name = 'ApolloService'
$executable = Join-Path $PSScriptRoot 'butterpollo-service.exe'
$config = Join-Path $env:PROGRAMDATA 'Butterpollo\config'
$service = Get-CimInstance Win32_Service -Filter "Name='$name'"
if ($Action -eq 'Status') {
    if ($service) { $service | Select-Object Name, State, PathName } else { Write-Output 'Rust service is not installed' }
    return
}
$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (!$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Service management requires an administrator PowerShell' }
if ($service) {
    $expected = '"' + [IO.Path]::GetFullPath($executable) + '"'
    if (!$service.PathName.StartsWith($expected, [StringComparison]::OrdinalIgnoreCase)) {
        throw "ApolloService belongs to another installation ($($service.PathName)). This script will not replace or stop it."
    }
}
switch ($Action) {
    'Install' {
        if (!(Test-Path -LiteralPath $executable) -or !(Test-Path -LiteralPath (Join-Path $PSScriptRoot 'butterpollo.exe'))) { throw 'Run this script from the extracted Rust distribution' }
        if ($ConfigSource) {
            $source = (Resolve-Path -LiteralPath $ConfigSource).Path
            if (!(Test-Path -LiteralPath (Join-Path $source 'sunshine.conf'))) { throw '-ConfigSource must contain sunshine.conf' }
            if ((Test-Path -LiteralPath $config) -and (Get-ChildItem -LiteralPath $config -Force | Select-Object -First 1)) { throw 'Rust config directory already contains data; import to a separate empty directory first' }
            New-Item -ItemType Directory -Path $config -Force | Out-Null
            Get-ChildItem -LiteralPath $source -Force | Copy-Item -Destination $config -Recurse -Force
        }
        if (!$service) {
            New-Service -Name $name -BinaryPathName ('"' + $executable + '"') -DisplayName 'Butterpollo Rust' -StartupType Automatic -Description 'Rust Moonlight streaming host' | Out-Null
        }
        Write-Output "Installed Rust service. Configuration: $config. Run service.ps1 -Action Start to start it."
    }
    'Start' { if (!$service) { throw 'Rust service is not installed' }; Start-Service -Name $name }
    'Stop' { if ($service) { Stop-Service -Name $name; (Get-Service -Name $name).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30)) } }
    'Uninstall' {
        if ($service) {
            Stop-Service -Name $name
            (Get-Service -Name $name).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
            & sc.exe delete $name
            if ($LASTEXITCODE -ne 0) { throw "Service removal failed ($LASTEXITCODE)" }
        }
        Write-Output 'Rust service removed. Configuration files retained.'
    }
}
