use super::*;
use crate::state::{OneTimePin, test_support::Fixture};
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use std::sync::atomic::Ordering;
use tower::ServiceExt;

async fn document(response: Response) -> xmltree::Element {
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("application/xml")
    );
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    xmltree::Element::parse(bytes.as_ref()).unwrap()
}
fn field(root: &xmltree::Element, name: &str) -> String {
    root.get_child(name)
        .unwrap()
        .get_text()
        .unwrap_or_default()
        .into_owned()
}
async fn request(f: &Fixture, https: bool, method: &str, path: &str) -> Response {
    router(f.host.clone(), https)
        .layer(Extension(f.connection(https)))
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn xml_preserves_protocol_status_and_escapes_values_and_errors() {
    let text = "<Game & \"screen\"> '日本語'";
    let root = document(xml(403, &[("AppTitle", text.into())], Some(text.into()))).await;
    assert_eq!(root.attributes["status_code"], "403");
    assert_eq!(root.attributes["status_message"], text);
    assert_eq!(field(&root, "AppTitle"), text);
    assert_eq!(root.children.len(), 1);
}

#[tokio::test]
async fn serverinfo_extras_preserve_permissions_codecs_commands_and_limiter_units() {
    let f = Fixture::new();
    let client = f.client(0x071f1f00);
    let config = butterpollo_core::config::Config::parse("sunshine_name=PC <&>\nvirtual_display_mode=shared\nframe_limiter_fps_limit=59.94\nserver_cmd=[{\"name\":\"Lock <&>\",\"cmd\":\"must never run\"},{\"name\":\"Sleep\"}]").unwrap();
    f.host
        .codecs
        .store(0x4000_0000 | 0x0003_0301, Ordering::Release);
    for ready in [false, true] {
        let root = document(serverinfo_response(
            &f.host,
            &f.connection(true),
            &config,
            Some(&client),
            true,
            "01:02:03:04:05:06".into(),
            1000,
            ready,
        ))
        .await;
        for (name, expected) in [
            ("hostname", "PC <&>"),
            ("PairStatus", "1"),
            ("ServerCodecModeSupport", "197377"),
            ("MaxLumaPixelsHEVC", "1869449984"),
            ("FrameLimiterSupported", "1"),
            ("FrameLimiterEnabled", "1"),
            ("VirtualDisplayFrameLimiterEnabled", "1"),
            ("FrameLimiterFpsLimitMilliHz", "59940"),
            ("VirtualDisplayCapable", "true"),
            ("VirtualDisplayHDRCapable", "true"),
            ("state", "SUNSHINE_SERVER_FREE"),
            ("currentgame", "0"),
            ("RustHostSessionCount", "0"),
            ("RustHostPendingSessionCount", "0"),
            ("LocalIP", "127.0.0.1"),
            ("mac", "01:02:03:04:05:06"),
            ("PyroWaveHostLinkMbps", "1000"),
            ("PyroWaveBandwidthProbeBytes", "33554432"),
        ] {
            assert_eq!(field(&root, name), expected, "{name}");
        }
        assert_eq!(field(&root, "Permission"), client.perm.to_string());
        assert_eq!(field(&root, "VirtualDisplayDriverReady"), ready.to_string());
        let commands: Vec<_> = root
            .children
            .iter()
            .filter_map(xmltree::XMLNode::as_element)
            .filter(|e| e.name == "ServerCommand")
            .map(|e| e.get_text().unwrap().into_owned())
            .collect();
        assert_eq!(commands, ["Lock <&>", "Sleep"]);
    }
    let mut c = f.connection(false);
    c.local = "[::1]:47989".parse().unwrap();
    c.peer = "[2001:db8::1]:12345".parse().unwrap();
    let root = document(serverinfo_response(
        &f.host,
        &c,
        &config,
        None,
        true,
        "00:00:00:00:00:00".into(),
        0,
        false,
    ))
    .await;
    assert_eq!(field(&root, "LocalIP"), "127.0.0.1");
    assert_eq!(field(&root, "Permission"), "0");
    assert_eq!(field(&root, "PairStatus"), "0");
    assert_eq!(field(&root, "RustHostProfile"), "");
    assert_eq!(field(&root, "RustHostSessionCount"), "");
    assert_eq!(field(&root, "PyroWaveBandwidthProbeBytes"), "0");
    assert!(root.get_child("ServerCommand").is_none());
    let mut restricted = client;
    restricted.perm &= !(1 << 20);
    let root = document(serverinfo_response(
        &f.host,
        &f.connection(true),
        &config,
        Some(&restricted),
        true,
        String::new(),
        0,
        false,
    ))
    .await;
    assert!(root.get_child("ServerCommand").is_none());
}

#[tokio::test]
async fn successful_launch_and_resume_xml_include_driver_readiness_and_the_called_endpoint() {
    for (reply, resume) in [("gamesession", false), ("resume", true)] {
        for ready in [false, true] {
            let url = "rtspenc://[::1]:48010";
            let root = document(launch_response(
                Ok((reply.into(), url.into())),
                1,
                reply,
                resume,
                ready,
            ))
            .await;
            assert_eq!(root.attributes["status_code"], "200");
            assert_eq!(field(&root, reply), "1");
            assert_eq!(field(&root, "sessionUrl0"), url);
            assert_eq!(field(&root, "VirtualDisplayDriverReady"), ready.to_string());
            assert_eq!(root.children.len(), 3);
        }
    }
}

#[test]
fn paired_permissions_require_tls_an_enabled_certificate_and_each_requested_bit() {
    let f = Fixture::new();
    f.client(1 << 25);
    let c = f.connection(true);
    assert!(authenticated_viewer(&f.host, &c).is_ok());
    assert!(authenticated(&f.host, &c, 1 << 26).is_err());
    assert!(authenticated(&f.host, &c, VIEW).is_err());
    assert!(authenticated(&f.host, &f.connection(false), 0).is_err());
    let mut unknown = c.clone();
    unknown.certificate = Some(vec![1, 2, 3]);
    assert!(authenticated(&f.host, &unknown, 0).is_err());
    unknown.certificate = None;
    assert!(authenticated(&f.host, &unknown, 0).is_err());
    f.host.paired.write().unwrap().clients[0].perm = 1 << 26;
    assert!(authenticated_viewer(&f.host, &c).is_ok());
    assert!(validate_launch_client(&f.host, &c, None, false).is_ok());
    f.host.paired.write().unwrap().clients[0].enabled = false;
    assert!(authenticated(&f.host, &c, 0).is_err());
    assert!(authenticated_viewer(&f.host, &c).is_err());
}

fn otp_request(f: &Fixture) -> Args {
    Args::from([
        ("uniqueid".into(), "otp-fixture".into()),
        ("phrase".into(), "getservercert".into()),
        ("salt".into(), hex::encode([0x12; 16])),
        (
            "clientcert".into(),
            hex::encode(f.host.identity.certificate.as_bytes()),
        ),
        ("devicename".into(), "Client name".into()),
        (
            "otpauth".into(),
            hex::encode(crypto::hash(
                format!("1234{}passphrase", hex::encode([0x12; 16])).as_bytes(),
            )),
        ),
    ])
}
fn set_otp(f: &Fixture, age: Duration) {
    *f.host.otp.lock().unwrap() = Some(OneTimePin {
        pin: "1234".into(),
        passphrase: "passphrase".into(),
        device_name: "Console name".into(),
        created: Instant::now() - age,
    });
}

#[tokio::test]
async fn otp_handshake_persists_the_device_name_and_consumes_the_pin_once() {
    let f = Fixture::new();
    set_otp(&f, Duration::ZERO);
    let args = otp_request(&f);
    let peer = "127.0.0.1".parse().unwrap();
    let start = do_pair(f.host.clone(), &args, peer).await.unwrap();
    assert_eq!(start[0], ("paired".into(), "1".into()));
    assert!(f.host.otp.lock().unwrap().is_none());
    let aes = crypto::pin_key(&[0x12; 16], "1234");
    let challenge = [0x34; 16];
    let step = |name: &str, value: Vec<u8>| {
        Args::from([
            ("uniqueid".into(), "otp-fixture".into()),
            (name.into(), hex::encode(value)),
        ])
    };
    let response = do_pair(
        f.host.clone(),
        &step(
            "clientchallenge",
            crypto::ecb(&aes, &challenge, true).unwrap(),
        ),
        peer,
    )
    .await
    .unwrap();
    let response = crypto::ecb(&aes, &hex::decode(&response[1].1).unwrap(), false).unwrap();
    let secret = [0x56; 16];
    let proof = crypto::hash(&[&response[32..], &f.host.identity.signature, &secret].concat());
    let server = do_pair(
        f.host.clone(),
        &step(
            "serverchallengeresp",
            crypto::ecb(&aes, &proof, true).unwrap(),
        ),
        peer,
    )
    .await
    .unwrap();
    let server = hex::decode(&server[1].1).unwrap();
    assert_eq!(
        &response[..32],
        crypto::hash(
            &[
                challenge.as_slice(),
                &f.host.identity.signature,
                &server[..16]
            ]
            .concat()
        )
    );
    assert!(crypto::verify(&f.host.identity.certificate, &server[..16], &server[16..]).unwrap());
    do_pair(
        f.host.clone(),
        &step(
            "clientpairingsecret",
            [&secret[..], &f.host.identity.sign(&secret)].concat(),
        ),
        peer,
    )
    .await
    .unwrap();
    let saved = butterpollo_core::state::PairedState::load(&f.host.paired_path).unwrap();
    assert_eq!(saved.clients.len(), 1);
    assert_eq!(saved.clients[0].name, "Console name");
    assert_eq!(saved.clients[0].perm, 0x071f1f00);
    assert!(saved.clients[0].enabled);
    assert!(f.host.pairings.lock().unwrap().sessions.is_empty());
    assert!(
        do_pair(f.host.clone(), &args, peer)
            .await
            .unwrap_err()
            .to_string()
            .contains("not available")
    );
}

#[tokio::test]
async fn otp_expiry_wrong_auth_and_pairing_disable_do_not_create_devices() {
    let f = Fixture::new();
    let mut args = otp_request(&f);
    let peer = "127.0.0.1".parse().unwrap();
    set_otp(&f, Duration::from_secs(181));
    assert!(do_pair(f.host.clone(), &args, peer).await.is_err());
    assert!(f.host.otp.lock().unwrap().is_none());
    set_otp(&f, Duration::ZERO);
    args.insert("otpauth".into(), "wrong".into());
    assert!(do_pair(f.host.clone(), &args, peer).await.is_ok());
    assert!(f.host.otp.lock().unwrap().is_some());
    assert!(f.host.paired.read().unwrap().clients.is_empty());
    f.host
        .config
        .write()
        .unwrap()
        .values
        .insert("enable_pairing".into(), "false".into());
    assert_eq!(
        do_pair(f.host.clone(), &args, peer)
            .await
            .unwrap_err()
            .to_string(),
        "pairing is disabled"
    );
    assert!(f.host.otp.lock().unwrap().is_some());
}

#[tokio::test]
async fn applist_permissions_placeholder_and_hdr_xml_match_moonlight() {
    let f = Fixture::new();
    f.client(1 << 25);
    let denied = document(request(&f, true, "GET", "/applist").await).await;
    let app = denied.get_child("App").unwrap();
    assert_eq!(field(app, "ID"), "114514");
    assert_eq!(field(app, "IsHdrSupported"), "0");
    assert_eq!(
        field(app, "AppTitle"),
        "Permission denied - enable \"List applications\" for this device in the host's Web UI"
    );
    assert_eq!(denied.children.len(), 1);
    f.host.paired.write().unwrap().clients[0].perm = 1 << 24;
    let title = "Game <&> \"日本語\"";
    f.host.apps.write().unwrap()[0].name = title.into();
    let allowed = document(request(&f, true, "GET", "/applist").await).await;
    assert_eq!(allowed.children.len(), f.host.apps.read().unwrap().len());
    let app = allowed.get_child("App").unwrap();
    assert_eq!(field(app, "AppTitle"), title);
    assert_eq!(
        field(app, "ID"),
        f.host.apps.read().unwrap()[0].id().to_string()
    );
    assert_eq!(field(app, "IsHdrSupported"), "1");
    assert!(!field(app, "UUID").is_empty());
    f.host.paired.write().unwrap().clients[0].enabled = false;
    let disabled = document(request(&f, true, "GET", "/applist").await).await;
    assert_eq!(disabled.attributes["status_code"], "401");
    assert!(disabled.children.is_empty());
}

#[tokio::test]
async fn unpair_get_and_post_require_tls_and_stop_only_the_paired_device() {
    for method in ["GET", "POST"] {
        let f = Fixture::new();
        let client = f.client(u32::MAX);
        let mine = f.session(client.clone(), Role::InputOnly);
        let mut other = client;
        other.uuid = "another-device".into();
        let theirs = f.session(other, Role::RemoteMonitor);
        let plain = document(request(&f, false, method, "/unpair").await).await;
        assert_eq!(field(&plain, "unpaired"), "0");
        assert_eq!(f.host.paired.read().unwrap().clients.len(), 1);
        assert!(!mine.stopping());
        let secure = document(request(&f, true, method, "/unpair").await).await;
        assert_eq!(field(&secure, "unpaired"), "1");
        assert!(mine.stopping());
        assert!(!theirs.stopping());
        assert!(
            butterpollo_core::state::PairedState::load(&f.host.paired_path)
                .unwrap()
                .clients
                .is_empty()
        );
        assert_eq!(
            field(
                &document(request(&f, true, method, "/unpair").await).await,
                "unpaired"
            ),
            "0"
        );
    }
}

#[tokio::test]
async fn launch_and_resume_reject_permissions_and_report_protocol_failures() {
    let f = Fixture::new();
    f.client(1 << 24);
    for endpoint in ["launch", "resume"] {
        let root = document(request(&f, true, "GET", &format!("/{endpoint}")).await).await;
        assert_eq!(root.attributes["status_code"], "401");
    }
    f.host.paired.write().unwrap().clients[0].perm = 1 << 26;
    for (endpoint, reply) in [("launch", "gamesession"), ("resume", "resume")] {
        let root = document(request(&f, true, "GET", &format!("/{endpoint}")).await).await;
        assert_eq!(root.attributes["status_code"], "503");
        assert_eq!(field(&root, reply), "0");
        assert_eq!(root.attributes["status_message"], "missing stream key");
    }
    assert!(f.host.sessions.lock().unwrap().pending.is_empty());
}

#[tokio::test]
async fn bitrate_caps_and_alias_update_only_the_authenticated_devices_sessions() {
    let f = Fixture::new();
    let client = f.client(1 << 25);
    let mine = f.session(client.clone(), Role::Stream);
    let second = f.session(client.clone(), Role::RemoteMonitor);
    let mut other = client;
    other.uuid = "other".into();
    let theirs = f.session(other, Role::Stream);
    let original = theirs.bitrate.load(Ordering::Acquire);
    for (ceiling, query, expected) in [
        ("60000", "bitrate=80000", 60000),
        ("0", "bitrate_kbps=900000", 500000),
        ("0", "bitrate=12345", 12345),
    ] {
        f.host
            .config
            .write()
            .unwrap()
            .values
            .insert("max_bitrate".into(), ceiling.into());
        let root = document(request(&f, true, "GET", &format!("/bitrate?{query}")).await).await;
        assert_eq!(root.attributes["status_code"], "200");
        assert_eq!(field(&root, "bitrate"), expected.to_string());
        assert_eq!(field(&root, "updated"), "2");
        assert_eq!(mine.bitrate.load(Ordering::Acquire), expected);
        assert_eq!(second.bitrate.load(Ordering::Acquire), expected);
        assert_eq!(theirs.bitrate.load(Ordering::Acquire), original);
    }
    for query in [
        "",
        "bitrate=0",
        "bitrate=-1",
        "bitrate=4294967296",
        "bitrate=no",
    ] {
        let root = document(request(&f, true, "GET", &format!("/bitrate?{query}")).await).await;
        assert_eq!(root.attributes["status_code"], "400");
        assert_eq!(field(&root, "bitrate"), "0");
        assert_eq!(mine.bitrate.load(Ordering::Acquire), 12345);
    }
    f.host.sessions.lock().unwrap().active.clear();
    assert_eq!(
        document(request(&f, true, "GET", "/bitrate?bitrate=1").await)
            .await
            .attributes["status_code"],
        "404"
    );
    f.host.paired.write().unwrap().clients[0].perm = 1 << 24;
    assert_eq!(
        document(request(&f, true, "GET", "/bitrate?bitrate=1").await)
            .await
            .attributes["status_code"],
        "403"
    );
}

#[tokio::test]
async fn abr_capabilities_are_client_driven_and_require_view_or_launch_permission() {
    let f = Fixture::new();
    f.client(0);
    for perm in [0, 1 << 24, 1 << 25, 1 << 26] {
        f.host.paired.write().unwrap().clients[0].perm = perm;
        let response = request(&f, true, "GET", "/api/abr/capabilities").await;
        if perm & VIEW == 0 {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        } else {
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), 1024).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                json!({"supported":false,"version":1,"features":["runtime_bitrate"]})
            );
        }
    }
}

