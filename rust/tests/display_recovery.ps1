param([Parameter(Mandatory)][string]$Binary, [Parameter(Mandatory)][string]$Directory)
$ErrorActionPreference = 'Stop'
$Binary = (Resolve-Path -LiteralPath $Binary).Path
New-Item -ItemType Directory -Path $Directory -Force | Out-Null
$Directory = (Resolve-Path -LiteralPath $Directory).Path
$journal = Join-Path $Directory 'display-recovery.json'
foreach ($newOwner in @($false, $true)) {
    $parent = Start-Process -FilePath "$env:SystemRoot\System32\cmd.exe" -ArgumentList '/D','/C','ping -n 30 127.0.0.1 >nul' -WindowStyle Hidden -PassThru
    $watcher = $null
    try {
        @{pid=$parent.Id;started=$parent.StartTime.ToFileTimeUtc();entries=@{}} | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $journal -Encoding utf8NoBOM
        $watcher = Start-Process -FilePath $Binary -ArgumentList '--display-watch',$parent.Id,'--config-dir',$Directory -WindowStyle Hidden -PassThru
        Start-Sleep -Milliseconds 300
        if ($newOwner) { @{pid=$PID;started=(Get-Process -Id $PID).StartTime.ToFileTimeUtc();entries=@{}} | ConvertTo-Json | Set-Content -LiteralPath $journal -Encoding utf8NoBOM }
        Stop-Process -InputObject $parent
        if (!$watcher.WaitForExit(10000)) { throw 'Display watcher failed to exit after parent death' }
        if ($watcher.ExitCode -ne 0) { throw "Display watcher failed: $($watcher.ExitCode)" }
        $state = Get-Content -LiteralPath $journal -Raw | ConvertFrom-Json
        if ($newOwner -and $state.pid -ne $PID) { throw 'Old watcher overwrote a new owner journal' }
        if (!$newOwner -and ($state.pid -ne 0 -or @($state.entries.PSObject.Properties).Count -ne 0)) { throw 'Stale journal was not recovered' }
    } finally {
        if (!$parent.HasExited) { Stop-Process -InputObject $parent }
        if ($watcher -and !$watcher.HasExited) { Stop-Process -InputObject $watcher }
    }
}
Write-Output 'DISPLAY RECOVERY PASS: abrupt parent death, stale journal recovery, newer owner preserved; no physical display changes'
