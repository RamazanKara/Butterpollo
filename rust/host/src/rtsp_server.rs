use crate::state::{Launch, Shared};
use anyhow::{Context, Result, bail};
use butterpollo_core::{
    crypto,
    rtsp::{self, Negotiated},
};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub async fn serve(address: SocketAddr, h: Shared, media: Arc<crate::stream::Media>) -> Result<()> {
    let listener = crate::network::tcp(address)?;
    let configurations = Arc::new(Mutex::new(HashMap::new()));
    tracing::info!(%address,"RTSP listener ready");
    loop {
        let (socket, peer) = crate::network::accept(&listener).await;
        let h = h.clone();
        let media = media.clone();
        let configs = configurations.clone();
        tokio::spawn(async move {
            if let Err(e) = connection(socket, peer, h, media, configs).await {
                // A client closing a connection early is routine; anything else
                // prevents a stream from starting and must be visible.
                let closed = e.downcast_ref::<std::io::Error>().is_some_and(|e| {
                    matches!(
                        e.kind(),
                        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
                    )
                });
                if closed {
                    tracing::debug!(%peer, error = %e, "RTSP connection closed");
                } else {
                    tracing::warn!(%peer, error = %format!("{e:#}"), "RTSP request failed");
                }
            }
        });
    }
}
async fn read_message(socket: &mut TcpStream, launches: &[Launch]) -> Result<(Launch, Vec<u8>)> {
    let mut first = [0; 4];
    socket.read_exact(&mut first).await?;
    let word = u32::from_be_bytes(first);
    if word & 0x80000000 != 0 {
        let size = (word & 0x7fffffff) as usize;
        if size > rtsp::MAX_MESSAGE {
            bail!("encrypted RTSP message too large");
        }
        let mut header = [0; 20];
        socket.read_exact(&mut header).await?;
        let mut encrypted = vec![0; size];
        socket.read_exact(&mut encrypted).await?;
        let mut iv = [0; 12];
        iv[..4].copy_from_slice(&u32::from_be_bytes(header[..4].try_into().unwrap()).to_le_bytes());
        iv[10] = b'C';
        iv[11] = b'R';
        let sequence = u32::from_be_bytes(header[..4].try_into().unwrap());
        for launch in launches.iter().filter(|l| l.rtsp_encrypted) {
            if let Ok(raw) = crypto::gcm_open(&launch.key, &iv, &header[4..], &encrypted) {
                if !launch.rtsp_received.lock().unwrap().accept(sequence) {
                    bail!("replayed RTSP message");
                }
                return Ok((launch.clone(), raw));
            }
        }
        bail!("RTSP authentication failed");
    }
    if launches.len() != 1 {
        bail!("ambiguous legacy RTSP peer; use encrypted RTSP");
    }
    let launch = &launches[0];
    if launch.rtsp_encrypted {
        bail!("unencrypted RTSP message for encrypted launch");
    }
    let mut b = first.to_vec();
    loop {
        if let Some(total) = rtsp::complete_length(&b)? {
            if b.len() != total {
                bail!("unexpected RTSP trailing data");
            }
            return Ok((launch.clone(), b));
        }
        let mut byte = [0; 1];
        socket.read_exact(&mut byte).await?;
        b.push(byte[0]);
    }
}
async fn connection(
    mut socket: TcpStream,
    peer: SocketAddr,
    h: Shared,
    media: Arc<crate::stream::Media>,
    configs: Arc<Mutex<HashMap<String, Negotiated>>>,
) -> Result<()> {
    socket.set_nodelay(true)?;
    let (launches, ids, expired) = {
        let mut sessions = h.sessions.lock().unwrap();
        let expired = sessions.expire();
        let launches = sessions.rtsp_for_peer(peer.ip());
        let ids: std::collections::HashSet<_> = sessions
            .pending
            .keys()
            .chain(sessions.active.keys())
            .cloned()
            .collect();
        (launches, ids, expired)
    };
    drop(expired);
    if launches.is_empty() {
        bail!("no authorized RTSP launch");
    }
    configs.lock().unwrap().retain(|id, _| ids.contains(id));

    {
        let (launch, raw) = tokio::time::timeout(
            Duration::from_secs(10),
            read_message(&mut socket, &launches),
        )
        .await??;
        let req = rtsp::Request::parse(&raw)?;
        h.request_codec_probe();
        let ports = h.config.read().unwrap().ports()?;
        let mut headers = vec![];
        let mut body = Vec::new();
        let mut code = 200;
        let mut reason = "OK";
        match req.method.as_str() {
            "OPTIONS" => headers.push((
                "Public",
                "OPTIONS, DESCRIBE, SETUP, ANNOUNCE, PLAY, TEARDOWN".to_owned(),
            )),
            "DESCRIBE" => {
                let flags = h.codecs.load(std::sync::atomic::Ordering::Acquire);
                let config = crate::stream::effective_config(&h, &launch)?;
                let encryption_mode = crate::network::encryption_mode(&config, peer.ip());
                let input_capabilities = butterpollo_windows::input::capabilities(&config);
                if config.boolean("mouse", true)
                    && config.boolean("native_pen_touch", true)
                    && input_capabilities & 1 == 0
                {
                    launch.warnings.set("input_touch_pen", "Native touch and pen unavailable: Windows does not expose synthetic pointer input. The client may emulate a mouse instead; update Windows or disable native touch/pen in Input settings.");
                }
                body = rtsp::describe(
                    input_capabilities,
                    if encryption_mode == 2 { 7 } else { 1 },
                    flags & 0x100 != 0,
                    flags & 0x10000 != 0,
                    flags & 0x800000 != 0,
                )
                .into_bytes();
                // NVENC recovers lost references whenever the driver can, as
                // in Vibepollo; AMF only with long-term references enabled.
                let backend = *h.probed_encoder.lock().unwrap();
                if flags & 0x40000000 != 0
                    && (backend == "nvenc"
                        || (backend == "amf" && config.integer("amd_ltr_frames", 0) > 0))
                {
                    body.extend_from_slice(b"a=x-nv-video[0].refPicInvalidation:1\r\n");
                }
                if encryption_mode == 0 {
                    body = String::from_utf8(body)?
                        .replace("encryptionSupported:7", "encryptionSupported:5")
                        .into_bytes();
                }
                if let Some(custom) = launch.options.get("surroundParams")
                    && butterpollo_core::audio::OpusLayout::valid_custom(custom)
                {
                    let at = body
                        .windows(9)
                        .position(|b| b == b"a=fmtp:97")
                        .unwrap_or(body.len());
                    body.splice(at..at, format!("a=fmtp:97 surround-params={custom}\r\na=fmtp:97 surround-params={custom}\r\n").bytes());
                }
                headers.push(("Content-Type", "application/sdp".into()));
            }
            "SETUP" => {
                let port = if req.target.contains("=audio") {
                    ports.audio
                } else if req.target.contains("=video") {
                    ports.video
                } else if req.target.contains("=control") {
                    ports.control
                } else {
                    bail!("unknown stream setup target")
                };
                headers.push(("Session", "DEADBEEFCAFE;timeout = 90".into()));
                headers.push(("Transport", format!("server_port={port}")));
                if port == ports.control {
                    headers.push(("X-SS-Connect-Data", launch.connect_data.to_string()));
                } else {
                    headers.push(("X-SS-Ping-Payload", launch.ping.clone()));
                }
            }
            "ANNOUNCE" => {
                let mut negotiated = Negotiated::from_sdp(&req.body)?;
                if negotiated.audio_channels == 2
                    && let Some(host) = req.headers.get("host")
                {
                    negotiated.audio_quality = !host.contains("0.0.0.0");
                }
                let config = crate::stream::effective_config(&h, &launch)?;
                let required_encryption = crate::network::encryption_mode(&config, peer.ip()) == 2;
                // The FEC and audio share of the client's bitrate is expected;
                // only a host cap is worth a warning.
                let uncapped = butterpollo_core::stream_policy::uncapped_bitrate_kbps(
                    &negotiated,
                    launch.requested_rate,
                    &config,
                );
                butterpollo_core::stream_policy::apply(
                    &mut negotiated,
                    launch.requested_rate,
                    &config,
                );
                butterpollo_core::stream_policy::report_bitrate(
                    &launch.warnings,
                    uncapped,
                    negotiated.bitrate_kbps,
                    &format!(
                        "Maximum bitrate (max_bitrate) is {} Kbps",
                        config.integer("max_bitrate", 0)
                    ),
                );
                butterpollo_core::stream_policy::apply_color(&mut negotiated, &config);
                negotiated.validate()?;
                negotiated.vrr_low_latency |= launch.vrr_requested;
                let flags = h.codecs.load(std::sync::atomic::Ordering::Acquire);
                if let Some(message) = rtsp::codec_warning(
                    config.get("encoder", "auto"),
                    negotiated.codec,
                    flags & 0x0780_0000 != 0,
                ) {
                    launch.warnings.set("pyrowave_negotiation", message);
                } else {
                    launch.warnings.clear("pyrowave_negotiation");
                }
                if negotiated.codec == 3 && !negotiated.pyrowave_records {
                    launch.warnings.set("pyrowave_fec", "PyroWave adaptive FEC was not negotiated by the client; recovery protection is reduced. Use a compatible Nonary Moonlight client, or HEVC/AV1 on a lossy network.");
                }
                let bit = match (negotiated.codec, negotiated.ten_bit()) {
                    (0, false) => 1,
                    (1, false) => 0x100,
                    (1, true) => 0x200,
                    (2, false) => 0x10000,
                    (2, true) => 0x20000,
                    (3, false) if negotiated.yuv444 => 0x1000000,
                    (3, true) if negotiated.yuv444 => 0x4000000,
                    (3, true) => 0x2000000,
                    (3, false) => 0x800000,
                    _ => 0,
                };
                if required_encryption && negotiated.encryption & 6 != 6 {
                    code = 403;
                    reason = "Required audio and video encryption was not negotiated";
                } else if (negotiated.ten_bit() && negotiated.codec == 0) || flags & bit == 0 {
                    code = 406;
                    reason = "Requested codec is unavailable";
                } else {
                    configs
                        .lock()
                        .unwrap()
                        .insert(launch.id.clone(), negotiated);
                }
            }
            "PLAY" => {
                let config = configs
                    .lock()
                    .unwrap()
                    .remove(&launch.id)
                    .context("ANNOUNCE required before PLAY")?;
                let session = h.sessions.lock().unwrap().start(launch.clone(), config)?;
                media.start(h.clone(), session);
            }
            "TEARDOWN" => {
                h.sessions.lock().unwrap().request_stop(Some(&launch.id));
            }
            _ => {
                code = 405;
                reason = "Method Not Allowed";
            }
        }
        if code != 200 {
            tracing::warn!(
                client = %launch.client.name,
                method = %req.method,
                code,
                reason,
                "RTSP request refused"
            );
        }
        let mut response = rtsp::response(req.cseq, code, reason, &headers, &body);
        if launch.rtsp_encrypted {
            let mut iv = [0; 12];
            let counter = launch
                .rtsp_counter
                .fetch_update(
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                    |n| n.checked_add(1),
                )
                .map_err(|_| anyhow::anyhow!("RTSP nonce exhausted"))?;
            iv[..4].copy_from_slice(&counter.to_le_bytes());
            iv[10] = b'H';
            iv[11] = b'R';
            let (tag, b) = crypto::gcm_seal(&launch.key, &iv, &response)?;
            response = ((b.len() as u32) | 0x80000000).to_be_bytes().to_vec();
            response.extend_from_slice(&counter.to_be_bytes());
            response.extend_from_slice(&tag);
            response.extend_from_slice(&b);
        }
        socket.write_all(&response).await?;
        socket.flush().await?;
        // Moonlight reads each response to EOF and opens a fresh TCP connection
        // for the next request. Close after the complete authenticated response.
        Ok(())
    }
}
