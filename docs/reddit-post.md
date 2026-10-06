# Butterpollo: I rebuilt my Moonlight host for Radeon. 56% lower picture delay under GPU load.

I rebuilt my Moonlight host in Rust around AMD Radeon. The payoff: **56% lower render-to-decode delay and 2.15× as many fresh pictures** in my controlled GPU load test.

**96.4 → 42.4 ms. 23.9 → 51.4 fresh FPS.**

Benchmark: Butterpollo rc.2 vs Vibepollo 2.0 · RX 7900 XT · DDX · 1080p60 HEVC HDR · 20 Mbps · three runs per host · October 4. [Measurements](https://github.com/RamazanKara/Butterpollo/blob/2.0.0-rc.10/rust/PERFORMANCE.md#against-vibepollo-20).

The trick: capture copies and colour conversion run on Radeon compute queues, feeding AMD's encoder directly. WGC + compute are on by default.

**PyroWave. Full HDR 4:4:4.** 10-bit HDR and full-resolution chroma for crisp coloured text and edges. Pair with [Nonary's Moonlight client](https://github.com/Nonary/moonlight-qt) on a fast wired LAN.

**Latest rc.10 checks: 6,342 frames decoded cleanly. ~60 distinct FPS in native virtual HDR, with verified colours. 263 automated tests passed.** Add virtual displays, Steam covers, RTSS and a live stats console.

Upgrade in place and keep your settings, pairings and games. Updates notify first; auto-install is opt-in.

**[Download](https://github.com/RamazanKara/Butterpollo/releases/tag/2.0.0-rc.10) · [Demo](https://github.com/RamazanKara/Butterpollo#butterpollo)**

Radeon owners, take it for a spin. Show me your results.
