//! RTSP message parsing and responses, and the stream parameters a client
//! negotiates in its `ANNOUNCE`/`SETUP` exchange.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_MESSAGE: usize = 65536;
#[derive(Debug)]
pub struct Request {
    pub method: String,
    pub target: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub cseq: u32,
}
pub fn complete_length(b: &[u8]) -> Result<Option<usize>> {
    if b.len() > MAX_MESSAGE {
        bail!("RTSP message too large");
    }
    let Some(i) = b.windows(4).position(|b| b == b"\r\n\r\n") else {
        return Ok(None);
    };
    let head = std::str::from_utf8(&b[..i])?;
    let mut length = 0;
    let mut seen = false;
    for l in head.lines().skip(1) {
        if let Some((k, v)) = l.split_once(':')
            && k.eq_ignore_ascii_case("Content-Length")
        {
            if seen {
                bail!("duplicate Content-Length");
            }
            seen = true;
            length = v.trim().parse::<usize>()?;
        }
    }
    let total = (i + 4)
        .checked_add(length)
        .context("RTSP length overflow")?;
    if total > MAX_MESSAGE {
        bail!("RTSP message too large");
    }
    Ok(if b.len() >= total { Some(total) } else { None })
}
impl Request {
    pub fn parse(b: &[u8]) -> Result<Self> {
        let n = complete_length(b)?.context("incomplete RTSP message")?;
        if n != b.len() {
            bail!("multiple messages passed to RTSP parser");
        }
        let i = b.windows(4).position(|b| b == b"\r\n\r\n").unwrap();
        let head = std::str::from_utf8(&b[..i])?;
        let mut lines = head.lines();
        let parts: Vec<_> = lines
            .next()
            .context("empty request")?
            .split_whitespace()
            .collect();
        if parts.len() != 3 || parts[2] != "RTSP/1.0" {
            bail!("invalid RTSP request line");
        }
        let mut headers = BTreeMap::new();
        for l in lines {
            let (k, v) = l.split_once(':').context("invalid RTSP header")?;
            if headers
                .insert(k.to_ascii_lowercase(), v.trim().to_owned())
                .is_some()
            {
                bail!("duplicate RTSP header");
            }
        }
        let cseq = headers.get("cseq").context("missing CSeq")?.parse()?;
        Ok(Self {
            method: parts[0].into(),
            target: parts[1].into(),
            headers,
            body: b[i + 4..].to_vec(),
            cseq,
        })
    }
}
pub fn response(
    cseq: u32,
    code: u16,
    reason: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> Vec<u8> {
    let mut s = format!("RTSP/1.0 {code} {reason}\r\nCSeq: {cseq}\r\n");
    for (k, v) in headers {
        s.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() {
        s.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    s.push_str("\r\n");
    let mut b = s.into_bytes();
    b.extend_from_slice(body);
    b
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Negotiated {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    #[serde(default)]
    pub rate_millihz: u32,
    pub bitrate_kbps: u32,
    #[serde(default)]
    pub configured_bitrate_kbps: u32,
    #[serde(default)]
    pub csc_mode: u8,
    pub codec: u8,
    pub hdr: bool,
    #[serde(default)]
    pub sdr_10bit: bool,
    pub yuv444: bool,
    #[serde(default)]
    pub slices: u32,
    #[serde(default)]
    pub references: u32,
    #[serde(default)]
    pub intra_refresh: bool,
    #[serde(default)]
    pub vrr_low_latency: bool,
    #[serde(default)]
    pub pyrowave_records: bool,
    pub packet_size: usize,
    pub min_fec: usize,
    pub audio_channels: u8,
    pub audio_packet_ms: u8,
    pub audio_quality: bool,
    pub encryption: u32,
    pub reliable_control: u32,
    /// Moonlight asks for QoS tags on a local network and not over the
    /// internet, where routers may drop tagged packets.
    #[serde(default)]
    pub video_qos: bool,
    #[serde(default)]
    pub audio_qos: bool,
}
impl Default for Negotiated {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            rate_millihz: 0,
            bitrate_kbps: 20000,
            configured_bitrate_kbps: 0,
            csc_mode: 2,
            codec: 0,
            hdr: false,
            sdr_10bit: false,
            yuv444: false,
            slices: 1,
            references: 0,
            intra_refresh: false,
            vrr_low_latency: false,
            pyrowave_records: false,
            packet_size: 1024,
            min_fec: 0,
            audio_channels: 2,
            audio_packet_ms: 5,
            audio_quality: false,
            encryption: 1,
            reliable_control: 13,
            video_qos: true,
            audio_qos: true,
        }
    }
}
impl Negotiated {
    pub fn ten_bit(&self) -> bool {
        self.hdr || self.sdr_10bit
    }
    pub fn color_matrix(&self) -> u8 {
        if self.hdr {
            2
        } else {
            match self.csc_mode >> 1 {
                0 => 0,
                2 if self.ten_bit() => 2,
                _ => 1,
            }
        }
    }
    pub fn full_range(&self) -> bool {
        self.csc_mode & 1 != 0
    }
    pub fn fps_millihz(&self) -> u32 {
        if self.rate_millihz > 0 {
            self.rate_millihz
        } else {
            self.fps.saturating_mul(1000)
        }
    }
    pub fn from_sdp(b: &[u8]) -> Result<Self> {
        let mut attrs = BTreeMap::new();
        for l in std::str::from_utf8(b)?.lines() {
            if let Some(a) = l.trim().strip_prefix("a=")
                && let Some((k, v)) = a.split_once(':')
            {
                attrs.insert(k.trim(), v.trim());
            }
        }
        let mut n = Self::default();
        let get = |k: &str, d: u32| -> Result<u32> {
            Ok(match attrs.get(k) {
                Some(s) => s.parse()?,
                None => d,
            })
        };
        // Encoders need even sizes; round an odd client size down.
        n.width = get("x-nv-video[0].clientViewportWd", n.width)? & !1;
        n.height = get("x-nv-video[0].clientViewportHt", n.height)? & !1;
        n.fps = get("x-nv-video[0].maxFPS", n.fps)?;
        n.rate_millihz = if n.fps > 1000 {
            n.fps
        } else {
            n.fps.saturating_mul(1000)
        };
        if n.fps > 4000 {
            n.rate_millihz = n.fps;
            n.fps = n.fps.saturating_add(500) / 1000;
        }
        let refresh_x100 = get("x-nv-video[0].clientRefreshRateX100", 0)?;
        if refresh_x100 > 0 && refresh_x100.saturating_add(50) / 100 == n.fps {
            n.rate_millihz = refresh_x100
                .checked_mul(10)
                .ok_or_else(|| anyhow::anyhow!("refresh rate overflow"))?;
        }
        n.bitrate_kbps = get("x-nv-vqos[0].bw.maximumBitrateKbps", n.bitrate_kbps)?;
        n.configured_bitrate_kbps = get("x-ml-video.configuredBitrateKbps", 0)?;
        n.csc_mode = u8::try_from(get("x-nv-video[0].encoderCscMode", 0)?)?;
        n.codec = u8::try_from(get("x-nv-vqos[0].bitStreamFormat", 0)?)?;
        n.hdr = get("x-nv-video[0].dynamicRangeMode", 0)? != 0;
        n.yuv444 = get("x-ss-video[0].chromaSamplingType", 0)? != 0;
        n.slices = get("x-nv-video[0].videoEncoderSlicesPerFrame", 1)?;
        n.references = get("x-nv-video[0].maxNumReferenceFrames", 0)?;
        n.intra_refresh = get(
            "x-ss-video[0].intraRefresh",
            get("x-nv-video[0].enableIntraRefresh", 0)?,
        )? != 0;
        n.vrr_low_latency = get("x-ss-video[0].vrrLowLatency", 0)? != 0;
        n.pyrowave_records = attrs.contains_key("x-ss-video[0].pyrowaveAdaptiveFec")
            || get("x-ss-video[0].pyrowaveFeatures", 0)? & 1 != 0;
        n.packet_size = get("x-nv-video[0].packetSize", 1024)? as usize;
        n.min_fec = get("x-nv-vqos[0].fec.minRequiredFecPackets", 0)? as usize;
        n.audio_channels = u8::try_from(get("x-nv-audio.surround.numChannels", 2)?)?;
        n.audio_packet_ms = u8::try_from(get("x-nv-aqos.packetDuration", 5)?)?;
        n.audio_quality = get("x-nv-audio.surround.AudioQuality", 0)? != 0;
        n.encryption = get("x-ss-general.encryptionEnabled", 0)?;
        if get("x-nv-general.featureFlags", 0)? & 0x20 != 0 {
            n.encryption |= 4;
        }
        n.reliable_control = get("x-nv-general.useReliableUdp", 13)?;
        // Tagged unless the client says not to, as in Vibepollo.
        n.video_qos = get("x-nv-vqos[0].qosTrafficType", 5)? != 0;
        n.audio_qos = get("x-nv-aqos.qosTrafficType", 4)? != 0;
        n.validate()?;
        Ok(n)
    }
    pub fn validate(&self) -> Result<()> {
        if !(64..=16384).contains(&self.width)
            || !(64..=16384).contains(&self.height)
            || !self.width.is_multiple_of(2)
            || !self.height.is_multiple_of(2)
        {
            bail!("invalid stream dimensions");
        }
        if !(1..=4000).contains(&self.fps)
            || self.rate_millihz > 4_000_000
            || !(1..=2_000_000).contains(&self.bitrate_kbps)
            || self.configured_bitrate_kbps > i32::MAX as u32
            || self.csc_mode > 5
            || self.codec > 3
            || self.slices > 16
            || self.references > 16
        {
            bail!("invalid encoder parameters");
        }
        if !(256..=1400).contains(&self.packet_size) || self.min_fec > 255 {
            bail!("invalid packet size or FEC count");
        }
        if !matches!(self.audio_channels, 2 | 6 | 8) || !matches!(self.audio_packet_ms, 5 | 10 | 20)
        {
            bail!("unsupported audio layout");
        }
        if self.encryption & !7 != 0 {
            bail!("unsupported encryption flags");
        }
        Ok(())
    }
}
pub fn describe(
    feature_flags: u32,
    encryption: u32,
    hevc: bool,
    av1: bool,
    pyrowave: bool,
) -> String {
    let mut s = format!(
        "a=x-ss-general.featureFlags:{feature_flags}\r\na=x-ss-general.encryptionSupported:7\r\na=x-ss-general.encryptionRequested:{encryption}\r\n"
    );
    if hevc {
        s.push_str("sprop-parameter-sets=AAAAAU\r\n");
    }
    if av1 {
        s.push_str("a=rtpmap:98 AV1/90000\r\n");
    }
    if pyrowave {
        s.push_str("a=rtpmap:99 PYROWAVE/90000\r\na=x-ss-pyrowave.bitstream:186f0393\r\n");
    }
    s.push_str("a=fmtp:97 surround-params=21101\r\na=fmtp:97 surround-params=642014523\r\na=fmtp:97 surround-params=85301456723\r\na=fmtp:97 surround-params=21101\r\na=fmtp:97 surround-params=660014523\r\na=fmtp:97 surround-params=88001456723\r\n");
    s
}
pub fn codec_warning(preference: &str, codec: u8, pyrowave_available: bool) -> Option<String> {
    if preference != "pyrowave" || codec == 3 {
        return None;
    }
    let selected = ["H.264", "HEVC", "AV1"][codec as usize];
    let reason = if pyrowave_available {
        "the client selected another codec"
    } else {
        "the host did not advertise a usable PyroWave mode"
    };
    Some(format!(
        "PyroWave was selected on the host, but the client negotiated {selected}: {reason}. Quality and latency may differ; use a compatible Nonary Moonlight client and check PyroWave capability-probe warnings, or select HEVC/AV1 intentionally."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pyrowave_downgrades_report_the_negotiated_codec_without_rewriting_it() {
        assert!(
            codec_warning("pyrowave", 0, false)
                .unwrap()
                .contains("negotiated H.264")
        );
        assert!(
            codec_warning("pyrowave", 1, true)
                .unwrap()
                .contains("client selected another codec")
        );
        assert!(codec_warning("auto", 0, false).is_none());
        assert!(codec_warning("pyrowave", 3, true).is_none());
    }

    #[test]
    fn fragmented_rtsp_and_duplicate_lengths() {
        let b = b"ANNOUNCE rtsp://host RTSP/1.0\r\nCSeq: 2\r\nContent-Length: 5\r\n\r\nhello";
        for i in 0..b.len() {
            assert_eq!(complete_length(&b[..i]).unwrap(), None);
        }
        let r = Request::parse(b).unwrap();
        assert_eq!(r.body, b"hello");
        assert_eq!(r.cseq, 2);
        assert!(
            complete_length(
                b"OPTIONS * RTSP/1.0\r\nContent-Length: 0\r\nContent-Length: 1\r\n\r\n"
            )
            .is_err()
        );
    }
    #[test]
    fn sdp_negotiation_rejects_integers_that_would_truncate() {
        assert!(Negotiated::from_sdp(b"a=x-nv-video[0].clientViewportWd:4294967295\n").is_err());
        assert!(Negotiated::from_sdp(b"a=x-nv-vqos[0].bitStreamFormat:256\n").is_err());
    }
    #[test]
    fn pyrowave_framing_presence_and_feature_bit_match_vrr_clients() {
        for attribute in [
            "x-ss-video[0].pyrowaveAdaptiveFec:0",
            "x-ss-video[0].pyrowaveFeatures:1",
        ] {
            let sdp = format!(
                "a=x-nv-vqos[0].bitStreamFormat:3\na={attribute}\na=x-ss-video[0].vrrLowLatency:1\na=x-nv-vqos[0].bw.maximumBitrateKbps:800000\n"
            );
            let config = Negotiated::from_sdp(sdp.as_bytes()).unwrap();
            assert!(config.pyrowave_records && config.vrr_low_latency);
            assert_eq!(config.codec, 3);
        }
        assert!(
            !Negotiated::from_sdp(b"a=x-ss-video[0].pyrowaveFeatures:2\n")
                .unwrap()
                .pyrowave_records
        );
        assert!(
            describe(0, 0, true, true, true).contains("a=x-ss-pyrowave.bitstream:186f0393\r\n")
        );
    }
    #[test]
    fn qos_tags_follow_the_client() {
        let local = Negotiated::from_sdp(
            b"a=x-nv-vqos[0].qosTrafficType:5\na=x-nv-aqos.qosTrafficType:4\n",
        )
        .unwrap();
        assert!(local.video_qos && local.audio_qos);
        let remote = Negotiated::from_sdp(
            b"a=x-nv-vqos[0].qosTrafficType:0\na=x-nv-aqos.qosTrafficType:0\n",
        )
        .unwrap();
        assert!(!remote.video_qos && !remote.audio_qos);
        let silent = Negotiated::from_sdp(b"").unwrap();
        assert!(silent.video_qos && silent.audio_qos);
    }
    #[test]
    fn fractional_negotiation_does_not_use_stale_display_refresh() {
        let n = Negotiated::from_sdp(
            b"a=x-nv-video[0].maxFPS:60\na=x-nv-video[0].clientRefreshRateX100:5994\n",
        )
        .unwrap();
        assert_eq!(n.fps_millihz(), 59940);
        let n = Negotiated::from_sdp(
            b"a=x-nv-video[0].maxFPS:120\na=x-nv-video[0].clientRefreshRateX100:5994\n",
        )
        .unwrap();
        assert_eq!(n.fps_millihz(), 120000);
        assert_eq!(
            Negotiated::from_sdp(b"a=x-nv-video[0].maxFPS:119880\n")
                .unwrap()
                .fps_millihz(),
            119880
        );
    }
}
