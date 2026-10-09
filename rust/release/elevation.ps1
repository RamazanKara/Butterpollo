<#
.SYNOPSIS
One-time setup so check.ps1 no longer needs a UAC prompt.

.DESCRIPTION
Run once from an elevated PowerShell. It copies elevated.ps1 and
elevated-task.ps1 to C:\ProgramData\ButterpolloRelease, where only
administrators and SYSTEM can change them, and registers the on-demand task
"ButterpolloReleaseElevated" that runs them for you with highest privileges.
check.ps1 starts that task instead of asking for elevation, as long as the
installed scripts match the ones in the checkout; after they change, it asks
once more and tells you to run this again.

Trade-off: any program running as you can start the task, and the task runs
the release's setup and the display self-test from your release work folder
with administrator rights. Only install it on a machine where you accept that.

.EXAMPLE
pwsh -File rust/release/elevation.ps1           # install or update
.EXAMPLE
pwsh -File rust/release/elevation.ps1 -Remove   # uninstall
#>
#Requires -RunAsAdministrator
param([switch] $Remove)
$ErrorActionPreference = 'Stop'
$task = 'ButterpolloReleaseElevated'
$home_ = 'C:\ProgramData\ButterpolloRelease'

Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction SilentlyContinue
if ($Remove) {
    Remove-Item -Recurse -Force $home_ -ErrorAction SilentlyContinue
    "Removed $task and $home_."
    return
}

New-Item -ItemType Directory -Force $home_ | Out-Null
# Administrators and SYSTEM may change the scripts; everyone else may only run them.
icacls $home_ /inheritance:r /grant:r '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-545:(OI)(CI)RX' | Out-Null
foreach ($script in 'elevated.ps1', 'elevated-task.ps1') {
    Copy-Item (Join-Path $PSScriptRoot $script) $home_ -Force
}
# On demand only (no trigger), for the signed-in user, with the elevated token.
# An interactive principal needs no stored password.
$action = New-ScheduledTaskAction -Execute 'powershell.exe' `
    -Argument "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$home_\elevated-task.ps1`""
$principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" -LogonType Interactive -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit (New-TimeSpan -Minutes 30) -MultipleInstances IgnoreNew
Register-ScheduledTask -TaskName $task -Action $action -Principal $principal -Settings $settings `
    -Description 'Rubylight release: display self-test and install (rust/release/elevation.ps1)' | Out-Null
"Installed $task for $env:USERDOMAIN\$env:USERNAME. check.ps1 now installs and self-tests without a UAC prompt."
