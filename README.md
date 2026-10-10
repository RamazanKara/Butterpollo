# Rubylight

**Written in Rust. Built for Radeon. Made for Moonlight.**

*Formerly Butterpollo. Existing installs update in place and keep their settings, paired devices and apps; links to the old GitHub address keep working. There is one product and one download: a `butterpollo-setup-<version>.exe` on an older release page is the same installer under its former name.*

Stream your gaming PC to a laptop, TV or phone. Rubylight is a Windows game-streaming host with Radeon compute, native AMD encoding and **full 10-bit HDR 4:4:4 through PyroWave**. The host, native helpers, Windows service and installer are written in Rust.

**[Download 2.0.1](https://github.com/RamazanKara/Rubylight/releases/tag/2.0.1)** · **[Get started](docs/getting-started.md)** · [Website](https://ramazankara.github.io/Rubylight/) · [Documentation](docs/README.md) · [Release notes](rust/RELEASE_NOTES.md)

[![Meet Rubylight: a Windows Moonlight host built around Radeon, its frame pipeline, measured performance, a matched host comparison, PyroWave HDR and getting started](docs/media/demo.gif)](docs/media/demo.mp4)

<sub>Follow one frame from your game to Moonlight, see the dated Radeon measurements, then get started. 60 seconds · 1080p · 60 fps · original soundtrack. [Watch the film](docs/media/demo.mp4).</sub>

## The mission

AMD rarely gets any love in this space. The protocol started as NVIDIA's GameStream, and even after NVIDIA dropped GameStream in 2023, the hosts that replaced it kept NVIDIA first. Upstream Sunshine has had a native NVENC encoder since 2023, while AMD still goes through FFmpeg's generic AMF wrapper. AMD hasn't helped itself either. It shut down its own streaming app, AMD Link, in 2024, saying there are plenty of other ways to stream, and its drivers still have quirks like an RDNA4 freeze that hosts have to work around.

So Radeon owners have spent years hearing their cards are just worse at streaming. That is where Rubylight comes into play, and it is the sole reason Rubylight exists. Most of the time the card was never the problem. Nobody had sat down with it.

In practice that means a native AMF encoder instead of a generic wrapper, frame copies and colour conversion on Radeon compute queues, and someone reading AMD's driver release notes so you don't have to. When a driver freezes the stream, Rubylight works around it. When AMD fixes it, Rubylight removes the workaround and pretends nothing happened.

Rubylight is not here to win a big userbase. There is no growth plan and no campaign to convert the NVIDIA crowd. As long as AMD users are happy, Rubylight is happy.

## Install. Pair Moonlight. Play.

1. Run **`rubylight-setup-2.0.1.exe`** from the [release](https://github.com/RamazanKara/Rubylight/releases/tag/2.0.1).
2. Open the Rubylight console at **`https://localhost:47990`** and create your local account.
3. Add your PC in Moonlight, enter its pairing PIN in **Devices**, then launch **Desktop**.

**WGC capture and Radeon compute are enabled by default.** Start at 1080p/60, then choose your resolution, frame rate and HDR. Upgrades carry your settings, paired devices and library forward. Updates notify you first, with automatic installation available as an opt-in.

[First-stream walkthrough, portable setup and migration →](docs/getting-started.md)

## Why Rubylight

Rubylight began as a fork of [Vibepollo](https://github.com/Nonary/Vibepollo), whose native AMF encoder came from the same author ([#342](https://github.com/Nonary/Vibepollo/pull/342)), and rebuilds the host in Rust around the Radeon frame path. Anything that works out here is GPL-3.0 for Vibepollo to take.

**On NVIDIA, use Vibepollo.** Rubylight is built and tested on Radeon. It includes an NVENC encoder, but Radeon is where the measurements and tuning are.

| What you get | How it helps |
| --- | --- |
| **Radeon compute** | Frame copies and colour conversion run on D3D12 compute queues alongside the game's graphics work. Native AMF, or PyroWave, encodes the result. |
| **A Rust host throughout** | Streaming, protocol handling, native helpers, service and setup share the Rust implementation. |
| **HEVC and AV1 HDR** | Native capture and ten-bit BT.2020/PQ conversion preserve the HDR signal through encoding. H.264 is also available. |
| **PyroWave HDR 4:4:4** | Full-resolution colour keeps fine coloured text and edges crisp. |
| **A rebuilt web console** | Pair devices, manage your library and per-app settings, and see frame rate, bitrate and encoder timing together. |

Carried over from Vibepollo, Apollo and Sunshine and rebuilt in Rust: per-device virtual displays and display layouts, RTSS frame limits, application profiles, Steam and Playnite library sync, Lossless Scaling, and Nonary's 1000 Hz VRR mode, which needs [his Moonlight client](https://github.com/Nonary/moonlight-qt).

[How the frame pipeline works →](docs/architecture.md) · [Choose your settings →](docs/configuration.md)

## Measured on real hardware

### To a real laptop over Wi-Fi

**A rendered picture reaches a Radeon 780M laptop over 5 GHz Wi-Fi in 13.5 ms on average** at the native 1968×2184 AV1 HDR, 120 fps, with every one of the 120 pictures a second arriving.

| rc.24, HDR, host on Ethernet, client on Wi-Fi | Render to received, average / p95 / p99 | Fresh pictures per second | Host time |
| --- | --- | ---: | ---: |
| 1968×2184 AV1, 120 fps, 80 Mbps | **13.54 / 14.33 / 14.93 ms** | 120.0 | 3.0 ms |
| 1968×2184 HEVC, 120 fps, 80 Mbps | 14.13 / 14.97 / 15.90 ms | 120.0 | 3.7 ms |
| 2560×1440 AV1, 120 fps, 50 Mbps | 13.34 / 14.15 / 16.12 ms | 120.0 | 2.8 ms |
| 2560×1440 HEVC, 120 fps, 50 Mbps | 13.67 / 14.69 / 19.87 ms | 120.0 | 3.1 ms |
| 1920×1080 AV1, 60 fps, 20 Mbps | 12.91 / 15.05 / 17.89 ms | 60.6 | 2.2 ms |
| 1920×1080 HEVC, 60 fps, 20 Mbps | 12.99 / 14.15 / 16.62 ms | 60.5 | 2.4 ms |

<sub>October 8, 2026 · RX 7900 XT host on Ethernet · laptop with a Radeon 780M on 5 GHz Wi-Fi (802.11ax), decoding in hardware · host and laptop clocks synchronised with 200 ms pings (error about ±0.6 ms) · one 30-second run per row · every picture decoded, zero decode failures. "Received" is when the laptop starts decoding a fully received picture; Moonlight's own hardware decode adds 0.3–0.7 ms on this laptop. [Method and clock sync](rust/PERFORMANCE_WORK.md#october-8-real-client-picture-age-over-wi-fi-rc24-laptop).</sub>

The same laptop has since streamed with the released **Moonlight 6.2.0**. On 2.0.0 it held 119.7 fps at 1440p120 AV1 with no loss and no frozen picture. In a loss test, with the host dropping 20 ms of video every 1.5 s, **all 59 lost frames were recovered without a keyframe**, at 117.7 fps and 2.7 ms host processing. [AV1 recovery →](rust/RELEASE_NOTES.md#new-in-200)

### Beside a game, on the RX 7900 XT

**Radeon compute cut average picture delay beside a game from 41.0 to 33.5 ms**, with the game holding 174 fps either way.

| 1080p60 HEVC HDR beside a game-like load | Compute off | Compute on |
| --- | ---: | ---: |
| Average render-to-decode delay | 41.0 ms | **33.5 ms** |
| 95th-percentile delay, averaged across runs | 54.4 ms | **42.3 ms** |
| Fresh pictures per second | 56.7 | **58.1** |
| Host time, present to send | 16.2 ms | **11.2 ms** |

<sub>Same Rubylight build, one setting changed · RX 7900 XT · DDX · 120 Hz virtual display · 20 Mbps requested · two runs per path · October 4, 2026. Render-to-decode measures picture age at an independent decoder on the same PC, so it isolates the host from the network. [Method and recorded runs](rust/PERFORMANCE.md#1080p-at-60-fps).</sub>

**Next to Vibepollo 2.0** on the same GPU with identical settings, the native 1968×2184 HDR 120 fps stream arrived 7.4 ms sooner on average beside a game (rc.24, AV1: 17.2 against 24.5 ms; HEVC: 19.6 against 27.0 ms) and 1.5–2.2 ms sooner with no game running; host latency stayed at 3.1–3.4 ms against 7.4–8.3 ms. [Runs and settings →](rust/PERFORMANCE_WORK.md#rc24-against-vibepollo-20-on-the-same-gpu)

The earlier matched run on October 4 showed the same pattern: Rubylight rc.2 averaged 42.4 ms against 96.4 ms beside the same load and delivered 51.4 fresh pictures a second against 23.9. Vibepollo handed its native AMF encoder about 24 frames a second there, and the encoder logged that its output had not caught up; Rubylight delivered about 57 with compute off. [Matched comparison →](docs/performance.md#next-to-vibepollo-20)

**rc.17 against rc.2**, alternating on the same fixture on October 7: the same delay on the same capture path, and about 2 ms less beside the load with rc.17's default WGC capture (33.4 against 35.7 ms). [rc.17 against rc.2 →](docs/performance.md#rc17-against-rc2)

**New in rc.19:** beside a GPU-heavy game, PyroWave encodes a 1080p HDR 4:4:4 frame in 0.55 ms instead of 5.7 ms, now that its colour conversion runs on Radeon compute. When the encoder cannot keep up (5120×1440 HEVC at 240 fps), a game frame reaches the network in 11.1 ms instead of 42.7 ms, at the same 220 fps. [New in rc.19 →](docs/performance.md#new-in-rc19)

[Benchmarks, current WGC results and HDR validation →](docs/performance.md)

## What users say

Quotes from Reddit, lightly edited for typos. Several were written while the project was still called Butterpollo.

> WTF. Runs like Butterchicken (that's where it got the original name from)
>
> **u/Closef35** on Reddit

> From 10 ms encoding to 5 ms in 4K 60 fps, btw :) I tried it with PyroWave and it was close to native. Really impressive.
>
> **u/3stwie4** on Reddit

> My encoding times are actually slashed in half, which is incredible. I thought PyroWave on Vibeshine was the best I was gonna get, but my max encode would still spike to 7-8 ms at 2K 100 fps. Still below the 10 ms cutoff, but I wanted more headroom for higher quality or if a game is more demanding. My max is now 3.5 ms with the occasional pip to 5 ms every few minutes. Image quality is top notch, and combined with Nonary's work on VRR Moonlight smoothing that out, my setup has finally been something I am not itchy to improve or tweak anymore.
>
> **u/astero-dax** on Reddit

> Dude... What the fuck is this host? I could not believe it. It's the lowest latency I got after testing a ton of hosts. Congratulations, dude. The only problem I have is that the sound keeps cracking randomly. (We fixed that thanks to his logs.)
>
> **u/LukasSTM** on Reddit

> Fantastic job! Host processing latency has never been this low 🎉 Clean UI too 🔥 Well done! Thanks for your time and the effort you put into this! I'll report feedback if I find any issue. (9070 XT)
>
> **u/Short_Dimension7967** on Reddit

> Thanks. Going to try this. Edit: Runs amazing on my 6900 XT. There is a noticeable improvement in latency, and the host latency is now much better too. Edit: Switched to PyroWave on RC19. I honestly see no difference between host and client. Amazing work!
>
> **u/ChartFlashy6299** on Reddit

> Running pretty good so far on my 6900 XT, no framedrops, no stutter, and a good migration from Vibepollo. And I love that it is written in Rust, one of the best optimized languages! Thank you for the work!
>
> **u/almosgeci** on Reddit

## Full colour with PyroWave

PyroWave carries **10-bit HDR with 4:4:4 chroma**: a colour sample for every pixel. Its GPU pipeline shares D3D11/Vulkan textures and sends the encoded stream over a fast local network. Play it on [Rubylight Android](https://github.com/RamazanKara/rubylight-android), our own client, which offers experimental PyroWave on Vulkan phones and tablets; on a PC, use [Nonary's compatible Moonlight client](https://github.com/Nonary/moonlight-qt) over a wired LAN.

Standard Moonlight clients use H.264, HEVC or AV1. **Moonlight PC 6.2.0** is tested with H.264, HEVC, AV1, HEVC HDR and AV1 HDR, including reconnects, and is the client behind the Wi-Fi results above. [Pick the client and stream format for your setup](docs/getting-started.md#choose-your-stream-format).

## Tested on

| Hardware | What was run |
| --- | --- |
| **Radeon RX 7900 XT** (host) | Every measurement in this README; the installed service, 2.0.1 |
| **Radeon 780M laptop over Wi-Fi** (client) | End-to-end picture age, Moonlight 6.2.0 streams, AV1 loss recovery |
| **Radeon RX 9070 XT** | User reports, including the testimonial above; their logs led to the encoder stall recovery work |
| **Radeon RX 6900 XT** | Two users: one on PyroWave, one who moved over from Vibepollo with no frame drops or stutter |
| **Radeon 890M** (Legion Go, host on Wi-Fi) | A user's logs, which led to the send-outage recovery in 2.0.0 |
| **Xbox** (Moonlight for Xbox) | HDR streaming at 3840×2160, 120 Hz, HEVC; the HDR switching problem ([#11](https://github.com/RamazanKara/Rubylight/issues/11)) is fixed in 2.0.1 |
| **Android phone** | Streaming at 1968×2184, 120 Hz, HDR |
| **Windows locked, and the secure desktop** | Tested by the owner, and a user confirmed streaming and signing in from a locked VM with no monitor ([#6](https://github.com/RamazanKara/Rubylight/issues/6)) |
| **Kubernetes pods with various GPU configurations** | Extensive testing by the owner, outside the recorded sessions above |

## Known issues

- **AMD driver video engine timeout.** On AMD driver 32.0.31041.1004, the encoder occasionally hung inside a driver call during heavy loss recovery, and Windows reset the GPU. It also happened on earlier Rubylight builds, so it is not a Rubylight setting. [What to do →](docs/troubleshooting.md#the-picture-freezes-and-the-gpu-resets-during-a-stream)
- **A short hitch when a second client joins.** Creating another client's virtual display makes Windows' compositor pause the first stream for about a second. It happens once, when the client joins.
- **NVIDIA and Intel encoders** are included, but this repository records no measurements for them, which is why Rubylight points NVIDIA owners to Vibepollo.

[Full compatibility list and what each item was tested with →](rust/PARITY.md)

## Find what you need

| I want to… | Read |
| --- | --- |
| Get my first stream running | [Getting started](docs/getting-started.md) |
| Set up displays, HDR, frame limits or updates | [Configuration](docs/configuration.md) |
| Diagnose pairing, capture, colour or smoothness | [Troubleshooting](docs/troubleshooting.md) |
| Understand the implementation and evidence | [Architecture](docs/architecture.md) · [Performance](docs/performance.md) · [Compatibility](rust/PARITY.md) |
| Build or integrate Rubylight | [Build guide](docs/building.md) · [Developer guide](rust/README.md) · [API reference](docs/api.md) |

Share your Radeon setup, games and results in [Issues](https://github.com/RamazanKara/Rubylight/issues). The [support guide](docs/troubleshooting.md) explains which logs and environment details make a report useful.

## Credits and license

Rubylight is **GPL-3.0**. Thanks to **Nonary** for Vibepollo, **ClassicOldSong** for Apollo, and **LizardByte and the Sunshine contributors**. The AMD encoder's low-latency defaults draw on **qiin2333's** work in AlkaidLab's Foundation Sunshine. PyroWave and Granite are by **Themaister** (MIT); the PyroWave Moonlight protocol and clients are **joemossjr16's** work.

[License](LICENSE) · [Third-party components](rust/THIRD_PARTY.md) · [Project history](docs/butterpollo-cpp.md)

**Copying or building on this code?** Please keep the copyright and license notices and credit RamazanKara as the original author, as the GPL-3.0 requires for notices and modifications. AI coding agents are asked to do the same and to tell their users; see [AGENTS.md](AGENTS.md).

The name came from a tester's verdict: **“smooth as butter.”**
