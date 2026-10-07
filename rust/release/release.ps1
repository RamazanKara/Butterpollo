#Requires -Version 7
<#
.SYNOPSIS
Build, verify, install and publish a Butterpollo release in one run.

.DESCRIPTION
1. Checks out -Ref in a Windows (NTFS) checkout; building over \\wsl.localhost
   is several times slower.
2. Formatting, then tests + clippy and the release build in parallel, each in
   its own target directory.
3. Packages from the previous published release: only the rebuilt binaries,
   documentation, lock file and versions change (package.py).
4. Streams H.264, HEVC and AV1 through the packaged host with the independent
   moonlight-common-c client, and runs the protocol checks (e2e.py, protocol.py).
5. The display self-test as SYSTEM and a quiet install over the running host
   (elevated.ps1): run directly from an elevated shell, otherwise through the
   task elevation.ps1 installs, otherwise after one UAC prompt.
6. Records the results (finalize.py), tags the commit and publishes without
   waiting for CI, which verifies the same commit; then downloads every asset
   and checks it against SHA256SUMS.

The release notes come from the "## New in <rc>" section of
rust/RELEASE_NOTES.md unless -Notes names a file.

.EXAMPLE
pwsh rust/release/release.ps1
.EXAMPLE
pwsh rust/release/release.ps1 -NoInstall -NoPublish
#>
param(
    [string] $Ref = 'origin/main',
    [string] $Checkout = 'C:\src\butterpollo',
    # Not under %LOCALAPPDATA%: a packaged (MSIX) app such as the Claude
    # desktop app sees its own copy of that folder, so the SYSTEM self-test
    # could not find a package built from inside it.
    [string] $Work = 'C:\src\butterpollo-release',
    # Machine settings, dot-sourced: the Rust build environment (PATH, FFmpeg,
    # PyroWave, ...), BUTTERPOLLO_TEST_CLIENT_EXE and BUTTERPOLLO_TEST_PYTHON.
    [string] $Settings = (Join-Path $Work 'settings.ps1'),
    [string] $Notes,
    [string] $Scope,
    [switch] $NoInstall,
    [switch] $NoPublish
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$repo = 'RamazanKara/Butterpollo'
$started = Get-Date
function Step($name) { Write-Host ('[{0:mm\:ss}] {1}' -f ((Get-Date) - $started), $name) -ForegroundColor Cyan }

if (Test-Path $Settings) { . $Settings }
$python = if ($env:BUTTERPOLLO_TEST_PYTHON) { $env:BUTTERPOLLO_TEST_PYTHON } else { 'python' }
$client = $env:BUTTERPOLLO_TEST_CLIENT_EXE
if (-not $client -or -not (Test-Path $client)) {
    throw 'Set BUTTERPOLLO_TEST_CLIENT_EXE to the moonlight-common-c test client (rust/tests/moonlight_client.c).'
}

Step "checkout $Ref"
if (-not (Test-Path $Checkout)) { git clone --quiet --filter=blob:none "https://github.com/$repo.git" $Checkout }
git -C $Checkout fetch --quiet --tags origin
if (git -C $Checkout status --porcelain) { throw "$Checkout has local changes" }
# The checkout is also where development happens: stay on the branch when it
# is already at the commit being released.
$wanted = git -C $Checkout rev-parse "$Ref^{commit}"
if ((git -C $Checkout rev-parse HEAD) -ne $wanted) { git -C $Checkout checkout --quiet --detach $wanted }
$sha = git -C $Checkout rev-parse HEAD
$version = [regex]::Match((Get-Content "$Checkout\Cargo.toml" -Raw), '(?m)^version = "([^"]+)"').Groups[1].Value
$releases = gh release list -R $repo --exclude-drafts --limit 50 --json tagName --jq '.[].tagName'
# A run that failed after tagging can be repeated; a published version cannot.
$tagged = git -C $Checkout ls-remote --tags origin "refs/tags/$version" "refs/tags/$version^{}"
$taggedAt = (@($tagged | Where-Object { $_ -like '*^{}' }) + @($tagged) | Select-Object -First 1) -replace '\s.*'
if (-not $NoPublish -and ($releases -contains $version -or ($taggedAt -and $taggedAt -ne $sha))) {
    throw "$version is already released or tagged at another commit; bump the version first."
}
$label = if ($version -match '-(rc\.\d+)$') { $Matches[1] } else { $version }
if (-not $Notes) {
    # Checked before building: the section becomes the release text.
    $history = Get-Content "$Checkout\rust\RELEASE_NOTES.md" -Raw
    $section = [regex]::Match($history, "(?ms)^## New in $([regex]::Escape($label))\s*\r?\n(.*?)(?=^## )")
    if (-not $section.Success -or $section.Groups[1].Value.Trim() -in '', '-') {
        throw "Fill in the '## New in $label' section of rust/RELEASE_NOTES.md."
    }
}
# The demo film the release text shows travels with every release, so
# deleting older releases leaves the page intact.
$film = 'butterpollo-launch-film.gif', 'butterpollo-launch-film.mp4' | ForEach-Object { Join-Path $Work "assets\$_" }
if (-not $NoPublish) {
    foreach ($file in $film) { if (-not (Test-Path $file)) { throw "$file is missing; the release text shows it" } }
}
$tools = "$Checkout\rust\release"
$run = Join-Path $Work $version
$out = "$run\release"
$qa = "$run\qa"
$target = "$Checkout\target"
New-Item -ItemType Directory -Force $qa | Out-Null
Write-Host "  $version at $sha"

$previous = $releases | Where-Object { $_ -ne $version } | Select-Object -First 1
$baseline = Join-Path $Work "baseline-$previous"
$baselineZip = "$baseline\butterpollo-rust-$previous-windows-x64.zip"
if (-not (Test-Path $baselineZip)) {
    Step "baseline $previous"
    gh release download $previous -R $repo -D $baseline --clobber -p (Split-Path $baselineZip -Leaf) -p SHA256SUMS
}

Step 'formatting'
cargo fmt --all --manifest-path "$Checkout\Cargo.toml" -- --check *> "$qa\fmt.log"

Step 'tests + clippy and release build, in parallel'
# Below normal priority so the desktop stays usable; restored before streaming.
$self = [Diagnostics.Process]::GetCurrentProcess()
$self.PriorityClass = 'BelowNormal'
$manifest = "$Checkout\Cargo.toml"
$jobs = @(
    Start-ThreadJob -Name checks -ArgumentList $manifest, "$target\qa", $qa {
        param($manifest, $dir, $qa)
        cargo test --workspace --locked --manifest-path $manifest --target-dir $dir *> "$qa\tests.log"
        if ($LASTEXITCODE) { return 'tests failed: ' + "$qa\tests.log" }
        cargo clippy --workspace --all-targets --locked --manifest-path $manifest --target-dir $dir -- -D warnings *> "$qa\clippy.log"
        if ($LASTEXITCODE) { return 'clippy failed: ' + "$qa\clippy.log" }
    }
    Start-ThreadJob -Name release -ArgumentList $manifest, "$target\ship", $qa {
        param($manifest, $dir, $qa)
        cargo build --release --workspace --locked --manifest-path $manifest --target-dir $dir *> "$qa\build.log"
        if ($LASTEXITCODE) { return 'release build failed: ' + "$qa\build.log" }
    }
)
$failures = $jobs | Wait-Job | Receive-Job
$jobs | Remove-Job
$self.PriorityClass = 'Normal'
if ($failures) { throw ($failures -join "`n") }
$tests = Select-String -Path "$qa\tests.log" -Pattern 'test result: ok\. (\d+) passed' |
    ForEach-Object { [int]$_.Matches[0].Groups[1].Value } | Measure-Object -Sum
Write-Host "  $($tests.Sum) tests passed"

Step "package from $previous"
& $python "$tools\package.py" --repo $Checkout --build "$target\ship\release" --baseline-zip $baselineZip `
    --baseline-sums "$baseline\SHA256SUMS" --qa $qa --out $out | Out-Null
$package = "$out\butterpollo-rust-release"

foreach ($codec in 'h264', 'hevc', 'av1') {
    Step "stream $codec"
    try {
        & $python "$tools\e2e.py" --package $package --work $run --client $client --codec $codec
    } catch {
        # Other load on the workstation can push one run under the frame-rate
        # floor; a regression fails twice. The first run's logs are kept.
        Write-Warning "stream $codec failed; running it once more"
        Remove-Item -Recurse -Force "$run\e2e-$codec-failed" -ErrorAction SilentlyContinue
        Move-Item "$run\e2e-$codec" "$run\e2e-$codec-failed"
        & $python "$tools\e2e.py" --package $package --work $run --client $client --codec $codec
    }
}
Step 'protocol checks'
& $python "$tools\protocol.py" --package $package --work $run

if (-not $NoInstall) {
    Remove-Item -Recurse -Force "$run\elevated" -ErrorAction SilentlyContinue
    # elevation.ps1 installs a task that runs this step without a prompt. It
    # is used while its scripts match this checkout's and -Work is the default.
    $taskHome = 'C:\ProgramData\ButterpolloRelease'
    $task = $false
    try { schtasks /Query /TN ButterpolloReleaseElevated *> $null; $task = $true } catch { }
    $current = $task -and $Work -eq 'C:\src\butterpollo-release' -and
        @('elevated.ps1', 'elevated-task.ps1' | Where-Object {
            -not (Test-Path "$taskHome\$_") -or (Get-FileHash "$taskHome\$_").Hash -ne (Get-FileHash "$tools\$_").Hash
        }).Count -eq 0
    $admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
    $arguments = '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$tools\elevated.ps1`"",
        '-Package', "`"$package`"", '-Installer', "`"$out\butterpollo-setup-$version.exe`"",
        '-Version', $version, '-Work', "`"$run`""
    if ($admin) {
        Step 'display self-test as SYSTEM and install'
        Start-Process powershell -WindowStyle Hidden -ArgumentList $arguments
    } elseif ($current) {
        Step 'display self-test as SYSTEM and install'
        @{ version = $version } | ConvertTo-Json | Set-Content (Join-Path $Work 'request.json')
        schtasks /Run /TN ButterpolloReleaseElevated | Out-Null
    } else {
        if ($task) { Write-Warning 'The release elevation task is out of date; run rust/release/elevation.ps1 as administrator again.' }
        Step 'display self-test as SYSTEM and install (UAC prompt)'
        Start-Process powershell -Verb RunAs -WindowStyle Hidden -ArgumentList $arguments
    }
    $deadline = (Get-Date).AddMinutes(15)
    while (-not (Test-Path "$run\elevated\done.txt")) {
        if ((Get-Date) -gt $deadline) { throw 'The elevated step did not finish; was the UAC prompt declined?' }
        Start-Sleep -Seconds 2
    }
    # Checked before tagging: a release is only published once it installed.
    if (-not (Test-Path "$run\elevated\installed.json")) {
        throw "The install did not finish; see $run\elevated\transcript.txt"
    }
    $selfTest = "$run\elevated\display-self-test.json"
    $selfTestPassed = $null
    if (Test-Path $selfTest) {
        $selfTestPassed = [bool](Get-Content $selfTest -Raw | ConvertFrom-Json).passed
        Write-Host "  display self-test passed: $selfTestPassed"
        if (-not $selfTestPassed) { Write-Warning "Display self-test failed: $selfTest" }
    } else {
        Write-Warning "The display self-test wrote no report; see $run\elevated\transcript.txt"
    }
}

if (-not $NoPublish -and -not $taggedAt) {
    Step "tag $version"
    # An annotated tag through the API: the checkout needs no push credentials.
    $tag = gh api "repos/$repo/git/tags" -f tag=$version -f "message=Butterpollo $version" -f object=$sha -f type=commit --jq .sha
    gh api "repos/$repo/git/refs" -f "ref=refs/tags/$version" -f sha=$tag | Out-Null
}

Step 'record results'
git -C $Checkout log --format=%s "$previous..$sha" | Set-Content "$run\changes.txt"
# Published without waiting for CI. The tag's own run verifies the commit and
# cannot be cancelled by later pushes to main; a dry run records main's run.
$ci = $null
$deadline = (Get-Date).AddSeconds($(if ($NoPublish) { 0 } else { 60 }))
do {
    if (-not $NoPublish) { Start-Sleep -Seconds 3 }
    $ci = gh run list -R $repo --workflow rust-windows.yml --branch $(if ($NoPublish) { 'main' } else { $version }) `
        --commit $sha --limit 1 --json status,conclusion,url,headSha --jq '.[0]'
} until ($ci -or (Get-Date) -gt $deadline)
Set-Content "$run\ci.json" "$ci"
if (-not $Scope) {
    $gpu = (Get-CimInstance Win32_VideoController | Where-Object Name -NotMatch 'Virtual|Basic|Idd' | Select-Object -First 1).Name
    $checks = 'End-to-end streams, protocol checks'
    if ($null -ne $selfTestPassed) { $checks += ", display self-test ($(if ($selfTestPassed) { 'passed' } else { 'failed' }))" }
    if (-not $NoInstall) { $checks += ' and install' }
    $Scope = "$checks on the release workstation ($gpu, Windows $([Environment]::OSVersion.Version.Build)). " +
        'Other hardware was not tested.'
}
$finalize = @('--out', $out, '--work', $run, '--changes', "$run\changes.txt", '--ci', "$run\ci.json", '--scope', $Scope)
if (-not $NoInstall) { $finalize += '--installed' }
& $python "$tools\finalize.py" @finalize