#[tokio::test]
async fn clipboard_requires_direction_view_text_and_an_active_session_for_this_device() {
    let f = Fixture::new();
    let client = f.client((1 << 16) | (1 << 17) | (1 << 25));
    let args = Args::from([("type".into(), "text".into())]);
    let c = f.connection(true);
    assert!(clipboard_client(&f.host, &c, 1 << 17, &args).is_err());
    let mut other = client.clone();
    other.uuid = "other".into();
    f.session(other, Role::Stream);
    assert!(clipboard_client(&f.host, &c, 1 << 17, &args).is_err());
    let mine = f.session(client, Role::InputOnly);
    assert!(clipboard_client(&f.host, &c, 1 << 17, &args).is_ok());
    assert!(clipboard_client(&f.host, &c, 1 << 16, &args).is_ok());
    assert!(clipboard_client(&f.host, &c, 1 << 17, &Args::new()).is_err());
    assert!(
        clipboard_client(
            &f.host,
            &c,
            1 << 17,
            &Args::from([("type".into(), "image".into())])
        )
        .is_err()
    );
    let invalid = clipboard_write(
        State(f.host.clone()),
        Extension(c.clone()),
        Query(args.clone()),
        Bytes::from_static(&[0xff]),
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    for perm in [1 << 17, 1 << 25] {
        f.host.paired.write().unwrap().clients[0].perm = perm;
        for method in ["GET", "POST"] {
            assert_eq!(
                request(&f, true, method, "/actions/clipboard?type=text")
                    .await
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
    }
    f.host.paired.write().unwrap().clients[0].perm = (1 << 17) | (1 << 26);
    assert!(clipboard_client(&f.host, &c, 1 << 17, &args).is_ok());
    f.host
        .sessions
        .lock()
        .unwrap()
        .request_stop(Some(&mine.launch.client.uuid));
    assert!(clipboard_client(&f.host, &c, 1 << 17, &args).is_err());
}

#[test]
fn input_and_monitor_roles_resolve_without_starting_an_app_or_preparing_hardware() {
    let f = Fixture::new();
    let client = f.client(u32::MAX);
    let args = Args::new();
    let resolve = |control, owner, resume| {
        resolve_launch_app(&f.host, &args, &client, 0, Some(control), owner, resume)
    };
    assert!(resolve(Control::Input, remote::Owner::None, false).is_err());
    f.host
        .config
        .write()
        .unwrap()
        .values
        .insert("enable_input_only_mode".into(), "true".into());
    for (control, role) in [
        (Control::Input, Role::InputOnly),
        (Control::Monitor, Role::RemoteMonitor),
    ] {
        let (resolved, app, _) = resolve(control, remote::Owner::None, false).unwrap();
        assert_eq!(resolved, role);
        assert!(app.is_none());
    }
    assert_eq!(
        resolve(Control::Resume, remote::Owner::Monitor, true)
            .unwrap()
            .0,
        Role::RemoteMonitor
    );
    assert!(resolve(Control::Resume, remote::Owner::None, true).is_err());
    let session = f.session(client.clone(), Role::InputOnly);
    assert!(remote_owner(&f.host, &client.uuid) == remote::Owner::Input);
    assert!(validate_launch_request(&f.host, &client, Some(Control::Monitor)).is_break());
    assert!(validate_launch_request(&f.host, &client, Some(Control::Input)).is_break());
    assert_eq!(session.launch.role, Role::InputOnly);
    assert!(f.host.current_app.lock().unwrap().is_none());
    assert!(session.launch.preparation.lock().unwrap().is_none());
}
