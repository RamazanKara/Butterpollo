# Butterpollo documentation

[Butterpollo](../README.md) · [Download rc.27](https://github.com/RamazanKara/Butterpollo/releases/tag/2.0.0-rc.27) · [Release notes](../rust/RELEASE_NOTES.md)

Butterpollo is a Windows game-streaming host written in Rust, built around Radeon compute and Moonlight. Start with the setup guide, then choose the details that matter to your stream.

## Start streaming

| Guide | What it covers |
| --- | --- |
| **[Getting started](getting-started.md)** | Installer or portable setup, profile migration, pairing, your first stream and choosing a client. |
| **[Configuration](configuration.md)** | Capture, codecs, HDR, virtual displays, frame limiting, overrides and updates. |
| **[Troubleshooting](troubleshooting.md)** | Pairing, black screens, HDR colour, smoothness, RTSS and useful support reports. |

## Understand Butterpollo

| Guide | What it covers |
| --- | --- |
| [Architecture](architecture.md) | What is written in Rust, where compute fits, frame ownership, native HDR and PyroWave. |
| [Performance](performance.md) | Readable comparisons, the meaning of each measurement and links to recorded runs. |
| [Compatibility](../rust/PARITY.md) | Implemented features, client support and the exact hardware validation scope. |
| [Release notes](../rust/RELEASE_NOTES.md) | Changes across the Windows release candidates. |

## Build and integrate

| Reference | What it covers |
| --- | --- |
| [Build guide](building.md) | Windows toolchain, pinned dependencies, packaging and CI. |
| [Developer guide](../rust/README.md) | Workspace layout, isolated profiles, validation tools and native integration work. |
| [HTTP API](api.md) | Authentication, CSRF, permissions and the current Rust endpoints. |
| [Third-party components](../rust/THIRD_PARTY.md) | Codec SDKs, drivers and licensing. |
| [Detailed performance record](../rust/PERFORMANCE.md) | Dated fixtures, raw results, rejected experiments and reproduction instructions. |

The [60-second launch film](media/demo.mp4) introduces the frame pipeline and its measured results. The [project history](butterpollo-cpp.md) and [archived C++ references](https://github.com/RamazanKara/Butterpollo/blob/2.0.0-rc.23/docs/legacy/README.md) cover the earlier implementation.