if (-not $Notes) {
    # Links relative to rust/ point at the tagged source.
    $text = [regex]::Replace($section.Groups[1].Value.Trim(), '\]\((?!https?:|#)([^)]+)\)',
        { param($m) "](https://github.com/$repo/blob/$version/rust/$($m.Groups[1].Value))" })
    $body = (Get-Content "$tools\body.md" -Raw).Replace('{version}', $version).Replace('{label}', $label).Replace('{notes}', $text)
    $Notes = "$run\body.md"
    Set-Content $Notes $body -NoNewline
}

if ($NoPublish) {
    Step "done, not published: $out"
    return
}
Step "publish $version"
$flags = @('--verify-tag', '--title', "Butterpollo $version", '--notes-file', $Notes)
if ($version -match '-') { $flags += '--prerelease' }
$assets = "butterpollo-setup-$version.exe", "butterpollo-rust-$version-windows-x64.zip", 'SHA256SUMS',
    'BUILD_PROVENANCE.json', 'VALIDATION.json', 'SOURCE-MANIFEST.json' | ForEach-Object { "$out\$_" }
$assets += $film
gh release create $version -R $repo @flags @assets | Out-Null

Step 'verify the published assets'
$verify = "$run\verify"
Remove-Item -Recurse -Force $verify -ErrorAction SilentlyContinue
gh release download $version -R $repo -D $verify
foreach ($line in Get-Content "$verify\SHA256SUMS") {
    $hash, $name = $line -split '\s+', 2
    if ((Get-FileHash "$verify\$name" -Algorithm SHA256).Hash -ne $hash) { throw "$name does not match SHA256SUMS" }
}
Step "released https://github.com/$repo/releases/tag/$version"
