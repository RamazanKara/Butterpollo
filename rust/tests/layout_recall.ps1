# Checks that a stream's exclusive layout survives Windows recalling its saved
# layout, which is what happens when an exclusive-fullscreen game loses focus
# (the Win key, Alt+Tab, Ctrl+Alt+Del): the physical monitor comes back on and
# the host must switch it off again within about a second.
#
# Run on the host while one virtual display stream with the exclusive layout is
# running (only the virtual display active). The script makes Windows apply the
# layout in its display database, then watches the number of active displays.
# The host saves the stream's layout to that database, so the monitor should
# not come on at all; a build without that (a588457) shows
# it coming on, which confirms the recall reproduces the focus-loss case.
param([int]$Rounds = 3, [int]$LimitMs = 1500, [int]$WatchMs = 5000)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class LayoutRecall {
    [DllImport("user32.dll")] static extern int GetDisplayConfigBufferSizes(uint flags, out uint paths, out uint modes);
    [DllImport("user32.dll")] static extern int SetDisplayConfig(uint paths, IntPtr pathArray, uint modes, IntPtr modeArray, uint flags);
    const uint QDC_ONLY_ACTIVE_PATHS = 0x2;
    const uint SDC_USE_DATABASE_CURRENT = 0xF;
    const uint SDC_APPLY = 0x80;
    public static int Active() {
        uint paths, modes;
        int error = GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, out paths, out modes);
        return error == 0 ? (int)paths : -1;
    }
    public static int Recall() {
        return SetDisplayConfig(0, IntPtr.Zero, 0, IntPtr.Zero, SDC_APPLY | SDC_USE_DATABASE_CURRENT);
    }
}
'@
$baseline = [LayoutRecall]::Active()
if ($baseline -ne 1) { throw "Expected only the stream's virtual display active (exclusive layout), found $baseline active displays" }
$results = @()
for ($round = 1; $round -le $Rounds; $round++) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $status = [LayoutRecall]::Recall()
    if ($status -ne 0) { throw "SetDisplayConfig(SDC_USE_DATABASE_CURRENT) failed: $status" }
    $on = $null
    $off = $null
    while ($clock.ElapsedMilliseconds -lt $WatchMs) {
        $active = [LayoutRecall]::Active()
        if ($null -eq $on -and $active -gt $baseline) { $on = $clock.ElapsedMilliseconds }
        if ($null -ne $on -and $active -eq $baseline) { $off = $clock.ElapsedMilliseconds; break }
        Start-Sleep -Milliseconds 20
    }
    if ($null -eq $on) {
        Write-Output "round ${round}: the recall left the layout as it was (no display came on)"
    } elseif ($null -eq $off) {
        Write-Output "round ${round}: a display came on at $on ms and stayed on for ${WatchMs} ms"
    } else {
        Write-Output "round ${round}: a display came on at $on ms and was off again at $off ms ($($off - $on) ms)"
    }
    $results += [pscustomobject]@{ round = $round; on = $on; off = $off }
    Start-Sleep -Seconds 3
}
$failed = @($results | Where-Object { $null -ne $_.on -and ($null -eq $_.off -or ($_.off - $_.on) -gt $LimitMs) })
$reproduced = @($results | Where-Object { $null -ne $_.on }).Count
if ($failed.Count -ne 0) { throw "LAYOUT RECALL FAIL: $($failed.Count) of $Rounds rounds kept a display on longer than $LimitMs ms" }
if ($reproduced -eq 0) { Write-Output "LAYOUT RECALL PASS: no display came on in $Rounds recalls (the saved layout is the stream's)"; exit 0 }
Write-Output "LAYOUT RECALL PASS (watchdog): $reproduced of $Rounds recalls switched a display on and the host switched it off within $LimitMs ms"
