# Butterpollo Rust 2.0.0-rc.1 — Windows

This candidate brings the Windows streaming features of Vibepollo 2.0 into the Rust host. Extract the complete package and open **Start Butterpollo.exe**. The host, launcher, service, console and platform helpers are Rust; codec SDKs and Windows drivers remain external dependencies.

## What players experience

- A first-run import for existing Vibepollo/Apollo settings, paired devices, certificates, library and covers. The original profile is retained. Missing configured identity files stop the import with an error instead of silently requiring everyone to pair again.
- A connection checklist explaining video, display and audio readiness, followed by the steps to pair Moonlight and start Desktop. Manual Add PC shows the physical LAN address and includes a custom Moonlight port. GPU checks show progress. Device names, commands and JSON remain data when changing language.
- Live session rates, encode p95 and two minutes of bounded performance history. Optional five-second refresh works without JavaScript.
- Vibepollo 2.0 PyroWave bitstream `186f0393`, GPU conversion, SDR/HDR, 8/10-bit and 4:2:0/4:4:4; old length framing and current record framing. Coarse data receives FEC, detail can recover after loss, and adaptive protection stays within each frame's bandwidth allowance.
- A PyroWave sender per client that retains one pending frame. A slow connection replaces older pending frames instead of building latency in an encoder queue. The console explains how to reduce repeated replacements.
- Streaming stays continuous when the 16-bit RTP counter wraps. The separate Moonlight stream index retains all 24 bits, including in FEC recovery. Explicit repeat-frame rates no longer drop to half the requested cadence because encoding time was counted twice.
- Encrypted video packets reuse AES-GCM setup per frame and share the final allocation with FEC, avoiding extra ciphertext allocations and payload moves. Measured packet-processing CPU time is 11–16% lower than the first Rust 2.0 candidate, with identical wire output. A standalone packet probe is included for reproduction.
- VRR virtual displays use 1000 Hz capture while keeping the client's requested stream rate. WGC composition timestamps travel through asynchronous encoders; DXGI present timing refines timestamps when Windows permits ETW tracking. Physical displays retain their selected refresh policy.
- Existing Remote Input/Monitor roles, retained monitor ownership, VHF controller profiles and feedback, per-app ten-bit SDR, capture-only audio, TrueHDR policies, fractional rates and permission controls remain available.

## Measured reason to switch

Rust video FEC uses **21–29% less CPU time** than the previous C++ baseline on representative blocks, with identical parity bytes. GPU conversion and bounded queues also remove avoidable work and backlog, but these do not establish a whole-host latency advantage. Reproduction and measurement limits are in [PERFORMANCE.md](PERFORMANCE.md).

## Before a production release

This is a release candidate. AMD codec/capture/color, independent encrypted Moonlight streaming, PyroWave vendor decoding and console tests are available. NVIDIA/Intel hardware, privileged VDD operations, VHF feedback, actual service installation and full Nonary-client VRR/HDR rendering still require acceptance on the relevant machines. See [PARITY.md](PARITY.md) for the precise evidence. The package does not install device drivers automatically.
