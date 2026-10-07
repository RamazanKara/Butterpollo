param([int] $HostId, [switch] $ProcessesOnly)
$ErrorActionPreference = 'Stop'
$process = Get-Process -Id $HostId
$processes = @(Get-CimInstance Win32_Process | Where-Object {
    $_.Name -match 'butterpollo|moonlight|gpu_load|cargo|rustc|_probe' -or
    ($_.Name -match 'python' -and $_.CommandLine -match 'e2e|soak|measure|gpu')
} | Select-Object ProcessId, ParentProcessId, Name, CommandLine)
if ($ProcessesOnly) {
    @{ utc = [DateTime]::UtcNow.ToString('o'); processes = $processes } | ConvertTo-Json -Depth 4 -Compress
    return
}
$gpu = $null
$gpuError = $null
try {
    $gpu = @(Get-CimInstance Win32_PerfFormattedData_GPUPerformanceCounters_GPUProcessMemory |
        Where-Object Name -Match "^pid_${HostId}_" |
        Select-Object Name, DedicatedUsage, SharedUsage, TotalCommitted)
} catch { $gpuError = $_.Exception.Message }
$devices = @(Get-CimInstance Win32_PnPEntity |
    Where-Object { $_.Present -and ($_.PNPClass -eq 'Monitor' -or $_.Name -match 'Xbox|ViGEm|Virtual.*Gamepad|VHF|DualSense|DualShock') } |
    Select-Object Name, PNPClass, DeviceID)
@{
    utc = [DateTime]::UtcNow.ToString('o')
    pid = $HostId
    working_set = $process.WorkingSet64
    handles = $process.HandleCount
    threads = $process.Threads.Count
    gpu_memory = $gpu
    gpu_memory_error = $gpuError
    devices = $devices
    processes = $processes
} | ConvertTo-Json -Depth 5 -Compress
