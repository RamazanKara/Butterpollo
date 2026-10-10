//! Microphone audio a client sends to the host, as Apollo's microphone
//! passthrough sends it: ClassicOldSong/moonlight-common-c
//! (`MicrophoneStream.c`) with Apollo pull request #1428. A client that
//! speaks it to Apollo works here unchanged.
//!
//! The host offers the stream in its `DESCRIBE` answer (an `m=audio` line
//! with mono Opus at 48 kHz) and asks for it to be encrypted. A client that
//! wants it sets up `streamid=mic`, learns the port from `Transport`, and
//! sends one datagram per Opus packet: a 12-byte little-endian header
//! (flags, type 0x61, sequence, a millisecond timestamp, magic 0x12345678)
//! followed by the packet, AES-128-CBC under the launch's remote input key.
//! The IV is the big-endian sum of the launch's key ID and the sequence
//! number, then twelve zero bytes, the same IV the host's own audio packets
//! use. Nothing goes back to the client.
//!
//! The client pads twice: `PltEncryptMessage` first pads the packet with
//! PKCS#7 up to the next block (nothing when it is already whole blocks,
//! one packet in sixteen), then OpenSSL or mbed TLS adds a full PKCS#7
//! block of its own. Both layers come off here; the inner one only when it
//! is valid, so a client that pads once works too. An unpadded packet whose
//! last bytes happen to look like padding is cut short, about once in 4000
//! packets, which Opus treats as a damaged frame.
//!
//! The host decodes packets as they arrive and does not wait to reorder
//! them: on a LAN that wait would cost more latency than the rare
//! reordering it saves. A packet behind the last one decoded is dropped,
//! and a short gap is covered by Opus loss concealment and the in-band FEC
//! of the packet after it.
use crate::crypto;
use anyhow::{Result, bail};
use std::collections::VecDeque;

/// `x-ss-general.encryption*` bit for microphone packets (`SS_ENC_MICROPHONE`).
pub const ENCRYPTION: u32 = 0x08;
pub const PACKET_TYPE_OPUS: u8 = 0x61;
pub const MAGIC: u32 = 0x1234_5678;
pub const HEADER: usize = 12;
/// The port is the base port plus this, after video (9), control (10) and
/// audio (11), as in Apollo.
pub const PORT_OFFSET: u16 = 12;
/// Samples per second; the stream is mono.
pub const RATE: usize = 48_000;

/// The `DESCRIBE` lines that offer the stream. Clients look for the
/// `rtpmap` line to decide whether the host takes a microphone.
pub fn sdp(port: u16) -> String {
    format!(
        "m=audio {port} RTP/AVP 96\r\na=rtpmap:96 opus/48000/1\r\na=fmtp:96 minptime=10;useinbandfec=1\r\n"
    )
}

