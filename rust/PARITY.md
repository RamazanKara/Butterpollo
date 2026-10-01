# Previous Butterpollo features and Rust validation

The compatibility baseline is `2.0.0-beta.3-butter.4`, C++ commit `f23ee0c9e7857887be7f774de6ac5153500a7e53`. This inventory describes the Rust implementation and the evidence available on 2026-10-01. An implemented platform adapter is not the same as an exercised hardware feature. NVIDIA, Intel, VHF and privileged virtual-display behavior are explicitly awaiting native validation.

The host, service supervisor, console, protocol, display recovery process, Vulkan layer and NGX adapter are Rust. External codec/GPU SDK libraries and Windows device drivers remain dependencies. The package does not build or invoke the previous C++ host, display helper, service wrapper or Vue frontend. WebRTC, SudoVDA and ViGEm were already removed in the baseline; they are not requirements introduced by this migration.

| Previous behavior | Rust implementation | Evidence and remaining validation |
| --- | --- | --- |
| Existing configuration, certificates, credentials, app/client identity, permissions and unknown fields | Atomic migration and state writes; legacy client normalization; stable app aliases after artwork changes | Configuration/state vectors, actual pairing and administration fixtures pass |
| PIN pairing and authenticated Moonlight endpoints | RSA/AES pairing, TLS identities, certificate authorization and permission checks | Independent Moonlight-common-c client passes real pairing and denied actions |
| RTSP, SDP, encrypted control, video and audio | Fractional negotiation, legacy CBC/GCM, replay window, ENet control, RTP, Cauchy FEC and codec capability advertisement | Wire vectors and independent encrypted streaming/decode pass |
| H.264, HEVC, AV1, HDR and 10-bit SDR | Direct AMF; native D3D11 NVENC/QSV imports with compatibility fallback; software encoders | AMD streams and GPU color tests pass; NVIDIA/Intel encoding awaits hardware |
| AMD reference frame invalidation | Bounded LTR anchors, loss feedback, recovery frame signaling, IDR fallback and AVC wrap handling | Strict independent H.264/HEVC/AV1 decode after dropping two frame ranges passes |
| PyroWave SDR/HDR | Rust encoder adapter and client-compatible container | Independent vendor decoder passes SDR/HDR containers; full HDR client rendering is unverified |
| DXGI/WGC capture, pacing and optional WGC publication alignment | Bounded capture pools, repeat-frame minimum, exact rate grid, latest uncopied WGC frame publication, fatal frame-pool error propagation and safe asynchronous runtime shutdown | Real capture/streaming and 16 repeated reconnect/COM teardown cycles pass, including closed-pool errors for GPU/CPU recovery; deadline/static-frame/waiting-slot policy vectors pass |
| TrueHDR app/client/live tuning and driver profiles | Explicit override precedence, asynchronous NVDRS/visible-window lookup, passive overlay handling, neutral desktop tuning, native FP16/PQ bypass and shared-device NGX output | Precedence, visible-stack, calibration and ABI vectors pass; live NVIDIA NGX/profile behavior awaits hardware |
| NVIDIA power, OpenGL/Vulkan presentation and HAGS priority preferences | Owned app/base profile journal and conditional restoration; process GPU scheduling policy | Vendor SDK ABI and recovery/policy vectors pass; live NVIDIA driver verification remains |
| Frame generation, fractional display rates, render scaling and EDID refresh checks | Previous provider aliases and multiplier semantics; independent resolution/refresh policies; inherited capture/sync settings | Baseline identity/rate/scaling/EDID vectors pass; physical mode application is unverified |
| Virtual displays, permanent counts, per-client/app/shared identities | Driver protocol 3.6+, original GUID/FNV identity rules, persisted shared GUID, labels, HDR peak and owned finite leases | Payload/identity vectors pass; the current user cannot open the VDD interface, so native create/recreate/count tests have not run |
| Display loss recovery during a game or retained monitor session | Reopen/recreate the owned lease, refresh output identity, reapply HDR/DPI/layout/profile and restart capture; retry retained layout restoration | Implemented; native driver-loss fault injection remains unverified |
| Exclusive, primary, extended and isolated arrangements; Golden snapshot recovery | Native CCD topology/modes/HDR/DPI/rotation, old snapshot import, original restoration snapshot and independent crash recovery | Import/route vectors and read-only Golden comparison pass; physical apply/crash restoration is unverified |
| Remote Game/Input/Monitor/Secondary catalogue and confirmations | Stable control IDs, owner projection, generation/target/client-scoped confirmations, independent roles and retained monitor layout state | Previous transition and permission vectors pass; actual retained monitors require the privileged driver path |
| Keyboard, mouse, clipboard, touch, pen and VHF controllers | Previous bindings, disabled-device policies, per-controller type selection, Back-to-Home and feedback paths | Packet/policy vectors and independent denied-input checks pass; VHF haptics/controller and secure-desktop interaction need native testing |
| Stereo, 5.1, 7.1, quality/custom Opus layouts and audio routing | WASAPI, bounded Opus packets, channel mappings, per-role default/format journals and shared owned leases | 21 actual Opus layout/quality/duration round trips pass; tests use capture-only routing and do not change default audio devices |
| App execution, preparation/undo, client connect/disconnect commands and lifecycle | Case-insensitive saved environment, owned process jobs, pause retention, state hooks, graceful exit and timeout fallback | Native process-tree teardown, quoted output and real client connect/disconnect hooks pass |
| RTSS/NVAPI frame limiting and HDR Vulkan interception | Direct SDK profile/RPC calls, durable conditional restoration and Rust implicit Vulkan layer | Profile/ABI/format vectors pass; live RTSS/NVIDIA/Vulkan presentation remains unverified |
| Discovery, IPv4/IPv6 listeners, UPnP and wake-on-LAN identity | Native mDNS, dual-stack sockets, finite IPv4 leases with permanent-only fallback, stable host ownership/adoption, IGDv2 IPv6 pinholes and interface MAC | Dual-stack, real loopback SOAP create/renew/cleanup and UDP offload tests pass; physical router behavior remains unverified |
| Administration, locales and persistent browser sessions | Rust HTML global/app/client editors, typed TrueHDR fields, inherited reset, app ordering, 22 previous locale catalogues with English fallback, token scopes, refresh/revoke/remember-me and previous session import | Real API, restart/migration and JavaScript-disabled desktop/mobile browser fixtures pass; locale vectors preserve names, commands, JSON and field values |
| Maintenance, diagnostics, logs, updates and crash support | Rust minidumps, support ZIP/parts, release checks, tray, CLI probes and SCM supervisor | Native minidump, ZIP integrity and administration tests pass; actual Rust service installation was not exercised |

