#Requires -Version 7
<#
.SYNOPSIS
Run the October 9 AMF A/B sweep after reboot, with an idle installed host.
.EXAMPLE
pwsh -NoProfile -File C:\src\bp-codex-amfsweep\rust\release\amf_sweep.ps1
#>
param([string] $Work = ('D:\bp-build\amfsweep-target\sweeps\' + (Get-Date -Format 'yyyyMMdd-HHmmss')))
$ErrorActionPreference = 'Stop'
$artifact = 'C:\Users\ramaz\.codex\artifacts\butterpollo-rust-20260930'
$python = "$artifact\test-python\Scripts\python.exe"
$driver = Join-Path $PSScriptRoot 'amf_sweep.py'
$estimate = (& $python $driver estimate | ConvertFrom-Json)
if ($LASTEXITCODE) { throw 'Cannot read the sweep estimate' }
Write-Host "Estimate: $($estimate.estimate_minutes) minutes; hard stop at $($estimate.hard_stop_minutes) minutes (including builds)."
Write-Host "$($estimate.quality_runs_max) quality runs maximum, $($estimate.age_runs_max) picture-age runs; A/B/A/B. $($estimate.assumptions)."
Write-Host 'HEVC/AV1 HDR and H.264 SDR: 1080p60/20 Mb/s, 1440p120/50 Mb/s, 1968x2184p120/80 Mb/s.'
Write-Host 'Quality: two existing SDR game clips, converted to PQ for HDR. Picture age: top three per codec and size.'
if (Test-Path -LiteralPath $Work) { throw "Use a new output directory: $Work" }
New-Item -ItemType Directory -Path $Work | Out-Null
$Work = (Resolve-Path -LiteralPath $Work).Path
$started = [Diagnostics.Stopwatch]::StartNew()
$watchdog = 'C:\Windows\LiveKernelReports\WATCHDOG'
$watchedLogs = @{}
$initialReports = @{}
$worker = $null
$job = [IntPtr]::Zero
$lastSafety = $null
$failed = $false

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class AmfSweepWindows {
    [StructLayout(LayoutKind.Sequential)] struct BasicLimits {
        public long ProcessTime, JobTime; public uint Flags; public UIntPtr MinWorkingSet, MaxWorkingSet;
        public uint ActiveProcesses; public UIntPtr Affinity; public uint Priority, Scheduling;
    }
    [StructLayout(LayoutKind.Sequential)] struct IoCounters { public ulong A, B, C, D, E, F; }
    [StructLayout(LayoutKind.Sequential)] struct ExtendedLimits {
        public BasicLimits Basic; public IoCounters Io; public UIntPtr ProcessMemory, JobMemory, PeakProcessMemory, PeakJobMemory;
    }
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr CreateJobObject(IntPtr attributes, string name);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool SetInformationJobObject(IntPtr job, int kind, ref ExtendedLimits limits, uint size);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
    [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr handle);
    public static IntPtr OwnProcess(IntPtr process) {
        var job = CreateJobObject(IntPtr.Zero, null);
        var limits = new ExtendedLimits { Basic = new BasicLimits { Flags = 0x2000 } };
        if (job == IntPtr.Zero || !SetInformationJobObject(job, 9, ref limits, (uint)Marshal.SizeOf<ExtendedLimits>()) ||
            !AssignProcessToJobObject(job, process)) {
            int error = Marshal.GetLastWin32Error();
            if (job != IntPtr.Zero) CloseHandle(job);
            throw new System.ComponentModel.Win32Exception(error, "Cannot own sweep process tree");
        }
        return job;
    }
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int L, T, R, B; }
    [StructLayout(LayoutKind.Sequential)] public struct MonitorInfo { public int Size; public Rect Monitor, Work; public uint Flags; }
    public class Fullscreen { public uint Pid; public string Title; }
    delegate bool EnumProc(IntPtr hwnd, IntPtr arg);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc proc, IntPtr arg);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool IsIconic(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] static extern IntPtr MonitorFromWindow(IntPtr hwnd, uint flags);
    [DllImport("user32.dll")] static extern bool GetMonitorInfo(IntPtr monitor, ref MonitorInfo info);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder text, int count);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int count);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd, int attr, out int value, int size);
    public static Fullscreen[] Find() {
        var windows = new List<Fullscreen>();
        if (!EnumWindows((hwnd, arg) => {
            if (!IsWindowVisible(hwnd) || IsIconic(hwnd)) return true;
            int cloaked;
            if (DwmGetWindowAttribute(hwnd, 14, out cloaked, 4) == 0 && cloaked != 0) return true;
            var name = new StringBuilder(256); GetClassName(hwnd, name, name.Capacity);
            if (name.ToString() == "Progman" || name.ToString() == "WorkerW" || name.ToString() == "Shell_TrayWnd") return true;
            Rect rect; var info = new MonitorInfo { Size = Marshal.SizeOf<MonitorInfo>() };
            if (!GetWindowRect(hwnd, out rect) || !GetMonitorInfo(MonitorFromWindow(hwnd, 2), ref info)) return true;
            var m = info.Monitor;
            if (rect.L <= m.L + 2 && rect.T <= m.T + 2 && rect.R >= m.R - 2 && rect.B >= m.B - 2) {
                uint pid; GetWindowThreadProcessId(hwnd, out pid);
                var title = new StringBuilder(1024); GetWindowText(hwnd, title, title.Capacity);
                windows.Add(new Fullscreen { Pid = pid, Title = title.ToString() });
            }
            return true;
        }, IntPtr.Zero)) throw new InvalidOperationException("Cannot enumerate fullscreen windows");
        return windows.ToArray();
    }
}
'@

