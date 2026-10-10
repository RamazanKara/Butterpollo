[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ArtifactDirectory,
    [Parameter(Mandatory)][string]$PyrowaveRoot,
    [string]$MsysRoot = 'C:\msys64'
)
$ErrorActionPreference = 'Stop'
$env:PATH = "$MsysRoot\ucrt64\bin;" + $env:PATH
New-Item -ItemType Directory -Path $ArtifactDirectory -Force | Out-Null
$ArtifactDirectory = (Resolve-Path -LiteralPath $ArtifactDirectory).ProviderPath
$PyrowaveRoot = (Resolve-Path -LiteralPath $PyrowaveRoot).ProviderPath
function Assert-Exit([string]$Operation) {
    if ($LASTEXITCODE) { throw "$Operation failed ($LASTEXITCODE)" }
}
function Pinned-Source([string]$Path, [string]$Url, [string]$Commit) {
    if (!(Test-Path -LiteralPath (Join-Path $Path '.git'))) {
        if ((Test-Path -LiteralPath $Path) -and (Get-ChildItem -LiteralPath $Path -Force | Select-Object -First 1)) { throw "Source directory already exists: $Path" }
        & git init --quiet $Path
        Assert-Exit 'Source initialization'
        & git -C $Path remote add origin $Url
        Assert-Exit 'Source remote'
        & git -C $Path fetch --quiet --depth 1 origin $Commit
        Assert-Exit 'Pinned source fetch'
        & git -C $Path -c advice.detachedHead=false checkout --quiet --detach FETCH_HEAD
        Assert-Exit 'Pinned source checkout'
    }
    $actual = & git -C $Path rev-parse HEAD
    Assert-Exit 'Source identity'
    if ($actual -ne $Commit) { throw "Unexpected source revision in $Path; use an empty artifact directory" }
    $changes = & git -C $Path status --porcelain --untracked-files=no
    Assert-Exit 'Source status'
    if ($changes) { throw "Pinned client source has local changes: $Path" }
}
$source = Join-Path $ArtifactDirectory 'moonlight-vrr-common'
Pinned-Source $source 'https://github.com/Nonary/moonlight-common-c.git' 'd6a11bc685b41037b352a96f29d08276fe5359ba'
Pinned-Source (Join-Path $source 'enet') 'https://github.com/cgutman/enet.git' 'aca87840b57f045a1f7f9299e4b1b9b8e2a5e2f1'
Pinned-Source (Join-Path $source 'nanors') 'https://github.com/sleepybishop/nanors.git' 'b1e3c22ca0cdc0bb83e3cd6ed1a2fc77869ed99a'
$info = [IO.File]::ReadAllText((Join-Path $PyrowaveRoot 'share\pyrowave-shared\build-info.txt'))
# Vibepollo 2.0's SDK and the 1.0 SDK the host ships produce the same 186f0393 bitstream.
if (!($info -match '(?m)^pyrowave_commit=(186f0393b77f7755953b5ecde994bb1cec2e4155|502a3b52a39312ab82c85b1e2fc0e746faee91a4)\r?$')) { throw 'The fixture requires a PyroWave SDK with bitstream 186f0393 (commit 186f0393 or 502a3b52)' }
$build = Join-Path $ArtifactDirectory 'moonlight-vrr-common-build'
& cmake -S $source -B $build -G Ninja -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DCMAKE_C_COMPILER=gcc "-DOPENSSL_ROOT_DIR=$MsysRoot/ucrt64"
Assert-Exit 'Client transport configuration'
& cmake --build $build
Assert-Exit 'Client transport build'
& gcc -O2 -Wall -Wextra -Werror -Wno-unused-parameter -Wno-misleading-indentation -DBUTTERPOLLO_PYROWAVE (Join-Path $PSScriptRoot 'moonlight_client.c') "-I$source/src" "-I$PyrowaveRoot/include" "-I$MsysRoot/ucrt64/include" "$build/libmoonlight-common-c.a" "$build/enet/libenet.a" "-L$PyrowaveRoot/lib" "-L$MsysRoot/ucrt64/lib" -lpyrowave-shared -lavcodec -lavutil -lopus -lcrypto -lws2_32 -lwinmm -o (Join-Path $ArtifactDirectory 'moonlight-pyrowave-client.exe')
Assert-Exit 'Vendor decoding fixture build'
Copy-Item -Path (Join-Path $PyrowaveRoot 'bin\libpyrowave-shared-*.dll') -Destination $ArtifactDirectory
Write-Output "Independent Nonary transport/vendor decoder: $ArtifactDirectory\moonlight-pyrowave-client.exe"