## Reproducible verification

`build.ps1 -Package` runs formatting, locked workspace tests, Clippy with warnings denied, release builds and separate MSVC NGX adapter checks. Hardware tests are intentionally opt-in in a normal CI environment:

```powershell
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p butterpollo-windows --locked -- --ignored --test-threads=1 --nocapture
```

The ignored tests require the packaged codec DLLs on `PATH`, the same SDK environment as the build, an AMD D3D11/AMF adapter and a local network route. All eight native tests pass together in one process. They exercise GPU color/retention, native FFmpeg frame ownership, all AMD loss-recovery codecs, actual Opus surround, 16 WGC reconnect/COM teardown cycles, closed-pool error propagation for GPU/CPU capture and read-only adapter MAC lookup. They do not change display modes, audio defaults or the installed service.

Set `BUTTERPOLLO_TEST_OPUS_ROOT` to the packaged runtime directory, `BUTTERPOLLO_TEST_FFMPEG` to an independent FFmpeg decoder executable, and `BUTTERPOLLO_TEST_RFI_REPORT` to the desired JSON report filename. `BUTTERPOLLO_TEST_AUDIO_REPORT` optionally saves the Opus report. The loss fixture saves its elementary streams beside the report and verifies all retained frames using the independent decoder.

Run `tests/web_api.py`, `tests/console_browser.cjs` and `tests/session_restart.py` against isolated test-owned configurations. The browser fixture requires Playwright/Chromium, an artifact directory and the fixture password through environment variables. The restart fixture launches and stops only its own subprocess. `tests/interop.py` pairs a temporary client, streams/decrypts/decodes, checks permissions/hooks, then cancels and unpairs that client. See [PERFORMANCE.md](PERFORMANCE.md) for the hardware, limitations and measurement commands.

## Production parity gate

Production parity remains open until the native hardware paths are exercised: NVIDIA/Intel codecs; NVIDIA NGX, power/presentation/profile restoration and HAGS; VHF controller feedback and secure desktop; privileged VDD create/recreate/permanent/shared/retained monitors; real display/audio policy changes and crash restoration; physical UPnP routers; Vulkan/RTSS integrations; and a separately authorized Rust SCM installation. The existing production service and physical display/audio settings were preserved during this work.

A controlled comparison with the same C++ baseline, content and hardware also remains outstanding. Current performance evidence establishes the Rust implementation's output and improvements over its earlier Rust paths; it does not establish a C++ speedup.