function Get-Reports {
    if (Test-Path -LiteralPath $watchdog) {
        Get-ChildItem -LiteralPath $watchdog -File -Recurse | ForEach-Object {
            [pscustomobject]@{ path = $_.FullName; signature = "$($_.Length):$($_.LastWriteTimeUtc.Ticks)" }
        }
    }
}

function Test-Logs {
    foreach ($path in @($watchedLogs.Keys)) {
        if (!(Test-Path -LiteralPath $path)) { continue }
        $file = [IO.File]::Open($path, 'Open', 'Read', 'ReadWrite')
        try {
            [void]$file.Seek($watchedLogs[$path], 'Begin')
            $reader = [IO.StreamReader]::new($file)
            $text = $reader.ReadToEnd()
            $watchedLogs[$path] = [math]::Max(0, $file.Position - 512)
            # Never interpret a supported optional-property rejection as a GPU reset.
            $fatal = '(?im)^.*(?:DXGI_ERROR_DEVICE_(?:REMOVED|HUNG|RESET)|0x887a000[567]|AMF_(?:DIRECTX_FAILED|DEVICE_LOST)|device (?:removed|hung|lost)|driver (?:timeout|timed out)|GPU (?:error|hang|reset)|AMF stall snapshot|encoder output stalled|was not encoded within two seconds).*$'
            if ($text -match $fatal) { throw "GPU/stall log evidence in ${path}: $($Matches[0])" }
        } finally { $file.Dispose() }
    }
}

function Assert-Safe([string] $Reason) {
    if ($started.Elapsed.TotalMinutes -ge $estimate.hard_stop_minutes) { throw '88-minute sweep deadline reached' }
    Test-Logs
    $radeon = @(Get-PnpDevice -Class Display -PresentOnly | Where-Object FriendlyName -Match 'Radeon' |
        Select-Object FriendlyName, Status, Problem, InstanceId)
    $reports = @(Get-Reports)
    $newReports = @($reports | Where-Object { !$initialReports.ContainsKey($_.path) -or $initialReports[$_.path] -ne $_.signature })
    $gpu = @{ radeon = $radeon; watchdog = $newReports } | ConvertTo-Json -Compress -Depth 6
    if (!$radeon.Count -or @($radeon | Where-Object { $_.Status -ne 'OK' -or [string]$_.Problem -notin @('', '0', 'CM_PROB_NONE') }).Count -or $newReports.Count) {
        $gpu | Add-Content -LiteralPath "$Work\safety.jsonl"
        throw "Radeon error or new/changed WATCHDOG report: $gpu"
    }
    $info = [xml](Invoke-WebRequest 'http://127.0.0.1:47989/serverinfo' -NoProxy -TimeoutSec 3).Content
    $sessions = [string]$info.root.RustHostSessionCount
    $pending = [string]$info.root.RustHostPendingSessionCount
    $fullscreen = @([AmfSweepWindows]::Find())
    if ($worker -and $fullscreen.Count) {
        # Only this sweep's motion renderer may cover a display during a run.
        $processes = @(Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, ExecutablePath)
        $owned = [Collections.Generic.HashSet[uint32]]::new()
        [void]$owned.Add($worker.Id)
        do {
            $added = $false
            foreach ($process in $processes) {
                if ($owned.Contains($process.ParentProcessId) -and $owned.Add($process.ProcessId)) { $added = $true }
            }
        } while ($added)
        $fullscreen = @($fullscreen | Where-Object {
            $window = $_
            $renderer = $processes | Where-Object { $_.ProcessId -eq $window.Pid -and $_.ExecutablePath -eq "$artifact\bench-rc21\tools\motion_probe.exe" }
            !($owned.Contains($window.Pid) -and $renderer)
        })
    }
    $observation = [ordered]@{ utc = [DateTime]::UtcNow.ToString('o'); reason = $Reason; sessions = $sessions;
        pending = $pending; radeon = $radeon; watchdog = $newReports; fullscreen = $fullscreen }
    $observation | ConvertTo-Json -Compress -Depth 6 | Add-Content -LiteralPath "$Work\safety.jsonl"
    $state = $observation | ConvertTo-Json -Compress -Depth 6
    if ($sessions -ne '0' -or $pending -ne '0') { throw "Installed host is busy or session counts are missing: $state" }
    if ($fullscreen.Count) { throw "Fullscreen application may be a game: $state" }
}

