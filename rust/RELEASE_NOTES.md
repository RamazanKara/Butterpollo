# Butterpollo Rust 2.0.0-rc.1 — Windows

This candidate brings the Windows streaming features of Vibepollo 2.0 into the Rust host. Extract the complete package and open **Start Butterpollo.exe**. The host, launcher, service, console and platform helpers are Rust; codec SDKs and Windows drivers remain external dependencies.

## What players experience

- A first-run import for existing Vibepollo/Apollo settings, paired devices, certificates, library and covers. The original profile is retained. Missing configured identity files stop the import with an error instead of silently requiring everyone to pair again.
- Imported passwords, browser access/refresh sessions and API keys recognize the previous C++ digest byte order as well as earlier Rust profiles. Saved login data stays intact; API permissions, expiry, rotation, CSRF checks and logout remain enforced.
- Existing `amdvce` encoder aliases resolve to the native Rust AMF backend without rewriting the saved profile.
- Installed Sunshine virtual display drivers using protocol 3.5 remain usable through their original lease API. Protocol 3.6 keeps its secure creation API and retains the same owner capability when recovering a display.
- The `ddx` capture setting now selects Desktop Duplication, with nonblocking capture polling, real hardware cursor composition and the newest desktop/pointer QPC timestamp. The log identifies the backend actually opened. HDR streaming from an SDR desktop uses an SDR capture surface and converts it to PQ.
- Virtual speakers negotiate integer, float and 24-bit-in-32-bit formats, with the previous host's depth and surround preferences. Failed routing and WASAPI capture are visible in logs and retried. Default devices and formats remain journaled for restoration.
- Mouse input uses a high-resolution timer instead of a coarse thread sleep. Service shutdown uses a private, parent-validated event across Windows sessions; intentional restarts bypass the crash backoff. Support archives include the supervisor log.
- A connection checklist explaining video, display and audio readiness, followed by the steps to pair Moonlight and start Desktop. Manual Add PC shows the physical LAN address and includes a custom Moonlight port. GPU checks show progress. Device names, commands and JSON remain data when changing language.
- Live session rates, encode p95 and two minutes of bounded performance history. Optional five-second refresh works without JavaScript.
- Capture and encoder waits use an interruptible high-resolution timer. A 250 µs wait measured 0.615 ms instead of 15.293 ms on the validation machine. Unchanged desktop images preserve the waiting encode slot, avoiding another full frame interval when fresh content arrives. Host processing and encode latency are measured separately; this is a scheduling improvement, not a measured whole-host advantage over C++.
- Vibepollo 2.0 PyroWave bitstream `186f0393`, GPU conversion, SDR/HDR, 8/10-bit and 4:2:0/4:4:4; old length framing and current record framing. Coarse data receives FEC, detail can recover after loss, and adaptive protection stays within each frame's bandwidth allowance.
- A PyroWave sender per client that retains one pending frame. A slow connection replaces older pending frames instead of building latency in an encoder queue. The console explains how to reduce repeated replacements.
- Streaming stays continuous when the 16-bit RTP counter wraps. The separate Moonlight stream index retains all 24 bits, including in FEC recovery. Explicit repeat-frame rates no longer drop to half the requested cadence because encoding time was counted twice.
- Encrypted video packets reuse AES-GCM setup per frame and share the final allocation with FEC, avoiding extra ciphertext allocations and payload moves. Measured packet-processing CPU time is 11–16% lower than the first Rust 2.0 candidate, with identical wire output. A standalone packet probe is included for reproduction.
- VRR virtual displays use 1000 Hz capture while keeping the client's requested stream rate. WGC composition timestamps travel through asynchronous encoders; DXGI present timing refines timestamps when Windows permits ETW tracking. Physical displays retain their selected refresh policy.
- Existing Remote Input/Monitor roles, retained monitor ownership, VHF controller profiles and feedback, per-app ten-bit SDR, capture-only audio, TrueHDR policies, fractional rates and permission controls remain available.

## Measured reason to switch

Rust video FEC uses **21–29% less CPU time** than the previous C++ baseline on representative blocks, with identical parity bytes. GPU conversion and bounded queues also remove avoidable work and backlog, but these do not establish a whole-host latency advantage. Reproduction and measurement limits are in [PERFORMANCE.md](PERFORMANCE.md).

## Before a production release

This is a release candidate. AMD codec/capture/color, independent encrypted Moonlight streaming, PyroWave vendor decoding and console tests are available. Actual service installation/restart, a per-client HDR virtual display and real stereo audio through WASAPI and independent Opus decoding have passed. AMD AV1 still pads unaligned dimensions: strict decoding fails at 1968×2184, while HEVC passes at that size. NVIDIA/Intel hardware, the remaining VDD lifecycle cases, VHF feedback and full Nonary-client VRR/HDR rendering still require acceptance. See [PARITY.md](PARITY.md) for the precise evidence. The package does not install device drivers automatically.
