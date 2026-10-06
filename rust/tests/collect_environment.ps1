# Read-only environment evidence for hardware-specific reports. Does not run a
# capture, change settings, include credentials, or upload anything.
[CmdletBinding()]
param(
    [string]$InstallDirectory = "$env:ProgramFiles\ButterpolloRust",
    [string]$OutputFile = (Join-Path $PWD ('butterpollo-environment-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.json'))
)
$ErrorActionPreference = 'Stop'
$report = [ordered]@{
    collected_at_utc = (Get-Date).ToUniversalTime().ToString('o')
    scope = 'Read-only host environment; no performance or Wi-Fi latency claim'
    errors = @()
}
try {
    $os = Get-CimInstance Win32_OperatingSystem -OperationTimeoutSec 10
    $report.os = [ordered]@{ name=$os.Caption; version=$os.Version; build=$os.BuildNumber; architecture=$os.OSArchitecture }
} catch { $report.errors += 'OS query: ' + $_.Exception.Message }
try {
    $report.gpus = @(Get-CimInstance Win32_VideoController -OperationTimeoutSec 10 | ForEach-Object {
        [ordered]@{ name=$_.Name; driver_version=$_.DriverVersion; driver_date=$_.DriverDate; status=$_.Status }
    })
} catch { $report.errors += 'GPU query: ' + $_.Exception.Message }
try {
    $report.network_adapters = @(Get-NetAdapter -Physical | ForEach-Object {
        $adapter = $_
        $stats = $null
        try { $stats = $adapter | Get-NetAdapterStatistics -ErrorAction Stop }
        catch { $report.errors += 'Statistics unavailable for ' + $adapter.InterfaceDescription }
        [ordered]@{
            description=$adapter.InterfaceDescription; media_type=[string]$adapter.MediaType
            physical_media_type=[string]$adapter.PhysicalMediaType; status=[string]$adapter.Status
            link_speed=$adapter.LinkSpeed; driver_version=$adapter.DriverVersion
            received_errors=$stats.ReceivedPacketErrors; outbound_errors=$stats.OutboundPacketErrors
            received_discards=$stats.ReceivedDiscardedPackets; outbound_discards=$stats.OutboundDiscardedPackets
        }
    })
} catch { $report.errors += 'Network query: ' + $_.Exception.Message }
$binary = Join-Path $InstallDirectory 'butterpollo.exe'
if (Test-Path -LiteralPath $binary -PathType Leaf) {
    try { $report.host_sha256 = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant() }
    catch { $report.errors += 'Host hash could not be read' }
    try {
        $report.host_version = ((& $binary --version 2>$null) -join '').Trim()
        if ($LASTEXITCODE -ne 0) { throw 'Version command failed' }
    } catch { $report.errors += 'Host version: ' + $_.Exception.Message }
} else { $report.errors += 'Butterpollo executable not found in the selected installation directory' }
try {
    $service = Get-Service -Name ApolloService -ErrorAction Stop
    $report.service_state = [string]$service.Status
} catch { $report.errors += 'Host service is not installed or cannot be queried' }
$destination = [IO.Path]::GetFullPath($OutputFile)
if (Test-Path -LiteralPath $destination) { throw 'Output exists; select a new report filename' }
$json = $report | ConvertTo-Json -Depth 8
[IO.File]::WriteAllText($destination, $json, [Text.UTF8Encoding]::new($false))
Write-Output ('Saved environment report: ' + $destination)
