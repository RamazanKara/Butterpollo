#Requires -Version 7
<#
.SYNOPSIS
Run isolated stream, reconnect and fault checks without changing the service.
.EXAMPLE
pwsh rust/release/soak.ps1 -Package 'C:\Program Files\ButterpolloRust' -Client target/soak-fixtures/moonlight-client.exe -GpuLoad target/qa/release/examples/gpu_load.exe
.EXAMPLE
pwsh rust/release/soak.ps1 -Mode long -Package target/soak-package -Client target/soak-fixtures/moonlight-client.exe -GpuLoad target/qa/release/examples/gpu_load.exe
#>
param(
    [ValidateSet('short', 'long')][string] $Mode = 'short',
    [Parameter(Mandatory)][string] $Package,
    [Parameter(Mandatory)][string] $Client,
    [Parameter(Mandatory)][string] $GpuLoad,
    [string] $Work = (Join-Path $PSScriptRoot '../../target/soak')
)
$ErrorActionPreference = 'Stop'
$python = if ($env:BUTTERPOLLO_TEST_PYTHON) { $env:BUTTERPOLLO_TEST_PYTHON } else { 'python' }
& $python "$PSScriptRoot/soak.py" --mode $Mode --package $Package --client $Client --gpu-load $GpuLoad --work $Work
exit $LASTEXITCODE
