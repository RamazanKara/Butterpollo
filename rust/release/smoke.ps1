#Requires -Version 7
<#
.SYNOPSIS
A 30-second real-client smoke test of a release from the measurement laptop.

.DESCRIPTION
Runs on the laptop (Moonlight-qt paired with the host as client "lap", whose
Extended layout streams a virtual display). For each codec it streams the
Desktop app at the owner's native settings, closes Moonlight after -Seconds,
and reads Moonlight's "Global video stats" from its log. A run passes when
Moonlight received at least -MinFps and the host processing average stayed
under -MaxHostMs. Moonlight's stats are session averages; they include the
first seconds of the session.

The host must be idle and must draw motion on the streamed display for the
whole session: motion_probe at twice the stream rate (240 Hz for 120 fps),
running at least -Seconds + 15 s from the launch. The host only sends frames
when the picture changes, so a static desktop streams at about 20 fps and a
probe that stops early or draws too slowly pulls the session average below
-MinFps. -App can name a host app that starts the probe itself.

Run it after check.ps1 installed the release on the host. Writes -Out
(JSON), uploads it to the release as REAL-CLIENT.json when -Version is given
and the GitHub CLI is signed in here, and exits 1 when a run fails. If the
laptop is offline, the release simply has no REAL-CLIENT.json.

.EXAMPLE
pwsh rust/release/smoke.ps1 -Version 2.0.0-rc.23
#>
param(
    [string] $Version = '',
    [string] $Server = '192.168.4.70',
    [string] $App = 'Desktop',
    [string] $Moonlight = "$env:ProgramFiles\Moonlight Game Streaming\Moonlight.exe",
    [string[]] $Codecs = @('AV1', 'HEVC'),
    [string] $Resolution = '1968x2184',
    [int] $Fps = 120,
    [int] $Bitrate = 80000,
    [int] $Seconds = 30,
    [double] $MinFps = 115,
    [double] $MaxHostMs = 5,
    [string] $Out = (Join-Path $PWD "smoke-$(if ($Version) { $Version } else { 'host' }).json")
)
$ErrorActionPreference = 'Stop'

function Wait-Idle {
    $deadline = (Get-Date).AddSeconds(60)
    while ($true) {
        try {
            $info = [xml](Invoke-WebRequest "http://${Server}:47989/serverinfo" -TimeoutSec 5 -NoProxy).Content
            if ($info.root.state -eq 'SUNSHINE_SERVER_FREE') { return $info.root.RustHostVersion }
        } catch { }
        if ((Get-Date) -gt $deadline) { throw "$Server is not reachable or not idle" }
        Start-Sleep -Seconds 2
    }
}

function Read-Stats([string] $text) {
    $at = $text.LastIndexOf('Global video stats')
    if ($at -lt 0) { return $null }
    $block = $text.Substring($at)
    # The numbers of the first line matching $pattern, or zeros when Moonlight
    # did not log it.
    function Numbers([string] $pattern, [int] $count = 1) {
        $m = [regex]::Match($block, $pattern)
        1..$count | ForEach-Object { if ($m.Success) { [double]$m.Groups[$_].Value } else { 0.0 } }
    }
    $hostMs = @(Numbers 'Host processing latency min/max/average: ([0-9.]+)/([0-9.]+)/([0-9.]+) ms' 3)
    [ordered]@{
        incoming_fps = Numbers 'Incoming frame rate from network: ([0-9.]+)'
        decoding_fps = Numbers 'Decoding frame rate: ([0-9.]+)'
        rendering_fps = Numbers 'Rendering frame rate: ([0-9.]+)'
        host_min_ms = $hostMs[0]
        host_max_ms = $hostMs[1]
        host_avg_ms = $hostMs[2]
        network_drop_pct = Numbers 'Frames dropped by your network connection: ([0-9.]+)'
        jitter_drop_pct = Numbers 'Frames dropped due to network jitter: ([0-9.]+)'
        network_ms = Numbers 'Average network latency: ([0-9.]+) ms'
        decode_ms = Numbers 'Average decoding time: ([0-9.]+)'
        render_ms = Numbers 'Average rendering time[^:]*: ([0-9.]+)'
    }
}

