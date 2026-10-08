[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ArtifactDirectory,
    [string]$MsysRoot = 'C:\msys64',
    [string]$MoonlightRoot = (Join-Path $PSScriptRoot '..\..\third-party\moonlight-common-c')
)
$ErrorActionPreference = 'Stop'
$env:PATH = "$MsysRoot\ucrt64\bin;" + $env:PATH
if (!(Test-Path -LiteralPath (Join-Path $MoonlightRoot 'src\Limelight.h'))) {
    throw 'Initialize the Moonlight-common-c submodule, or pass -MoonlightRoot pointing to its source checkout'
}
New-Item -ItemType Directory -Path $ArtifactDirectory -Force | Out-Null
$ArtifactDirectory = (Resolve-Path -LiteralPath $ArtifactDirectory).ProviderPath
$MoonlightRoot = (Resolve-Path -LiteralPath $MoonlightRoot).ProviderPath
$build = Join-Path $ArtifactDirectory 'moonlight-client-build'
& cmake -S $MoonlightRoot -B $build -G Ninja -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DCMAKE_C_COMPILER=gcc "-DOPENSSL_ROOT_DIR=$MsysRoot/ucrt64"
if ($LASTEXITCODE) { throw 'Moonlight-common-c configuration failed' }
& cmake --build $build
if ($LASTEXITCODE) { throw 'Moonlight-common-c build failed' }
& gcc -O2 (Join-Path $PSScriptRoot 'moonlight_client.c') "-I$MoonlightRoot/src" "-I$MsysRoot/ucrt64/include" "$build/libmoonlight-common-c.a" "$build/enet/libenet.a" "-L$MsysRoot/ucrt64/lib" -lavcodec -lavutil -lopus -lssl -lcrypto -lws2_32 -lwinmm -o (Join-Path $ArtifactDirectory 'moonlight-client.exe')
if ($LASTEXITCODE) { throw 'Moonlight decoding fixture build failed' }
& gcc -O2 (Join-Path $PSScriptRoot 'moonlight_client_test.c') "-I$MoonlightRoot/src" "-I$MsysRoot/ucrt64/include" "$build/libmoonlight-common-c.a" "$build/enet/libenet.a" "-L$MsysRoot/ucrt64/lib" -lavcodec -lavutil -lopus -lssl -lcrypto -lws2_32 -lwinmm -o "$build/moonlight-client-test.exe"
if ($LASTEXITCODE) { throw 'Moonlight fixture test build failed' }
& "$build/moonlight-client-test.exe"
if ($LASTEXITCODE) { throw 'Moonlight fixture tests failed' }
Write-Output "Independent client: $ArtifactDirectory\moonlight-client.exe"
