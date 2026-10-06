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
        # deleted afterwards. The service spawns the host into the console session.
        $report = Join-Path $out 'display-self-test.json'
        $task = 'ButterpolloDisplaySelfTest'
        $action = New-ScheduledTaskAction -Execute (Join-Path $Package 'butterpollo-service.exe') -Argument "--display-self-test `"$report`""
        $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
        Register-ScheduledTask -TaskName $task -Action $action -Principal $principal -Force | Out-Null
        Start-ScheduledTask -TaskName $task
        $deadline = (Get-Date).AddMinutes(5)
        do { Start-Sleep -Seconds 2 } until (((Test-Path $report) -and (Get-ScheduledTask -TaskName $task).State -ne 'Running') -or (Get-Date) -gt $deadline)
        Unregister-ScheduledTask -TaskName $task -Confirm:$false
        if (Test-Path $report) { Get-Content $report } else { 'no self-test report' }
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
