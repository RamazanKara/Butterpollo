use super::*;
use crate::state::{PendingPin, test_support::Fixture};
use axum::body::{Body, to_bytes};
use butterpollo_core::{session::Role, state::Credentials};
use tower::ServiceExt;

async fn request(f: &Fixture, method: &str, path: &str, data: Value) -> Response {
    *f.host.credentials.write().unwrap() =
        Some(Credentials::new("parity".into(), "parity-test").unwrap());
    router(f.host.clone())
        .layer(Extension(f.connection(true)))
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, "Basic cGFyaXR5OnBhcml0eS10ZXN0")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&data).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}
async fn value(response: Response, status: StatusCode) -> Value {
    assert_eq!(response.status(), status);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn device_updates_persist_permissions_overrides_and_revoke_only_that_devices_sessions() {
    let f = Fixture::new();
    let client = f.client(0x071f1f00);
    let mine = f.session(client.clone(), Role::Stream);
    let mut other = client.clone();
    other.uuid = "other".into();
    let theirs = f.session(other, Role::InputOnly);
    let body = json!({"uuid":client.uuid,"name":"Renamed","cert":"cannot replace the certificate","perm":u32::MAX,
        "config_overrides":{"max_bitrate":20000},"display_mode":"2560x1600x120","custom":"preserved"});
    let updated = value(
        request(&f, "POST", "/api/clients/update", body).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(updated["disconnected"], false);
    assert!(!mine.stopping());
    let listed = value(
        request(&f, "GET", "/api/clients/list", json!({})).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(listed["clients"], listed["named_certs"]);
    let device = &listed["clients"][0];
    assert_eq!(device["perm"], 0x071f1f00);
    assert_eq!(device["name"], "Renamed");
    assert_eq!(device["connected"], true);
    assert!(device.get("cert").is_none());
    assert_eq!(device["config_overrides"]["max_bitrate"], 20000);
    for body in [
        json!({"uuid":client.uuid,"perm":1<<25}),
        json!({"uuid":client.uuid,"enabled":false,"display_mode":null}),
    ] {
        let updated = value(
            request(&f, "POST", "/api/clients/update", body).await,
            StatusCode::OK,
        )
        .await;
        assert_eq!(updated["disconnected"], true);
        assert!(mine.stopping());
        assert!(!theirs.stopping());
    }
    let saved = butterpollo_core::state::PairedState::load(&f.host.paired_path).unwrap();
    assert!(!saved.clients[0].enabled);
    assert_eq!(saved.clients[0].perm, 1 << 25);
    assert_eq!(saved.clients[0].cert, client.cert);
    assert_eq!(saved.clients[0].extra["custom"], "preserved");
    assert!(!saved.clients[0].extra.contains_key("display_mode"));
    assert_eq!(
        saved.clients[0].extra["config_overrides"]["max_bitrate"],
        20000
    );
}

#[tokio::test]
async fn pending_pairing_requests_expire_and_pin_submission_selects_one_device() {
    let f = Fixture::new();
    let mut receivers = Vec::new();
    for (id, age) in [("first", 0), ("second", 0), ("expired", 301)] {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        receivers.push(receiver);
        f.host.pins.lock().unwrap().insert(
            id.into(),
            PendingPin {
                name: id.into(),
                certificate: "secret".into(),
                created: Instant::now() - Duration::from_secs(age),
                sender,
            },
        );
    }
    let pending = value(
        request(&f, "GET", "/api/clients/pending", json!({})).await,
        StatusCode::OK,
    )
    .await;
    let ids: Vec<_> = pending["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["uniqueid"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["first", "second"]);
    assert!(!pending.to_string().contains("secret"));
    for body in [
        json!({"pin":"1234"}),
        json!({"pin":"12x4","uniqueid":"first"}),
        json!({"pin":"123","uniqueid":"first"}),
    ] {
        value(
            request(&f, "POST", "/api/pin", body).await,
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert_eq!(f.host.pins.lock().unwrap().len(), 3);
    }
    value(
        request(
            &f,
            "POST",
            "/api/pin",
            json!({"pin":"1234","uniqueid":"first","name":"Living room"}),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        receivers[0].try_recv().unwrap(),
        ("1234".into(), "Living room".into())
    );
    assert!(receivers[1].try_recv().is_err());
    assert!(!f.host.pins.lock().unwrap().contains_key("first"));
}

#[tokio::test]
async fn otp_api_validates_passphrases_and_replaces_the_previous_pin() {
    let f = Fixture::new();
    value(
        request(&f, "POST", "/api/otp", json!({"passphrase":"abc"})).await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert!(f.host.otp.lock().unwrap().is_none());
    for phrase in ["abcd", "日本語文"] {
        let response = value(
            request(
                &f,
                "POST",
                "/api/otp",
                json!({"passphrase":phrase,"deviceName":"Phone"}),
            )
            .await,
            StatusCode::OK,
        )
        .await;
        let pin = response["otp"].as_str().unwrap();
        assert_eq!(pin.len(), 4);
        assert!(pin.bytes().all(|c| c.is_ascii_digit()));
        let otp = f.host.otp.lock().unwrap();
        let otp = otp.as_ref().unwrap();
        assert_eq!(otp.pin, pin);
        assert_eq!(otp.passphrase, phrase);
        assert_eq!(otp.device_name, "Phone");
    }
}

#[tokio::test]
async fn log_tail_tracks_offsets_appends_rotation_and_a_configured_path() {
    let f = Fixture::new();
    f.host
        .config
        .write()
        .unwrap()
        .values
        .insert("log_path".into(), "custom.log".into());
    let path = f.host.directory.join("custom.log");
    let missing = value(
        request(&f, "GET", "/api/logs/tail?offset=0", json!({})).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        missing,
        json!({"status":true,"offset":0,"size":0,"reset":false,"text":""})
    );
    let first = format!("{}\n", "a".repeat(699));
    let second = format!("{}\n", "b".repeat(699));
    std::fs::write(&path, format!("{first}{second}")).unwrap();
    assert_eq!(crate::maintenance::log_path(&f.host), path);
    let chunk = value(
        request(&f, "GET", "/api/logs/tail?offset=0&max=1024", json!({})).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(chunk["text"], first);
    assert_eq!(chunk["offset"], 700);
    assert_eq!(chunk["reset"], false);
    let chunk = value(
        request(&f, "GET", "/api/logs/tail?offset=700&max=1024", json!({})).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(chunk["text"], second);
    assert_eq!(chunk["offset"], 1400);
    std::fs::write(&path, format!("{first}{second}new 日本語\n")).unwrap();
    let chunk = value(
        request(&f, "GET", "/api/logs/tail?offset=1400", json!({})).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(chunk["text"], "new 日本語\n");
    std::fs::write(&path, "rotated\n").unwrap();
    let chunk = value(
        request(&f, "GET", "/api/logs/tail?offset=1400", json!({})).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(chunk["reset"], true);
    assert_eq!(chunk["offset"], 8);
    assert_eq!(chunk["text"], "rotated\n");
}

#[tokio::test]
async fn browse_and_app_icon_routes_return_the_local_library_assets() {
    let f = Fixture::new();
    let icons = f.host.directory.join("covers");
    std::fs::create_dir_all(&icons).unwrap();
    let png = b"\x89PNG\r\n\x1a\nfixture";
    let icon = icons.join("playnite_fixture.png");
    std::fs::write(&icon, png).unwrap();
    let app = json!({"name":"Playnite game","uuid":"icon-app","playnite-id":"fixture","playnite-icon-path":icon.to_string_lossy()});
    value(request(&f, "POST", "/api/apps", app).await, StatusCode::OK).await;
    let response = request(&f, "GET", "/api/apps/icon-app/icon", json!({})).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
        png
    );
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("path", &icon.to_string_lossy())
        .append_pair("type", "file")
        .finish();
    let listing = value(
        request(&f, "GET", &format!("/api/browse?{query}"), json!({})).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(listing["path"], icons.to_string_lossy().as_ref());
    assert!(
        listing["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "playnite_fixture.png")
    );
}
