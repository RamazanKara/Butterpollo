#Requires -Version 7
<#
.SYNOPSIS
Stream-test and install a published Butterpollo release on the Radeon host.

.DESCRIPTION
Pushing a tag builds, tests, packages and publishes a release on GitHub
Actions (.github/workflows/rust-windows.yml). This script is the part only
the release workstation can do, run when its installed host is idle:
1. Downloads the release's installer and portable ZIP and checks them against
   SHA256SUMS.
2. Streams H.264, HEVC, HEVC VRR, AV1 and PyroWave (SDR and HDR 4:4:4)
   through the packaged host with the independent moonlight-common-c client,
   plus HEVC with keyframe requests from the client while the picture moves
   at the stream rate, then runs the protocol checks (e2e.py, protocol.py).
3. The display self-test as SYSTEM and a quiet install over the running host
   (elevated.ps1): directly from an elevated shell, otherwise through the task
   elevation.ps1 installs, otherwise after one UAC prompt.
4. Uploads the results to the release as VALIDATION.json (validation.py).
The laptop's real-client smoke test (smoke.ps1) runs after the install and
adds REAL-CLIENT.json.

.EXAMPLE
pwsh rust/release/check.ps1                  # the newest release
.EXAMPLE
pwsh rust/release/check.ps1 2.0.0-rc.23 -NoInstall -NoUpload
#>
param(
    # Defaults to the newest published release.
    [string] $Version,
    # Not under %LOCALAPPDATA%: a packaged (MSIX) app such as the Claude
    # desktop app sees its own copy of that folder, so the SYSTEM self-test
    # could not find a package unpacked there.
    [string] $Work = 'C:\src\butterpollo-release',
    # Machine settings, dot-sourced: the Rust build environment (PATH, FFmpeg,
    # PyroWave, ...) and BUTTERPOLLO_TEST_PYTHON.
    [string] $Settings = (Join-Path $Work 'settings.ps1'),
    # Stream cases to leave out, e.g. 'pyrowave','pyrowave-hdr-444' when a
    # shared machine cannot sustain them; recorded in VALIDATION.json.
    [string[]] $SkipStreams = @(),
    # A smoke.ps1 result from the laptop to include; smoke.ps1 normally
    # uploads its own REAL-CLIENT.json after this check installed the release.
    [string] $RealClient,
    [switch] $NoInstall,
    [switch] $NoUpload
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$repo = 'RamazanKara/Butterpollo'
$started = Get-Date
function Step($name) { Write-Host ('[{0:mm\:ss}] {1}' -f ((Get-Date) - $started), $name) -ForegroundColor Cyan }

if (Test-Path $Settings) { . $Settings }
$python = if ($env:BUTTERPOLLO_TEST_PYTHON) { $env:BUTTERPOLLO_TEST_PYTHON } else { 'python' }
$tools = $PSScriptRoot
$checkout = (Resolve-Path "$PSScriptRoot\..\..").Path
if (-not $Version) { $Version = gh release list -R $repo --exclude-drafts --limit 1 --json tagName --jq '.[0].tagName' }
# The layout elevated-task.ps1 expects: <Work>\<version>\release\...
$run = Join-Path $Work $Version
$out = "$run\release"
$qa = "$run\qa"
# Kept between releases, so the receivers and probes only rebuild what changed.
$fixtures = Join-Path $Work 'fixtures'
$package = "$out\butterpollo-rust-release"
$installer = "$out\butterpollo-setup-$Version.exe"
$zip = "$out\butterpollo-rust-$Version-windows-x64.zip"

Step "download $Version"
Remove-Item -Recurse -Force $out -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $out, $qa | Out-Null
gh release download $Version -R $repo -D $out -p (Split-Path $installer -Leaf) -p (Split-Path $zip -Leaf) -p SHA256SUMS
foreach ($line in Get-Content "$out\SHA256SUMS") {
    $hash, $name = $line -split '\s+', 2
    if ((Get-FileHash "$out\$name" -Algorithm SHA256).Hash -ne $hash) { throw "$name does not match SHA256SUMS" }
}
Expand-Archive $zip -DestinationPath $out

Step 'test receivers and probes'
git -C $checkout submodule update --init --recursive --depth 1 third-party/moonlight-common-c
& "$checkout\rust\tests\build-moonlight-client.ps1" -ArtifactDirectory $fixtures *> "$qa\receiver-build.log"
& "$checkout\rust\tests\build-pyrowave-client.ps1" -ArtifactDirectory $fixtures -PyrowaveRoot $env:BUTTERPOLLO_PYROWAVE_ROOT *> "$qa\pyrowave-receiver-build.log"
cargo build --release --locked --manifest-path "$checkout\Cargo.toml" --target-dir "$checkout\target\ship" -p butterpollo-windows `
    --example audio_probe --example motion_probe *> "$qa\probe-build.log"
Copy-Item "$checkout\target\ship\release\examples\audio_probe.exe", "$checkout\target\ship\release\examples\motion_probe.exe" $fixtures
$client = "$fixtures\moonlight-client.exe"

foreach ($case in 'h264', 'hevc', 'av1', 'hevc-vrr', 'hevc-recovery', 'pyrowave', 'pyrowave-hdr-444') {
    if ($SkipStreams -contains $case) { Write-Warning "stream $case skipped (-SkipStreams); recorded in VALIDATION.json"; continue }
    $codec = $case -replace '-(vrr|recovery)$'
    # 12 s of streaming each: enough for the steady-rate, motion and audio checks.
    $stream = @('--package', $package, '--work', $run, '--client', $client, '--codec', $codec, '--seconds', '12')
    if ($case.EndsWith('-vrr')) { $stream += '--vrr' }
    # A client losing packets: 16 keyframe requests, one every 0.5 s, must
    # not send pictures twice or skip new frames (rc.27-rc.28 did).
    if ($case.EndsWith('-recovery')) { $stream += '--recovery', '16' }
    if ($codec.StartsWith('pyrowave')) {
        $stream = @('--package', $package, '--work', $run, '--client', "$fixtures\moonlight-pyrowave-client.exe",
                    '--codec', $codec, '--mode', '1920x1080x60', '--seconds', '12', '--bitrate', '400000')
    }
    Step "stream $case"
    try {
        & $python "$tools\e2e.py" @stream
    } catch {
        # Shared-machine load can starve the tone or motion fixture too. Keep
        # the evidence: a passing retry does not explain the first failure.
        $resultPath = Join-Path $run "e2e-$case\result.json"
        if (Test-Path -LiteralPath $resultPath) {
            $result = Get-Content -LiteralPath $resultPath -Raw | ConvertFrom-Json
            Write-Warning ("stream $case failed: " + ($result.failures -join '; '))
        }
        Write-Warning "stream $case failed; preserving its logs and retrying once. Check shared CPU/GPU load and tone.log; a second failure stops the check."
        $failed = Join-Path $run "e2e-$case-failed"
        if (([IO.Path]::GetFullPath($failed) | Split-Path -Parent) -ne [IO.Path]::GetFullPath($run)) { throw 'Test output left the work directory' }
        Remove-Item -LiteralPath $failed -Recurse -Force -ErrorAction SilentlyContinue
        Move-Item -LiteralPath "$run\e2e-$case" -Destination $failed
        & $python "$tools\e2e.py" @stream
    }
}
Step 'protocol checks'
& $python "$tools\protocol.py" --package $package --work $run

if (-not $NoInstall) {
    Remove-Item -Recurse -Force "$run\elevated" -ErrorAction SilentlyContinue
    # elevation.ps1 installs a task that runs this step without a prompt. It
    # is used while its scripts match this checkout's and -Work is the default.
    $taskHome = 'C:\ProgramData\ButterpolloRelease'
    $task = $false
    try { schtasks /Query /TN ButterpolloReleaseElevated *> $null; $task = $true } catch { }
    $current = $task -and $Work -eq 'C:\src\butterpollo-release' -and
        @('elevated.ps1', 'elevated-task.ps1' | Where-Object {
            -not (Test-Path "$taskHome\$_") -or (Get-FileHash "$taskHome\$_").Hash -ne (Get-FileHash "$tools\$_").Hash
        }).Count -eq 0
    $admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
    $arguments = '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$tools\elevated.ps1`"",
        '-Package', "`"$package`"", '-Installer', "`"$installer`"", '-Version', $Version, '-Work', "`"$run`""
    if ($admin) {
        Step 'display self-test as SYSTEM and install'
        # Started from here, Windows PowerShell inherits PowerShell 7's module
        # path and cannot load Get-FileHash; give it its own.
        $modulePath = $env:PSModulePath
        $env:PSModulePath = [Environment]::GetEnvironmentVariable('PSModulePath', 'Machine')
        try { Start-Process powershell -WindowStyle Hidden -ArgumentList $arguments }
        finally { $env:PSModulePath = $modulePath }
    } elseif ($current) {
        Step 'display self-test as SYSTEM and install'
        @{ version = $Version } | ConvertTo-Json | Set-Content (Join-Path $Work 'request.json')
        schtasks /Run /TN ButterpolloReleaseElevated | Out-Null
    } else {
        if ($task) { Write-Warning 'The release elevation task is out of date; run rust/release/elevation.ps1 as administrator again.' }
        Step 'display self-test as SYSTEM and install (UAC prompt)'
        Start-Process powershell -Verb RunAs -WindowStyle Hidden -ArgumentList $arguments
    }
    $deadline = (Get-Date).AddMinutes(15)
    while (-not (Test-Path "$run\elevated\done.txt")) {
        if ((Get-Date) -gt $deadline) { throw 'The elevated step did not finish; was the UAC prompt declined?' }
        Start-Sleep -Seconds 2
    }
    if (-not (Test-Path "$run\elevated\installed.json")) { throw "The install did not finish; see $run\elevated\transcript.txt" }
    if (-not (Test-Path "$run\elevated\display-self-test.json")) {
        Write-Warning "The display self-test wrote no report; see $run\elevated\transcript.txt"
    }
}