#[derive(Debug, PartialEq, Eq)]
pub struct Packet<'a> {
    pub sequence: u16,
    /// The client's clock in milliseconds; only for logs.
    pub timestamp: u32,
    pub payload: &'a [u8],
}
/// A microphone datagram's header and payload, or `None` for anything else.
pub fn parse(datagram: &[u8]) -> Option<Packet<'_>> {
    if datagram.len() <= HEADER
        || datagram[1] != PACKET_TYPE_OPUS
        || u32::from_le_bytes(datagram[8..12].try_into().unwrap()) != MAGIC
    {
        return None;
    }
    Some(Packet {
        sequence: u16::from_le_bytes([datagram[2], datagram[3]]),
        timestamp: u32::from_le_bytes(datagram[4..8].try_into().unwrap()),
        payload: &datagram[HEADER..],
    })
}
fn iv(key_id: u32, sequence: u16) -> [u8; 16] {
    let mut iv = [0; 16];
    iv[..4].copy_from_slice(&key_id.wrapping_add(u32::from(sequence)).to_be_bytes());
    iv
}
/// The Opus packet inside an encrypted payload.
pub fn open(key: &[u8; 16], key_id: u32, packet: &Packet) -> Result<Vec<u8>> {
    if packet.payload.is_empty() {
        bail!("empty microphone payload");
    }
    let mut opus = crypto::cbc_open(key, &iv(key_id, packet.sequence), packet.payload)?;
    if let Some(&last) = opus.last()
        && opus.len().is_multiple_of(16)
    {
        let pad = usize::from(last);
        if (1..16).contains(&pad) && opus[opus.len() - pad..].iter().all(|&b| b == last) {
            opus.truncate(opus.len() - pad);
        }
    }
    if opus.is_empty() {
        bail!("empty microphone packet");
    }
    Ok(opus)
}
/// An encrypted datagram as a client sends it, padding included, for tests
/// and probes.
pub fn seal(key: &[u8; 16], key_id: u32, sequence: u16, timestamp: u32, opus: &[u8]) -> Vec<u8> {
    let mut datagram = vec![0, PACKET_TYPE_OPUS];
    datagram.extend_from_slice(&sequence.to_le_bytes());
    datagram.extend_from_slice(&timestamp.to_le_bytes());
    datagram.extend_from_slice(&MAGIC.to_le_bytes());
    let mut padded = opus.to_vec();
    let pad = (16 - opus.len() % 16) % 16;
    padded.resize(opus.len() + pad, pad as u8);
    datagram.extend_from_slice(&crypto::cbc_seal(key, &iv(key_id, sequence), &padded));
    datagram
}

/// How to decode a packet that just arrived.
#[derive(Debug, PartialEq, Eq)]
pub enum Arrival {
    /// The packet that was due.
    Next,
    /// `missing` packets before it never came: conceal all but the last,
    /// recover the last from this packet's in-band FEC, then decode it.
    AfterLoss { missing: u16 },
    /// A gap too long to conceal, or a client that started counting again:
    /// decode it as the start of new speech.
    Restart,
    /// Already decoded, or behind the last one decoded: drop it.
    Late,
}
/// Longest gap covered by concealment, in packets (100 ms of 20 ms packets).
/// Longer gaps are a pause, and concealing them would only add a smear.
const MAX_CONCEALED: u16 = 5;
/// Late packets in a row that mean the client's count started again.
const LATE_RESTART: u32 = 16;
#[derive(Default)]
pub struct Sequencer {
    next: Option<u16>,
    late_run: u32,
}
impl Sequencer {
    pub fn arrive(&mut self, sequence: u16) -> Arrival {
        let Some(next) = self.next else {
            self.next = Some(sequence.wrapping_add(1));
            return Arrival::Restart;
        };
        let ahead = sequence.wrapping_sub(next);
        if ahead >= 0x8000 {
            self.late_run += 1;
            if self.late_run < LATE_RESTART {
                return Arrival::Late;
            }
        }
        self.late_run = 0;
        self.next = Some(sequence.wrapping_add(1));
        match ahead {
            0 => Arrival::Next,
            1..=MAX_CONCEALED => Arrival::AfterLoss { missing: ahead },
            _ => Arrival::Restart,
        }
    }
}

