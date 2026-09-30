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
    pub bitrate_kbps: u32,
    pub codec: u8,
    pub hdr: bool,
    pub yuv444: bool,
    pub packet_size: usize,
    pub min_fec: usize,
    pub audio_channels: u8,
    pub audio_packet_ms: u8,
    pub audio_quality: bool,
    pub encryption: u32,
    pub reliable_control: u32,
}
impl Default for Negotiated {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 20000,
            codec: 0,
            hdr: false,
            yuv444: false,
            packet_size: 1024,
            min_fec: 0,
            audio_channels: 2,
            audio_packet_ms: 5,
            audio_quality: false,
            encryption: 1,
            reliable_control: 13,
        }
    }
}
impl Negotiated {
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
        n.width = get("x-nv-video[0].clientViewportWd", n.width)?;
        n.height = get("x-nv-video[0].clientViewportHt", n.height)?;
        n.fps = get("x-nv-video[0].maxFPS", n.fps)?;
        n.bitrate_kbps = get("x-nv-vqos[0].bw.maximumBitrateKbps", n.bitrate_kbps)?;
        n.codec = u8::try_from(get("x-nv-vqos[0].bitStreamFormat", 0)?)?;
        n.hdr = get("x-nv-video[0].dynamicRangeMode", 0)? != 0;
        n.yuv444 = get("x-ss-video[0].chromaSamplingType", 0)? != 0;
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
        if !(1..=1000).contains(&self.fps)
            || !(100..=500000).contains(&self.bitrate_kbps)
            || self.codec > 3
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
        s.push_str("a=rtpmap:99 PYROWAVE/90000\r\n");
    }
    s.push_str("a=fmtp:97 surround-params=21101\r\na=fmtp:97 surround-params=642014523\r\na=fmtp:97 surround-params=85301456723\r\na=fmtp:97 surround-params=642012345\r\na=fmtp:97 surround-params=85301234567\r\n");
    s
}
#[cfg(test)]
mod tests {
    use super::*;
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
}