Step 'record results'
$gpu = (Get-CimInstance Win32_VideoController | Where-Object Name -NotMatch 'Virtual|Basic|Idd' | Select-Object -First 1).Name
$record = @('--version', $Version, '--package', $package, '--work', $run, '--out', "$run\VALIDATION.json",
            '--scope', "Release workstation: $gpu, Windows $([Environment]::OSVersion.Version.Build). Other hardware was not tested.")
$record += @('--seconds', [int]((Get-Date) - $started).TotalSeconds)
if ($RealClient) { $record += @('--real-client', $RealClient) }
if (-not $NoInstall) { $record += '--installed' }
foreach ($skip in $SkipStreams) { $record += @('--skipped', $skip) }
& $python "$tools\validation.py" @record
if ($NoUpload) {
    Step "done, not uploaded: $run\VALIDATION.json"
    return
}
gh release upload $Version -R $repo --clobber "$run\VALIDATION.json"
# gh writes UTF-8; read in the console's code page, every non-ASCII
# character in the release text came back garbled (rc.23).
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$body = gh release view $Version -R $repo --json body --jq .body
if ($body -notmatch 'VALIDATION\.json') {
    $body = ($body -join "`n").TrimEnd() +
        " · [Host validation](https://github.com/$repo/releases/download/$Version/VALIDATION.json)`n"
    [IO.File]::WriteAllText("$run\body.md", $body, [Text.UTF8Encoding]::new($false))
    gh release edit $Version -R $repo --notes-file "$run\body.md"
}
Step "checked https://github.com/$repo/releases/tag/$Version"