/// Decoded samples waiting for the render device. The client delivers whole
/// packets and Windows takes a device period at a time; the queue absorbs
/// that and the network's jitter. When it holds more than three packets'
/// worth (at least 60 ms), from a burst or a client clock that runs fast,
/// the oldest samples go, down to one packet (at least 20 ms), so the
/// microphone never drifts behind the speaker.
#[derive(Default)]
pub struct Playout {
    queue: VecDeque<f32>,
    /// Samples dropped to keep the delay bounded.
    pub trimmed: u64,
}
impl Playout {
    pub fn push(&mut self, samples: &[f32]) {
        self.queue.extend(samples.iter().map(|s| s.clamp(-1., 1.)));
        let limit = (3 * samples.len()).max(RATE * 60 / 1000);
        if self.queue.len() > limit {
            let keep = samples.len().max(RATE * 20 / 1000);
            let drop = self.queue.len() - keep;
            self.queue.drain(..drop);
            self.trimmed += drop as u64;
        }
    }
    pub fn len(&self) -> usize {
        self.queue.len()
    }
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
    /// Moves up to `out.len()` samples into `out`; returns how many.
    pub fn take(&mut self, out: &mut [f32]) -> usize {
        let n = out.len().min(self.queue.len());
        for (slot, sample) in out.iter_mut().zip(self.queue.drain(..n)) {
            *slot = sample;
        }
        n
    }
    pub fn clear(&mut self) {
        self.queue.clear();
    }
}
/// Frames to write to a render buffer of `buffer` frames that still holds
/// `padding` unplayed frames and plays `period` frames per wake-up, with
/// `queued` decoded frames waiting: (samples, silence). The device is kept
/// at most two periods ahead, so the waiting audio stays in the queue,
/// where `Playout` bounds it. Silence goes in only when the device would
/// otherwise run dry before the next wake-up.
pub fn render_plan(buffer: usize, padding: usize, period: usize, queued: usize) -> (usize, usize) {
    let room = buffer.saturating_sub(padding);
    let samples = queued.min((2 * period).saturating_sub(padding)).min(room);
    let silence = period.saturating_sub(padding + samples).min(room - samples);
    (samples, silence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datagrams_match_the_apollo_client_layout() {
        let key = [7; 16];
        let datagram = seal(&key, 0xffff_fffe, 3, 0x0102_0304, b"opus frame");
        // flags, type, LE sequence, LE timestamp, LE magic.
        assert_eq!(
            &datagram[..HEADER],
            &[0, 0x61, 3, 0, 4, 3, 2, 1, 0x78, 0x56, 0x34, 0x12]
        );
        // The packet padded to a block, and a block of padding after it.
        assert_eq!(datagram.len(), HEADER + 32);
        let packet = parse(&datagram).unwrap();
        assert_eq!((packet.sequence, packet.timestamp), (3, 0x0102_0304));
        // The key ID and sequence add with wrap-around, as in C.
        let mut iv = [0; 16];
        iv[..4].copy_from_slice(&1u32.to_be_bytes());
        let mut inner = b"opus frame".to_vec();
        inner.extend([6; 6]);
        assert_eq!(crypto::cbc_open(&key, &iv, packet.payload).unwrap(), inner);
        assert_eq!(open(&key, 0xffff_fffe, &packet).unwrap(), b"opus frame");
        // Ciphertext from moonlight-common-c's own PltEncryptMessage (OpenSSL),
        // key 07.., key ID 9, sequence 70, packet bytes 0x20, 0x21, ...
        for (length, hex) in [
            (
                4,
                "306a36d0512bb803ccc69a69ae7e2aed501d20abae2a575017bdb0ca2cf32fc3",
            ),
            (
                16,
                "1e10689ae2b276383d9f36756341ba398f6060d44c12333163d5a710f9c2c20e",
            ),
            (
                33,
                "1e10689ae2b276383d9f36756341ba3996287332b3430f8ba7119c451d482a13c19f4f2afd4574a761afc77c968ba6025e91b8ea56f889d013870cd329a41bd6",
            ),
        ] {
            let opus: Vec<u8> = (0x20..0x20 + length).collect();
            let datagram = seal(&key, 9, 70, 0, &opus);
            assert_eq!(hex::encode(&datagram[HEADER..]), hex, "{length}");
            assert_eq!(open(&key, 9, &parse(&datagram).unwrap()).unwrap(), opus);
        }
        // A client that pads once.
        let once = Packet {
            sequence: 70,
            timestamp: 0,
            payload: &crypto::cbc_seal(&key, &super::iv(9, 70), &[1, 2, 3]),
        };
        assert_eq!(open(&key, 9, &once).unwrap(), [1, 2, 3]);
        assert_ne!(
            open(&[8; 16], 0xffff_fffe, &packet).ok().as_deref(),
            Some(&b"opus frame"[..])
        );
    }

    #[test]
    fn other_datagrams_are_not_microphone_packets() {
        let datagram = seal(&[1; 16], 0, 0, 0, b"x");
        assert!(parse(&datagram[..HEADER]).is_none());
        let mut wrong_type = datagram.clone();
        wrong_type[1] = 0x60;
        assert!(parse(&wrong_type).is_none());
        let mut wrong_magic = datagram.clone();
        wrong_magic[8] ^= 1;
        assert!(parse(&wrong_magic).is_none());
        for payload in [&[0; 15][..], &[]] {
            let packet = Packet {
                sequence: 0,
                timestamp: 0,
                payload,
            };
            assert!(open(&[1; 16], 0, &packet).is_err());
        }
    }

    #[test]
    fn the_offer_is_what_clients_look_for() {
        let sdp = sdp(48001);
        assert!(sdp.starts_with("m=audio 48001 RTP/AVP 96\r\n"));
        assert!(sdp.contains("a=rtpmap:96 opus/48000/1\r\n"));
    }

    #[test]
    fn gaps_are_concealed_late_packets_dropped_and_restarts_followed() {
        let mut s = Sequencer::default();
        assert_eq!(s.arrive(65534), Arrival::Restart);
        assert_eq!(s.arrive(65535), Arrival::Next);
        // Wrap-around is the next packet.
        assert_eq!(s.arrive(0), Arrival::Next);
        assert_eq!(s.arrive(0), Arrival::Late);
        assert_eq!(s.arrive(65535), Arrival::Late);
        assert_eq!(s.arrive(3), Arrival::AfterLoss { missing: 2 });
        // A packet that was concealed and then arrives is late.
        assert_eq!(s.arrive(2), Arrival::Late);
        assert_eq!(s.arrive(4), Arrival::Next);
        assert_eq!(
            s.arrive(4 + 1 + MAX_CONCEALED),
            Arrival::AfterLoss {
                missing: MAX_CONCEALED
            }
        );
        assert_eq!(s.arrive(100), Arrival::Restart);
        assert_eq!(s.arrive(101), Arrival::Next);
        // A client that counts from 0 again is followed after a short run.
        for n in 0..LATE_RESTART - 1 {
            assert_eq!(s.arrive(n as u16), Arrival::Late);
        }
        assert_eq!(s.arrive(LATE_RESTART as u16 - 1), Arrival::Restart);
        assert_eq!(s.arrive(LATE_RESTART as u16), Arrival::Next);
    }

    #[test]
    fn the_queue_stays_within_three_packets_and_keeps_the_newest() {
        let mut p = Playout::default();
        let packet: Vec<f32> = (0..960).map(|n| n as f32 / 960.).collect();
        for _ in 0..3 {
            p.push(&packet);
        }
        assert_eq!((p.len(), p.trimmed), (2880, 0));
        p.push(&packet);
        // Cut back to one packet, the newest.
        assert_eq!((p.len(), p.trimmed), (960, 2880));
        let mut out = vec![0.; 1000];
        assert_eq!(p.take(&mut out), 960);
        assert_eq!(out[959], 959. / 960.);
        assert!(p.is_empty());
        // Small packets still get 60 ms before anything goes.
        for _ in 0..12 {
            p.push(&[2.; 240]);
        }
        assert_eq!((p.len(), p.trimmed), (2880, 2880));
        assert_eq!(p.take(&mut out[..1]), 1);
        assert_eq!(out[0], 1., "samples are clamped");
    }

    #[test]
    fn the_device_is_kept_two_periods_ahead_and_never_runs_dry() {
        // 10 ms periods in a 20 ms buffer.
        assert_eq!(render_plan(960, 0, 480, 0), (0, 480));
        assert_eq!(render_plan(960, 0, 480, 960), (960, 0));
        assert_eq!(render_plan(960, 480, 480, 960), (480, 0));
        assert_eq!(render_plan(960, 480, 480, 100), (100, 0));
        assert_eq!(render_plan(960, 100, 480, 100), (100, 280));
        assert_eq!(render_plan(960, 960, 480, 960), (0, 0));
        // A larger buffer is not filled past two periods.
        assert_eq!(render_plan(4800, 0, 480, 4000), (960, 0));
    }
}