if (-not (Test-Path $Moonlight)) { throw "Moonlight-qt is not installed at $Moonlight" }
$gpu = (Get-CimInstance Win32_VideoController | Select-Object -First 1)
$runs = foreach ($codec in $Codecs) {
    $hostVersion = Wait-Idle
    if ($Version -and $hostVersion -ne $Version) { throw "$Server runs $hostVersion, not $Version" }
    Write-Host "[$codec] streaming $Resolution@$Fps HDR for $Seconds s" -ForegroundColor Cyan
    $started = Get-Date
    $arguments = 'stream', $Server, $App, '--resolution', $Resolution, '--fps', $Fps, '--bitrate', $Bitrate,
        '--video-codec', $codec, '--hdr', '--display-mode', 'windowed', '--no-vsync', '--no-frame-pacing',
        '--audio-on-host', '--quit-after'
    $process = Start-Process $Moonlight -ArgumentList $arguments -PassThru
    Start-Sleep -Seconds $Seconds
    # A window close ends the session the way a user does, so Moonlight logs its stats.
    $null = $process.CloseMainWindow()
    if (-not $process.WaitForExit(30000)) { $process.Kill(); $process.WaitForExit() }
    $log = Get-ChildItem $env:TEMP -Filter 'Moonlight-*.log' | Where-Object LastWriteTime -GE $started |
        Sort-Object LastWriteTime | Select-Object -Last 1
    $stats = if ($log) { Read-Stats (Get-Content $log.FullName -Raw) }
    $failures = @()
    if (-not $stats) { $failures += 'no Moonlight video stats' }
    else {
        if ($stats.incoming_fps -lt $MinFps) {
            $failures += "incoming $($stats.incoming_fps) fps < $MinFps"
            # The host only sends frames when the picture changes.
            if ($stats.incoming_fps -lt $Fps * .75) { $failures += "too few frames for a moving picture: was motion_probe drawing on the streamed display at $($Fps * 2) Hz for the whole $Seconds s?" }
        }
        if ($stats.host_avg_ms -ge $MaxHostMs -or $stats.host_avg_ms -le 0) { $failures += "host processing average $($stats.host_avg_ms) ms, limit $MaxHostMs" }
    }
    $run = [ordered]@{ codec = $codec; mode = "${Resolution}x$Fps"; hdr = $true; bitrate_kbps = $Bitrate; seconds = $Seconds
        host_version = $hostVersion; stats = $stats; log = $log.FullName; passed = $failures.Count -eq 0; failures = $failures }
    Write-Host ("[$codec] " + $(if ($run.passed) { "passed: $($stats.incoming_fps) fps, host $($stats.host_avg_ms) ms avg ($($stats.host_max_ms) max)" } else { 'FAILED: ' + ($failures -join '; ') }))
    $run
}
$result = [ordered]@{
    client = "$env:COMPUTERNAME, $($gpu.Name), driver $($gpu.DriverVersion), $((Get-Item $Moonlight).VersionInfo.ProductVersion)"
    server = $Server
    criteria = "incoming fps >= $MinFps and host processing average < $MaxHostMs ms (Moonlight session averages)"
    at_utc = (Get-Date).ToUniversalTime().ToString('o')
    runs = @($runs)
    passed = -not ($runs | Where-Object { -not $_.passed })
}
$result | ConvertTo-Json -Depth 5 | Set-Content $Out -Encoding utf8
if ($Version -and (Get-Command gh -ErrorAction SilentlyContinue)) {
    $upload = Join-Path ([IO.Path]::GetTempPath()) 'REAL-CLIENT.json'
    Copy-Item $Out $upload -Force
    try { gh release upload $Version -R RamazanKara/Butterpollo --clobber $upload } catch { Write-Warning "not uploaded: $_" }
}
Write-Host "smoke test $(if ($result.passed) { 'passed' } else { 'FAILED' }): $Out"
if (-not $result.passed) { exit 1 }
