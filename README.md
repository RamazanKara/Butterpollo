# Butterpollo

**Built for AMD Radeon. Made for Moonlight.**

Your gaming PC has serious horsepower. Put it to work for your stream.

Butterpollo is a Windows game-streaming host rebuilt in Rust around Radeon compute and AMD's native encoder. Stream your games to a laptop, TV or phone with fast capture, sharp HDR and a console that puts you in control.

**PyroWave with full HDR 4:4:4. WGC + Radeon compute by default.**

**56% lower render-to-decode delay. 2.15× as many fresh pictures.**

<sub>Butterpollo rc.2 vs Vibepollo 2.0 · RX 7900 XT · DDX · 1080p60 HEVC HDR at 20 Mbps · controlled GPU load · three runs per host · October 4, 2026.</sub>

[![Butterpollo animated demo: Radeon compute and measured streaming results](docs/media/demo.gif)](docs/media/demo.mp4)

**[Download rc.10](https://github.com/RamazanKara/Butterpollo/releases/tag/2.0.0-rc.10)** · [Watch the demo](docs/media/demo.mp4) · [See the measurements](rust/PERFORMANCE.md#against-vibepollo-20)

## Radeon muscle. Measured results.

Butterpollo moves capture copies and colour conversion onto Direct3D 12 compute queues, then feeds the GPU textures straight into AMD's native AMF encoder. Streaming gets a dedicated compute path alongside the game's graphics work.

Here's what that delivered in the controlled comparison:

| RX 7900 XT · 1080p60 HEVC HDR · GPU under load | Vibepollo 2.0 | Butterpollo rc.2 |
| --- | ---: | ---: |
| Render to decoded picture, average | 96.4 ms | **42.4 ms** |
| Render to decoded picture, 95th percentile | 137.0 ms | **56.5 ms** |
| Fresh pictures per second | 23.9 | **51.4** |

<sub>October 4, 2026 · DDX · 20 Mbps · mean of three runs per host. Encrypted loopback streams, moving picture IDs and independent decoding. [Benchmark setup, runs and colour checks](rust/PERFORMANCE.md#against-vibepollo-20).</sub>

The idle comparison reached **13.8 ms** render-to-decode against **16.0 ms** for Vibepollo 2.0. In a separate comparison with the original C++ FEC implementation, video error correction uses **21–29% less CPU time** with byte-identical parity output. [Explore the performance work](rust/PERFORMANCE.md).

## PyroWave. Full HDR 4:4:4.

Stream **10-bit HDR with full-resolution chroma**, keeping fine coloured text and edges crisp. GPU conversion feeds PyroWave through shared D3D11/Vulkan textures. Pair with [Nonary's Moonlight client](https://github.com/Nonary/moonlight-qt) on a fast wired LAN for the PyroWave experience.

Our recorded **1080p/120 HDR 4:4:4** stream decoded **all 2,357 received frames**, with zero video or audio decode errors. [PyroWave transport results and setup](rust/PERFORMANCE.md#vibepollo-20-pyrowave-transport).

## Ready to stream

- **WGC and Radeon compute enabled by default.** Capture and pacing are tuned to get fresh frames moving.
- **AV1 and HEVC with HDR10, plus H.264.** Native AMD encoding keeps the video pipeline on the GPU.
- **Native HDR. Verified colours.** rc.10's native virtual-HDR checks delivered about **60 distinct pictures per second** with HEVC and AV1, verified decoded colours and restored the display layout.
- **6,342 frames decoded cleanly** across five latest rc.10 SDR and native-HDR checks.
- **263 automated tests passed.** Coverage includes HDR transitions, pairing, encryption, input, display hotplug and recovery.
- **Moonlight PC 6.2.0 tested.** Codec, reconnect and AMD AV1 crop checks have recorded results.

<sub>Native HDR setup: rc.10 · RX 7900 XT · 1280×720/60 · FP16 virtual-display capture · independent decoding. [Detailed results](rust/PERFORMANCE.md#final-native-virtual-hdr-pixels-excluding-physical-panel-calibration) · [Compatibility evidence](rust/PARITY.md).</sub>

## Your setup, your way

Create per-device virtual displays, choose extended or isolated layouts, and give every game its own profile. Bring in your Steam library with covers, launch through Playnite, and configure Lossless Scaling per app.

The web console brings live frame rate, encoder timing and frame age together with pairing, device permissions, display controls and RTSS frame limiting. rc.10 adds precise Windows HDR detection and a startup guard that automatically restores your chosen display layout as your virtual screen comes online.

## Upgrade and play

1. Get **`butterpollo-setup-2.0.0-rc.10.exe`** from [Releases](https://github.com/RamazanKara/Butterpollo/releases/tag/2.0.0-rc.10).
2. Run the installer. A fresh install sets up the host, service and drivers. An upgrade brings your settings, paired devices and game library forward.
3. Open **`https://localhost:47990`**, pair Moonlight and launch a stream.

Updates **notify you first**. Automatic installation is available as an opt-in. A portable ZIP is also available: extract it and open **Start Butterpollo.exe**.

Share your Radeon setup, your games and your results in [Issues](https://github.com/RamazanKara/Butterpollo/issues). The console's support bundle and the included environment-report tool make it easy to bring useful details.

[Release notes](rust/RELEASE_NOTES.md) · [Performance evidence](rust/PERFORMANCE.md) · [Feature and compatibility matrix](rust/PARITY.md) · [Build and run](rust/README.md) · [Project history](docs/butterpollo-cpp.md)

## Credits and license

Butterpollo is GPL-3.0. Thanks to Nonary for Vibepollo, ClassicOldSong for Apollo, and LizardByte and the Sunshine contributors. PyroWave and Granite are by Themaister (MIT); the PyroWave Moonlight protocol and clients are joemossjr16's work.

The name came from a tester's verdict: **“smooth as butter.”**
