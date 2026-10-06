# Release tooling

`release.ps1` builds, verifies, installs and publishes a release in one run, on the Windows release workstation:

```powershell
pwsh rust/release/release.ps1                         # release origin/main
pwsh rust/release/release.ps1 -NoInstall -NoPublish   # build, package and stream-test only
```

Before running it, bump the version with `python rust/release/bump.py 2.0.0-rc.N` (workspace version, `Cargo.lock`, READMEs, docs and release notes header), fill in the `## New in rc.N` section it adds to `rust/RELEASE_NOTES.md` (it becomes the GitHub release text unless `-Notes` names a file), and push to `main`. The release refuses to run with that section empty.

| Step | Script |
| --- | --- |
| Check out the commit in an NTFS checkout (`C:\src\butterpollo`); building over `\\wsl.localhost` is several times slower | `release.ps1` |
| Formatting, then tests + clippy and the release build in parallel, each in its own target directory | `release.ps1` |
| Package from the previous published release: only the rebuilt binaries, documentation, lock file and versions change, and every retained file is checked against the baseline's manifest | `package.py` |
| Stream H.264, HEVC and AV1 through the packaged host on an isolated profile with the independent moonlight-common-c client, then the pairing and launch protocol checks | `e2e.py`, `protocol.py` |
| One UAC prompt: the display self-test as SYSTEM, then a quiet install over the running host | `elevated.ps1` |
| Record the results in `VALIDATION.json` and `BUILD_PROVENANCE.json`, write `SHA256SUMS` | `finalize.py` |
| Tag through the GitHub API and publish without waiting for CI: the tag's own CI run verifies the same commit and later pushes to `main` cannot cancel it. Then download every asset and check it | `release.ps1` |

Machine settings live outside the repository in `%LOCALAPPDATA%\Butterpollo\release\settings.ps1`, which is dot-sourced: the Rust build environment (MSYS2 UCRT64 on `PATH`, `BUTTERPOLLO_FFMPEG_ROOT`, `BUTTERPOLLO_PYROWAVE_ROOT`, ...), `BUTTERPOLLO_TEST_CLIENT_EXE` (built from `rust/tests/moonlight_client.c`) and `BUTTERPOLLO_TEST_PYTHON` (a Python with `requests` and `cryptography`). Work files go to `%LOCALAPPDATA%\Butterpollo\release\<version>`.

## No prompts

- **UAC.** Run `rust/release/elevation.ps1` once from an elevated PowerShell. It copies `elevated.ps1` and `elevated-task.ps1` to `C:\ProgramData\ButterpolloRelease` (writable only by administrators and SYSTEM) and registers the on-demand task `ButterpolloReleaseElevated`, which runs them with highest privileges for you. `release.ps1` starts that task instead of asking, as long as the installed scripts match the checkout; after they change it asks once more and says to run `elevation.ps1` again. The trade-off: any program running as you can start the task, which installs the setup and runs the self-test from `%LOCALAPPDATA%\Butterpollo\release\<version>` with administrator rights. `elevation.ps1 -Remove` uninstalls it.
- **Firewall.** The release's test hosts listen on 127.0.0.1 only, and no unit test listens beyond loopback, so Windows Firewall has nothing to ask about.

The installed host must be idle: the stream checks start a second host on ports 48518-48544 and do not touch displays, HDR or audio.
