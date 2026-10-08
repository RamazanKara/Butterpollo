param(
    [Parameter(Mandatory=$true)][string]$ArtifactDirectory,
    [string]$NanorsDirectory,
    [string]$Gcc = 'C:\msys64\ucrt64\bin\gcc.exe'
)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
if (!$NanorsDirectory) { $NanorsDirectory = Join-Path $repo 'third-party\nanors' }
$ArtifactDirectory = [IO.Path]::GetFullPath($ArtifactDirectory)
$reference = Join-Path $ArtifactDirectory 'cpp-nanors-reference'
New-Item -ItemType Directory -Path "$reference\src", "$reference\third-party\nanors\deps\obl" -Force | Out-Null
# Verify all compiled source/header bytes against the original baseline. CRLF
# checkouts are normalized to LF; the original ISA dispatch stays unchanged.
$files = @(
    @('src/rswrapper.c', '334edab54b75d64e597c13655eaaed93dce116c8e469c0dffa6e657c1c12d686'),
    @('src/rswrapper.h', 'f9e3a6924872f03f28831b3dd0d103e4529e82978b00aa8c18e2b97352da6c8c'),
    @('third-party/nanors/rs.c', 'c76534113dfcf58a94be34830254b8c1291ff73f96432ee22d76afb8cd161743'),
    @('third-party/nanors/rs.h', '8bdacd178fe33c88c8305cda721fda245bc743d3ccdabc3bbc70f707e2fa5a34'),
    @('third-party/nanors/deps/obl/autoshim.h', '1a421b05e7991572ea0591cf61c04785c0b64746e61d222e68d09b637e438806'),
    @('third-party/nanors/deps/obl/gf2_8_mul_table.h', '3e1edf62873f2dfbc6c2ac076c325fbdba014d7cfeaadb4cd6dad1b2cc142e20'),
    @('third-party/nanors/deps/obl/gf2_8_tables.h', 'ed4480ba7cb72217724b24ae2cb51c58c74a10456bb0d7c8317f9a5e613a31db'),
    @('third-party/nanors/deps/obl/oblas_lite.c', '024b32387535274db0354d0f1487574cae26b6dfb5609f6371553213a5e178d2'),
    @('third-party/nanors/deps/obl/oblas_lite.h', 'cc16f537d6b3c9dfbc639e7fc5c32dc1c8d13018faa5d85ae68bf5ace27463f5')
)
$encoding = [Text.UTF8Encoding]::new($false)
foreach ($entry in $files) {
    $relative, $expected = $entry
    $source = if ($relative.StartsWith('third-party/nanors/')) { Join-Path $NanorsDirectory $relative.Substring('third-party/nanors/'.Length) } else { Join-Path $PSScriptRoot "fec-reference\$(Split-Path -Leaf $relative)" }
    $text = [IO.File]::ReadAllText($source).Replace("`r`n", "`n")
    $sha = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($encoding.GetBytes($text))).ToLowerInvariant()
    if ($sha -ne $expected) { throw "Reference source differs from the pinned C++ baseline: $relative" }
    [IO.File]::WriteAllText((Join-Path $reference $relative), $text, $encoding)
}
$adapter = @'
#include "src/rswrapper.h"
__declspec(dllexport) void fec_reference_init(void) { reed_solomon_init(); }
__declspec(dllexport) int fec_reference_encode(int data, int parity, unsigned char **shards, int bytes) {
    reed_solomon *matrix = reed_solomon_new(data, parity);
    if (!matrix) return -1;
    int result = reed_solomon_encode(matrix, shards, data + parity, bytes);
    reed_solomon_release(matrix);
    return result;
}
'@
[IO.File]::WriteAllText("$reference\reference.c", $adapter, $encoding)
$flags = @('-O3', '-ftree-vectorize', '-funroll-loops', '-std=c11', '-shared', '-Wl,--export-all-symbols', '-static-libgcc')
$compilerPath = (Get-Command $Gcc -ErrorAction Stop).Source
$previousPath = $env:PATH
try {
    $env:PATH = (Split-Path -Parent $compilerPath) + ';' + $previousPath
    if ((& $Gcc -dumpmachine) -ne 'x86_64-w64-mingw32') { throw 'Use the MSYS2 UCRT64 x64 compiler for this reference' }
    & $Gcc @flags -I "$reference\third-party\nanors" -I "$reference\third-party\nanors\deps\obl" "$reference\src\rswrapper.c" "$reference\reference.c" -o "$reference\nanors-reference.dll" 2> "$reference\compile.log"
    if ($LASTEXITCODE -ne 0) { Get-Content "$reference\compile.log"; throw 'Pinned C++ FEC reference build failed' }
    $compilerVersion = & $Gcc --version | Select-Object -First 1
} finally { $env:PATH = $previousPath }
$metadata = [ordered]@{
    wrapper_commit = 'f23ee0c9e7857887be7f774de6ac5153500a7e53'
    nanors_commit = '19f07b513e924e471cadd141943c1ec4adc8d0e0'
    compiler = $compilerVersion
    flags = $flags
    dispatch = 'Unchanged original SSSE3/AVX2/AVX512BW runtime dispatch'
    processor = (Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty Name)
    reference_sha256 = (Get-FileHash "$reference\nanors-reference.dll" -Algorithm SHA256).Hash.ToLowerInvariant()
}
$metadata | ConvertTo-Json -Depth 5 | Set-Content "$reference\reference.json" -Encoding utf8
Write-Output "$reference\nanors-reference.dll"