try {
    foreach ($report in @(Get-Reports)) { $initialReports[$report.path] = $report.signature }
    Write-Host "Existing WATCHDOG files: $($initialReports.Keys -join ', ')"
    $initialReports | ConvertTo-Json | Set-Content -LiteralPath "$Work\watchdog-before.json"
    Assert-Safe 'initial preflight'
    Write-Host (Get-Content -LiteralPath "$Work\safety.jsonl" -Tail 1)
    . "$artifact\performance-probe\rust-env.ps1"
    $env:CARGO_TARGET_DIR = 'D:\bp-build\amfsweep-target'
    $env:PYTHONUNBUFFERED = '1'
    $start = [Diagnostics.ProcessStartInfo]::new($python)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.WindowStyle = 'Hidden'
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in @($driver, 'run', $Work)) { $start.ArgumentList.Add($argument) }
    $worker = [Diagnostics.Process]::Start($start)
    $job = [AmfSweepWindows]::OwnProcess($worker.Handle)
    $stderr = $worker.StandardError.ReadToEndAsync()
    $line = $worker.StandardOutput.ReadLineAsync()
    $lastSafety = $started.Elapsed.TotalSeconds
    while (!$worker.HasExited -or $line.IsCompleted) {
        if ($line.IsCompleted) {
            $message = $line.GetAwaiter().GetResult()
            if ($null -eq $message) { break }
            $event = $message | ConvertFrom-Json
            if ($event.guard) {
                foreach ($path in $event.logs) { if (!$watchedLogs.ContainsKey($path)) { $watchedLogs[$path] = 0L } }
                Assert-Safe $event.guard
                $worker.StandardInput.WriteLine('ok')
                $worker.StandardInput.Flush()
                $lastSafety = $started.Elapsed.TotalSeconds
            } else {
                Test-Logs
                $watchedLogs.Clear()
                Write-Host $event.result
            }
            $line = $worker.StandardOutput.ReadLineAsync()
        }
        if ($started.Elapsed.TotalSeconds - $lastSafety -ge 2) {
            Assert-Safe 'running'
            $lastSafety = $started.Elapsed.TotalSeconds
        }
        Start-Sleep -Milliseconds 50
    }
    $worker.WaitForExit()
    $stderr.GetAwaiter().GetResult() | Set-Content -LiteralPath "$Work\driver.stderr.log"
    Assert-Safe 'finished'
    if ($worker.ExitCode) { throw "Sweep driver exited $($worker.ExitCode): $(Get-Content -LiteralPath "$Work\driver.stderr.log" -Tail 12 | Out-String)" }
} catch {
    $failed = $true
    $failure = "ABORTED: $($_.Exception.Message)"
    Write-Host $failure
    $failure | Set-Content -LiteralPath "$Work\status.txt"
} finally {
    if ($job -ne [IntPtr]::Zero) { [void][AmfSweepWindows]::CloseHandle($job) }
    if ($worker -and !$worker.HasExited) {
        try { $worker.Kill($true) } catch [InvalidOperationException] { }
        $worker.WaitForExit()
    }
    if ($stderr -and $stderr.IsCompleted) { $stderr.GetAwaiter().GetResult() | Set-Content -LiteralPath "$Work\driver.stderr.log" }
    & $python $driver summarize $Work
    Write-Host "Output: $Work\summary.md (quality.csv, picture-age.csv, runs.csv, raw logs and safety.jsonl)."
    Write-Host "Elapsed: $([math]::Round($started.Elapsed.TotalMinutes, 1)) minutes."
}
if ($failed) { exit 1 }
