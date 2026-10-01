[CmdletBinding()]
param(
    [string]$MsysRoot = 'C:\msys64',
    [string]$Dependencies = "$env:LOCALAPPDATA\ButterpolloRust\sdk",
    [string]$TargetDirectory = "$env:LOCALAPPDATA\ButterpolloRust\target",
    [string]$FfmpegRoot = $env:BUTTERPOLLO_FFMPEG_ROOT,
    [string]$PyrowaveRoot = $env:BUTTERPOLLO_PYROWAVE_ROOT,
    [string]$NvidiaRoot = $env:NV_RTX_VIDEO_SDK,
    [string]$MsvcSdk = $env:BUTTERPOLLO_MSVC_ROOT,
    [switch]$FetchDependencies,
    [switch]$DebugBuild,
    [switch]$SkipTrueHdr,
    [switch]$SkipTests,
    [switch]$Package
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$toolchain = '+1.98.1-x86_64-pc-windows-gnu'
function Assert-NativeExit([string]$Operation) {
    if ($LASTEXITCODE -ne 0) { throw "$Operation failed with exit code $LASTEXITCODE" }
}
function Get-PinnedArchive([string]$Url, [string]$Destination, [string]$Hash) {
    if (!(Test-Path -LiteralPath $Destination)) { Invoke-WebRequest -Uri $Url -OutFile $Destination -TimeoutSec 180 }
    if ((Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Hash) {
        throw "Checksum mismatch: $Destination"
    }
}
New-Item -ItemType Directory -Path $Dependencies, $TargetDirectory -Force | Out-Null
$Dependencies = (Resolve-Path -LiteralPath $Dependencies).ProviderPath
$TargetDirectory = (Resolve-Path -LiteralPath $TargetDirectory).ProviderPath
$env:PATH = "$MsysRoot\ucrt64\bin;$env:USERPROFILE\.cargo\bin;" + $env:PATH
$env:LIBCLANG_PATH = "$MsysRoot\ucrt64\bin"
$env:BUTTERPOLLO_SYSTEM_LIBS = "$MsysRoot\ucrt64\lib"
$env:BUTTERPOLLO_VULKAN_INCLUDE = "$MsysRoot\ucrt64\include"
$env:CARGO_TARGET_DIR = $TargetDirectory
if ($FetchDependencies) {
    if (!$FfmpegRoot) {
        $archive = Join-Path $Dependencies 'ffmpeg-v2026.516.30821.tar.gz'
        Get-PinnedArchive 'https://github.com/LizardByte/build-deps/releases/download/v2026.516.30821/Windows-AMD64-ffmpeg.tar.gz' $archive '2f7a2c2fc6be9b96de3c6f654389f73a5e5d369d7e802d017894fae96247661d'
        $directory = Join-Path $Dependencies 'ffmpeg-v2026.516.30821'
        New-Item -ItemType Directory -Path $directory -Force | Out-Null
        & tar -xzf $archive -C $directory
        Assert-NativeExit 'FFmpeg extraction'
        $FfmpegRoot = (Get-ChildItem -LiteralPath $directory -Filter avcodec.h -Recurse | Select-Object -First 1).Directory.Parent.Parent.FullName
    }
    if (!$PyrowaveRoot) {
        $PyrowaveRoot = Join-Path $Dependencies 'pyrowave-89f7e47'
        $cygpath = "$MsysRoot\usr\bin\cygpath.exe"
        $prefix = & $cygpath -u $PyrowaveRoot
        $buildScript = & $cygpath -u (Join-Path $repo 'scripts\build_pyrowave.sh')
        $env:PYROWAVE_WORKDIR = & $cygpath -u (Join-Path $Dependencies 'pyrowave-work')
        $env:MSYSTEM = 'UCRT64'
        & "$MsysRoot\usr\bin\bash.exe" $buildScript '89f7e47d4abbf650c91fae766728af866c5e32a0' $prefix
        Assert-NativeExit 'PyroWave SDK build'
    }
    if (!$SkipTrueHdr -and !$NvidiaRoot) {
        $archive = Join-Path $Dependencies 'RTX_Video_SDK_v1.1.0.zip'
        if (!(Test-Path -LiteralPath $archive)) {
            $redirect = Invoke-WebRequest 'https://api.ngc.nvidia.com/v2/models/nvidia/multimedia/dlpp/versions/1.5/files/RTX_Video_SDK_v1.1.0.zip' -TimeoutSec 180
            Invoke-WebRequest -Uri ([string]$redirect.Headers.Location) -OutFile $archive -TimeoutSec 180
        }
        if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne 'abf4f34e2b5a618e355b0d5a0365d8ecc3db4396e756e4c850a867e1ae2ed69e') { throw 'NVIDIA SDK checksum mismatch' }
        $NvidiaRoot = Join-Path $Dependencies 'ngx-1.1.0'
        Expand-Archive -LiteralPath $archive -DestinationPath $NvidiaRoot -Force
    }
}
if (!$FfmpegRoot -or !$PyrowaveRoot) { throw 'Provide FFmpeg/PyroWave SDK paths, or use -FetchDependencies' }
$env:BUTTERPOLLO_FFMPEG_ROOT = $FfmpegRoot
$env:BUTTERPOLLO_PYROWAVE_ROOT = $PyrowaveRoot
Push-Location -LiteralPath $repo
try {
    & cargo $toolchain fmt --all -- --check
    Assert-NativeExit 'Rust formatting'
    if (!$SkipTests) {
        & cargo $toolchain test --workspace --locked
        Assert-NativeExit 'Rust tests'
        & cargo $toolchain clippy --workspace --all-targets --locked -- -D warnings
        Assert-NativeExit 'Rust lint'
    }
    $buildArgs = @('build', '--workspace', '--locked')
    $profile = 'debug'
    if (!$DebugBuild) { $buildArgs += '--release'; $profile = 'release' }
    & cargo $toolchain @buildArgs
    Assert-NativeExit 'Rust host build'
    $probeArgs = @('build', '-p', 'butterpollo-windows', '--example', 'performance', '--locked')
    if (!$DebugBuild) { $probeArgs += '--release' }
    & cargo $toolchain @probeArgs
    Assert-NativeExit 'Rust performance probe build'
    $output = Join-Path $TargetDirectory $profile
    $runtimeDlls = @('libopus-0.dll','libvpl-2.dll','libstdc++-6.dll','libgcc_s_seh-1.dll','libwinpthread-1.dll','libpyrowave-shared-0.dll')
    foreach ($dll in $runtimeDlls | Where-Object { $_ -ne 'libpyrowave-shared-0.dll' }) {
        Copy-Item -LiteralPath (Join-Path "$MsysRoot\ucrt64\bin" $dll) -Destination $output
    }
    Copy-Item -LiteralPath (Join-Path $PyrowaveRoot 'bin\libpyrowave-shared-0.dll') -Destination $output
    if (!$SkipTrueHdr) {
        if (!$NvidiaRoot) { throw 'NVIDIA SDK path is required for the TrueHDR adapter' }
        $env:NV_RTX_VIDEO_SDK = $NvidiaRoot
        if ($MsvcSdk) {
            $sysroot = & rustc $toolchain --print sysroot
            $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = Join-Path $sysroot 'lib\rustlib\x86_64-pc-windows-gnu\bin\rust-lld.exe'
            $env:LIB = "$MsvcSdk\crt\lib\x86_64;$MsvcSdk\sdk\lib\um\x86_64;$MsvcSdk\sdk\lib\ucrt\x86_64;" + $env:LIB
        }
        & cargo $toolchain fmt --manifest-path rust/truehdr-runtime/Cargo.toml -- --check
        Assert-NativeExit 'TrueHDR formatting'
        if (!$SkipTests) {
            & cargo $toolchain clippy --manifest-path rust/truehdr-runtime/Cargo.toml --target x86_64-pc-windows-msvc --locked -- -D warnings
            Assert-NativeExit 'TrueHDR lint'
        }
        & cargo $toolchain build --manifest-path rust/truehdr-runtime/Cargo.toml --target x86_64-pc-windows-msvc --release --locked
        Assert-NativeExit 'Rust TrueHDR adapter build'
        Copy-Item -LiteralPath (Join-Path $TargetDirectory 'x86_64-pc-windows-msvc\release\butterpollo_truehdr.dll') -Destination $output
        Copy-Item -LiteralPath (Join-Path $NvidiaRoot 'bin\Windows\x64\rel\nvngx_truehdr.dll') -Destination $output
        $runtimeDlls += @('butterpollo_truehdr.dll','nvngx_truehdr.dll')
    }
    if ($Package) {
        $distribution = Join-Path $TargetDirectory "butterpollo-rust-$profile"
        if (Test-Path -LiteralPath $distribution) {
            $resolvedDistribution = (Resolve-Path -LiteralPath $distribution).ProviderPath
            if (!$resolvedDistribution.StartsWith($TargetDirectory.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Package cleanup escaped its output directory' }
            Remove-Item -LiteralPath $resolvedDistribution -Recurse -Force
        }
        New-Item -ItemType Directory -Path "$distribution\assets\web", "$distribution\licenses" -Force | Out-Null
        foreach ($exe in @('butterpollo.exe', 'butterpollo-service.exe')) { Copy-Item -LiteralPath (Join-Path $output $exe) -Destination $distribution }
        Copy-Item -LiteralPath (Join-Path $output 'examples\performance.exe') -Destination (Join-Path $distribution 'butterpollo-performance.exe')
        foreach ($dll in $runtimeDlls) { Copy-Item -LiteralPath (Join-Path $output $dll) -Destination $distribution }
        New-Item -ItemType Directory -Path "$distribution\vulkan-layer" -Force | Out-Null
        Copy-Item -LiteralPath (Join-Path $output 'butterpollo_vulkan_layer.dll') -Destination "$distribution\vulkan-layer"
        Copy-Item -LiteralPath (Join-Path $repo 'rust\vulkan-layer\VkLayer_butterpollo_hdr.json') -Destination "$distribution\vulkan-layer"
        Get-ChildItem -LiteralPath (Join-Path $repo 'src_assets\common\assets') -File | Where-Object { $_.Extension -in '.png','.ico' } | Copy-Item -Destination "$distribution\assets"
        Copy-Item -LiteralPath (Join-Path $repo 'src_assets\common\assets\remote-session') -Destination "$distribution\assets\remote-session" -Recurse -Force
        Copy-Item -LiteralPath (Join-Path $repo 'LICENSE') -Destination "$distribution\licenses\Butterpollo.txt"
        Copy-Item -LiteralPath (Join-Path $repo 'rust\README.md') -Destination $distribution
        Copy-Item -LiteralPath (Join-Path $repo 'rust\PERFORMANCE.md') -Destination $distribution
        Copy-Item -LiteralPath (Join-Path $repo 'rust\PARITY.md') -Destination $distribution
        Copy-Item -LiteralPath (Join-Path $repo 'rust\THIRD_PARTY.md') -Destination "$distribution\licenses"
        Copy-Item -LiteralPath (Join-Path $repo 'rust\service.ps1') -Destination $distribution
        Copy-Item -LiteralPath (Join-Path $repo 'Cargo.lock') -Destination "$distribution\licenses"
        if (Test-Path -LiteralPath (Join-Path $PyrowaveRoot 'share\licenses')) { Copy-Item -LiteralPath (Join-Path $PyrowaveRoot 'share\licenses') -Destination "$distribution\licenses\pyrowave" -Recurse -Force }
        foreach ($library in @('gcc-libs','libwinpthread','winpthreads','libvpl','opus','svt-av1')) {
            $source = Join-Path "$MsysRoot\ucrt64\share\licenses" $library
            if (Test-Path -LiteralPath $source) { Copy-Item -LiteralPath $source -Destination "$distribution\licenses\$library" -Recurse -Force }
        }
        if (!$SkipTrueHdr) { Copy-Item -LiteralPath (Join-Path $NvidiaRoot 'NVIDIA_RTX_Video_SDK_License.pdf') -Destination "$distribution\licenses" }
        $metadata = & cargo $toolchain metadata --format-version 1 --locked
        Assert-NativeExit 'Dependency manifest'
        ($metadata | ConvertFrom-Json).packages | Select-Object name, version, license, repository, source | ConvertTo-Json | Set-Content -LiteralPath "$distribution\licenses\rust-dependencies.json" -Encoding utf8
        $manifest = Get-ChildItem -LiteralPath $distribution -File -Recurse | ForEach-Object {
            [pscustomobject]@{ path = $_.FullName.Substring($distribution.Length + 1); sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
        }
        $manifest | ConvertTo-Json | Set-Content -LiteralPath "$distribution\manifest.json" -Encoding utf8
        Compress-Archive -LiteralPath $distribution -DestinationPath "$TargetDirectory\butterpollo-rust-$profile.zip" -Force
        Write-Output "Package: $TargetDirectory\butterpollo-rust-$profile.zip"
    }
    Write-Output "Rust executables: $output"
} finally { Pop-Location }
