# Microphone

[Documentation](README.md) · [Configuration](configuration.md) · [Compatibility](../rust/PARITY.md)

A streaming device can send its microphone to the PC. Games and chat apps on the PC hear it as **Microphone (Steam Streaming Microphone)**, a recording device Steam provides. Sunshine and Vibepollo have no microphone; Rubylight speaks the format of Apollo's microphone passthrough ([Apollo pull request #1428](https://github.com/ClassicOldSong/Apollo/pull/1428), client side in [ClassicOldSong/moonlight-common-c](https://github.com/ClassicOldSong/moonlight-common-c/blob/784fa1d0f501155ab01fea7cefe8a0e9c9628b77/src/MicrophoneStream.c)), so a client built for it works with Rubylight unchanged.

## On the PC

- **Use the device microphone** (`stream_mic`, on by default) offers the microphone to every client. A device or app override can turn it off for one device or game.
- The first time a device sends its microphone, Rubylight installs Steam Streaming Microphone from Steam's drivers, as it does Steam Streaming Speakers, when Steam is installed and **Install Steam audio drivers** (`install_steam_audio_drivers`) is on. If Windows makes the new device the default speaker or microphone, Rubylight puts your previous defaults back.
- In the game or chat app, choose **Microphone (Steam Streaming Microphone)** as the microphone, or make it the Windows default recording device.
- The host plays the microphone only while a device sends it, and releases the device three seconds after it stops. When two devices send at once, the first is heard until it has been quiet for a second.
- The microphone uses UDP port 48001 (the base port plus 12), next to the other stream ports. With UPnP on, it is mapped with them.
- The stream card shows **Microphone unavailable** with the reason when the PC cannot play it, for example when Steam is not installed. The log records `microphone started` and, at the end, `microphone stopped` with the packets received, concealed, recovered from error correction, late and trimmed.

## For client developers

The microphone is a separate UDP stream from the client to the host. Nothing goes back to the client; the video and audio streams are unchanged.

### Negotiation

1. **DESCRIBE.** A host that takes a microphone adds these lines, and adds `8` (microphone encryption) to `x-ss-general.encryptionSupported` and `x-ss-general.encryptionRequested`:

   ```
   m=audio 48001 RTP/AVP 96
   a=rtpmap:96 opus/48000/1
   a=fmtp:96 minptime=10;useinbandfec=1
   ```

   The `a=rtpmap:96 opus/48000/1` line is the signal. Without it, do not set the microphone up.
2. **SETUP** `streamid=mic/0/0` (`streamid=mic` for a host older than app version 5), after the audio, video and control streams and before ANNOUNCE. The `Transport: server_port=` header gives the port.
3. **ANNOUNCE** with bit `8` set in `x-ss-general.encryptionEnabled`. Rubylight, like Apollo, never takes a microphone in the clear: without the bit, the microphone is off for that session and the stream card says why. Everything else in the stream goes on.
4. **PLAY.** A client that plays each stream separately may send `PLAY streamid=mic`; a client that sends one `PLAY` for the session needs nothing more.

### Packets

One datagram per Opus packet, from any local port to the host's microphone port:

| Bytes | Field | Value |
| --- | --- | --- |
| 0 | flags | 0 |
| 1 | type | `0x61` (Opus) |
| 2–3 | sequence | little-endian, +1 per packet, wraps at 65535 |
| 4–7 | timestamp | little-endian, the client's clock in milliseconds (logs only) |
| 8–11 | magic | `0x12345678`, little-endian (`78 56 34 12`) |
| 12– | payload | the encrypted Opus packet |

The payload is AES-128-CBC with the launch's `rikey` as the key. The IV is 16 bytes: the big-endian 32-bit sum of the launch's `rikeyid` and the packet's sequence number, then 12 zero bytes (the same IV scheme as the host's audio packets). Pad the Opus packet with PKCS#7 before encrypting. moonlight-common-c pads twice, first to a whole block and then a full PKCS#7 block from OpenSSL; the host accepts both that and a single layer, such as Java's `AES/CBC/PKCS5Padding`. Keep a datagram within 1400 bytes.

The host matches a packet to its session by the client's IP address and the key that decrypts it.

### Audio

- Mono, 48 kHz Opus. The host decodes any frame length from 2.5 to 120 ms; 10 or 20 ms frames suit voice.
- Use the VOIP application, turn on in-band FEC (`OPUS_SET_INBAND_FEC(1)`) and give a packet loss estimate (`OPUS_SET_PACKET_LOSS_PERC`, for example 10). A single lost packet is then recovered from the next one; up to five lost packets in a row are concealed. 24 to 64 kb/s is plenty for speech.
- Send packets as they are captured; do not batch them. The host does not wait to reorder: a packet that arrives after a later one is dropped. It keeps at most 60 ms (or three packets) queued and drops the oldest audio beyond that, so a burst cannot leave the microphone behind the game.
- To mute, stop sending. Sequence numbers may continue where they left off.

### Android

- Ask for `RECORD_AUDIO` at run time, and offer the microphone as a setting that is off until the user turns it on.
- Record with `AudioRecord` from `MediaRecorder.AudioSource.VOICE_COMMUNICATION` at 48 kHz mono. That source applies the device's echo cancellation, which keeps the game audio playing from the phone's speaker out of the microphone; add `AcousticEchoCanceler` and `NoiseSuppressor` where the device has them.
- Moonlight for Android already builds libopus for audio decoding; the encoder (`opus_encoder_create`, `opus_encode`) is in the same library.
- The simplest route is to bring ClassicOldSong's `MicrophoneStream.c` and its DESCRIBE, SETUP and SDP changes into moonlight-common-c, then call `LiSendMicrophoneOpusData` for each encoded packet with `enableMic` set in the stream configuration.

### Testing

`rust/windows/examples/mic_probe.rs` runs the host half on a real PC: it sends tone bursts through the same packet, decryption, decoding and playback code and records them back from Microphone (Steam Streaming Microphone), reporting the delay from a packet's arrival to the recording app and any dropouts. The RTSP exchange and packet routing have an automated test (`host/src/stream/reconnect_tests.rs::microphone_packets_reach_only_the_session_that_set_them_up`).
