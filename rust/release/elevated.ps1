# The one elevated step of a release: the display self-test as SYSTEM, then
# a quiet install of the new setup over the running host. release.ps1 starts
# it with a UAC prompt; it writes WORK\elevated\installed.json and done.txt.
param(
    [Parameter(Mandatory)] [string] $Package,
    [Parameter(Mandatory)] [string] $Installer,
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $Work,
    [switch] $SkipSelfTest
)
$ErrorActionPreference = 'Continue'
$out = Join-Path $Work 'elevated'
New-Item -ItemType Directory -Force $out | Out-Null
Start-Transcript (Join-Path $out 'transcript.txt') -Force | Out-Null
try {
    if (-not $SkipSelfTest) {
        # The virtual display driver only accepts SYSTEM: a one-shot task,
        # deleted afterwards. The service spawns the host into the console
        # session. schtasks.exe, not the ScheduledTask cmdlets: a task from
        # Register-ScheduledTask never ran on the release workstation. Its
        # command line is limited to 261 characters, hence the short report path.
        $report = Join-Path $env:ProgramData 'Butterpollo\release-self-test.json'
        Remove-Item $report -ErrorAction SilentlyContinue
        $task = 'ButterpolloDisplaySelfTest'
        $command = "`"$(Join-Path $Package 'butterpollo-service.exe')`" --display-self-test `"$report`""
        if ($command.Length -gt 261) { throw "self-test command line too long ($($command.Length))" }
        schtasks /Create /TN $task /TR $command /SC ONCE /ST 23:59 /RU SYSTEM /RL HIGHEST /F | Out-Host
        schtasks /Run /TN $task | Out-Host
        $deadline = (Get-Date).AddMinutes(5)
        do {
            Start-Sleep -Seconds 2
            $state = (schtasks /Query /TN $task /FO LIST /V | Select-String '^(Status|Last Result|Letztes Ergebnis):').Line -join ' | '
        } until (((Test-Path $report) -and $state -notmatch 'Running|Wird ausgef') -or (Get-Date) -gt $deadline)
        "self-test task: $state"
        if (-not (Test-Path $report)) { schtasks /End /TN $task | Out-Host }
        schtasks /Delete /TN $task /F | Out-Host
        if (Test-Path $report) {
            Move-Item $report (Join-Path $out 'display-self-test.json') -Force
            Get-Content (Join-Path $out 'display-self-test.json')
        } else { 'no self-test report' }
    }

    $setup = Start-Process -FilePath $Installer -ArgumentList '--quiet' -Wait -PassThru
    "setup exit $($setup.ExitCode)"
    $reported = $null
    $deadline = (Get-Date).AddSeconds(90)
    do {
        Start-Sleep -Seconds 2
        $info = (& curl.exe -s --max-time 5 http://127.0.0.1:47989/serverinfo) -join ' '
        if ($info -match '<RustHostVersion>([^<]+)</RustHostVersion>') { $reported = $Matches[1] }
    } until ($reported -eq $Version -or (Get-Date) -gt $deadline)
    $installed = [ordered]@{
        setup = "$(Split-Path $Installer -Leaf) --quiet over the running host"
        setup_exit = $setup.ExitCode
        reported_version = $reported
        host_sha256 = (Get-FileHash 'C:\Program Files\ButterpolloRust\butterpollo.exe' -Algorithm SHA256).Hash.ToLowerInvariant()
        service = "$((Get-Service ApolloService -ErrorAction SilentlyContinue).Status)"
    }
    $installed | ConvertTo-Json | Set-Content (Join-Path $out 'installed.json') -Encoding utf8
    $installed | ConvertTo-Json
} finally {
    Stop-Transcript | Out-Null
    'done' | Set-Content (Join-Path $out 'done.txt')
}
