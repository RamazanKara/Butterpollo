# Contributing to Butterpollo

Butterpollo is a Windows Moonlight host built for AMD Radeon. Changes that make streaming faster, smoother or more reliable on Radeon are the priority; for NVIDIA-first work, [Vibepollo](https://github.com/Nonary/Vibepollo) is the better home.

## Reporting problems

Search the existing issues first, then use the bug or crash form. Attach the support bundle from the console (**Logs → Download support bundle**); it holds the logs and diagnostics needed to reproduce most problems. [Troubleshooting](../docs/troubleshooting.md) covers the common cases.

## Layout

| Path | What it is |
| --- | --- |
| `rust/core` | Portable protocol, state and policy. No vendor SDKs; builds and tests on any OS. |
| `rust/windows` | Windows platform layer: capture, Radeon compute, AMF/NVENC/FFmpeg/PyroWave encoders, displays, input, audio. |
| `rust/host` | The `butterpollo` executables: HTTP, RTSP and stream servers, sessions and the console API. |
| `rust/setup` | The installer. |
| `rust/vulkan-layer` | The Vulkan layer that offers HDR formats on virtual displays. |
| `rust/web` | The Svelte web console. |
| `rust/tests`, `rust/release` | Hand-run harnesses (independent Moonlight client, soak, release checks). |

[Architecture](../docs/architecture.md) follows a frame through these pieces. `cargo doc --workspace --no-deps --open` builds the API reference.

## Building and checking

[Build Butterpollo](../docs/building.md) lists the prerequisites and the SDK environment. The first build fetches and verifies the pinned SDKs:

```powershell
.\rust\build.ps1 -FetchDependencies -SkipTrueHdr
```

Before sending a change, run the checks CI runs, in the same shell:

```powershell
cargo +1.98.1-x86_64-pc-windows-gnu fmt --all -- --check
cargo +1.98.1-x86_64-pc-windows-gnu clippy --workspace --all-targets --locked -- -D warnings
cargo +1.98.1-x86_64-pc-windows-gnu test --workspace --locked
```

For console changes, also run `npm ci` and `npm run check` in `rust/web`. Changes confined to `rust/core` can be checked anywhere with `cargo test -p butterpollo-core`.

## Expectations for a change

- **Measure latency changes.** Anything on the frame path (capture, colour conversion, encoding, packetization, send) needs before and after numbers from real hardware, recorded in [PERFORMANCE.md](../rust/PERFORMANCE.md). A change does not ship on reasoning alone.
- **Hardware tests are opt-in.** Tests that need a GPU, a display or a controller are `#[ignore]` with a reason; run them with `--ignored` on a suitable machine. Passing ordinary tests is not a hardware result.
- **Never test against the installed service.** Run a development host with `--config-dir` on its own profile and port, as the [Rust guide](../rust/README.md) shows.
- **Keep the crates layered.** Decisions that do not need Windows go in `core`, where they can be tested on any OS; Windows calls go in `windows`; `host` wires them together and has no `unsafe`.
- **Document unsafe code.** Each `unsafe` block says which invariant it relies on in a `// SAFETY:` comment.
- **Update the docs with the behaviour.** User-visible changes update `docs/` and the current section of [RELEASE_NOTES.md](../rust/RELEASE_NOTES.md); feature coverage lives in [PARITY.md](../rust/PARITY.md). Versions change only through `rust/release/bump.py`.

AI-assisted changes are welcome on the same terms: you understand every line, and the tests and measurements above back the claims.

## Pull requests

Keep each pull request to one change with a clear description of what a user would notice, link the issue it fixes, and include screenshots for console changes. Contributions are GPL-3.0-only, like the rest of the project.
