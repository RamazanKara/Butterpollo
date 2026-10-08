use anyhow::Result;
use butterpollo_core::{auth, config::Config, crypto::Identity, state};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub fn rc23_profile(profile: &Path) -> Result<()> {
    for folder in [
        "credentials",
        "covers/custom",
        "logs",
        "extensions",
        "updates/downloads",
    ] {
        std::fs::create_dir_all(profile.join(folder))?;
    }
    let settings = "# rc.23 profile with comments, Unicode and custom settings\r\n\
        sunshine_name = \"Çağrı Müller # Gaming\"\r\n\
        port = 48001\r\n\
        bind_address = 127.0.0.1\r\n\
        encoder = amf\r\n\
        amd_quality = quality\r\n\
        gamepad = vhf_ds4\r\n\
        virtual_display_mode = per_client\r\n\
        virtual_display_layout = extended_primary\r\n\
        audio_sink = speakers\r\n\
        frame_limiter_enable = true\r\n\
        frame_limiter_fps_limit = 117\r\n\
        resolutions = [1920x1080, 2560x1440, 3840x2160]\r\n\
        fps = [60, 90, 120]\r\n\
        keybindings = [\r\n  0x10, 0xa0,\r\n  0x11, 0xa2\r\n]\r\n\
        future_setting = {\"nested\":{\"keep\":[1,2,3]}}\r\n";
    let mut conf = vec![0xff, 0xfe];
    conf.extend(settings.encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(profile.join("sunshine.conf"), conf)?;
    let identity = Identity::generate()?;
    std::fs::write(
        profile.join("credentials/cacert.pem"),
        &identity.certificate,
    )?;
    std::fs::write(profile.join("credentials/cakey.pem"), &identity.private_pem)?;
    let mut clients = Vec::new();
    for (index, (name, layout)) in [
        ("Living room TV", "extended"),
        ("Steam Deck", "extended_primary_isolated"),
        ("Çağrı's tablet", "exclusive"),
    ]
    .into_iter()
    .enumerate()
    {
        let identity = Identity::generate()?;
        clients.push(json!({
            "name":name,"cert":identity.certificate,
            "uuid":format!("10000000-0000-4000-8000-{index:012}"),
            "perm":119480064 - index as u32,"enabled":index != 2,
            "virtual_display_layout":layout,"display_mode":"2560x1440x120",
            "virtual_display_guid":format!("20000000-0000-4000-8000-{index:012}"),
            "allow_client_commands":false,
            "do":[{"cmd":"before-client.cmd","elevated":false}],
            "undo":[{"cmd":"after-client.cmd","elevated":false}],
            "config_overrides":{"gamepad":"vhf_ds4","amd_quality":"speed","max_bitrate":"45000"},
            "future_device":{"nested":[1,true,{"keep":"device extras"}]}
        }));
    }
    let credentials = state::Credentials::new("admin".into(), "rc23-test-password")?;
    let mut paired = serde_json::to_value(&credentials)?;
    paired["root"] = json!({
        "uniqueid":"30000000-0000-4000-8000-000000000001",
        "named_devices":clients,"future_pairing":{"keep":true}
    });
    paired["future_credentials"] = json!({"keep":true});
    state::write_json(&profile.join("sunshine_state.json"), &paired)?;
    state::write_json(&profile.join("sunshine_credentials.json"), &credentials)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let aliases = json!({"root":{
        "shared_virtual_display_guid":"40000000-0000-4000-8000-000000000001",
        "app_id_aliases":{"game-uuid":{"current_id":"1234","cover_fingerprint":"default","aliases":["2345","3456"]}},
        "api_tokens":[
            {"hash":butterpollo_core::crypto::legacy_hash(b"rc23-apps-token"),"username":"admin","created_at":now - 86400,
                "scopes":[{"path":"/api/apps","methods":["GET"]}],"future_token":{"keep":true}},
            {"hash":butterpollo_core::crypto::legacy_hash(b"rc23-config-token"),"username":"admin","created_at":now - 3600,
                "scopes":[{"path":"/api/config","methods":["GET","POST"]}]}
        ],
        "session_tokens":[
            {"hash":"ab".repeat(32),"refresh_token_hash":"cd".repeat(32),"username":"admin",
                "created_at":now - 3600,"last_seen":now - 10,"expires_at":now + 3600,
                "refresh_expires_at":now + 86400,"remember_me":true,"user_agent":"Firefox on Windows",
                "remote_address":"192.168.1.23","future_session":{"keep":true}},
            {"hash":"ef".repeat(32),"username":"admin","expires_at":now + 1800,"remember_me":false}
        ],
        "future_state":{"keep":[1,2,3]}
    },"future_document":true});
    state::write_json(&profile.join("vibeshine_state.json"), &aliases)?;
    std::fs::write(
        profile.join("covers/desktop.png"),
        include_bytes!("../../assets/package/desktop.png"),
    )?;
    std::fs::write(
        profile.join("covers/custom/Épopée.png"),
        include_bytes!("../../assets/package/steam.png"),
    )?;
    let apps = json!({"env":{"CUSTOM":"kept","PATH":"$(PATH);C:\\Games"},"future_library":{"keep":true},"apps":[
        {"name":"Desktop","uuid":"desktop-uuid","cmd":"","image-path":"covers/desktop.png",
            "allow-client-commands":false,"config-overrides":{"virtual_display_layout":"extended","gamepad":"vhf_xbox_one"}},
        {"name":"Épopée","uuid":"game-uuid","cmd":"\"C:\\Games\\Épopée\\game.exe\" --fullscreen",
            "working-dir":"C:\\Games\\Épopée","image-path":profile.join("covers/custom/Épopée.png"),
            "prep-cmd":[{"do":"before-app.cmd","undo":"after-app.cmd","elevated":true}],
            "detached":["helper.exe"],"auto-detach":false,"wait-all":true,
            "config-overrides":{"amd_quality":"speed","frame_limiter_fps_limit":"90","audio_sink":"headphones"},
            "future_app":{"keep":[true,"custom"]}},
        {"name":"Steam","uuid":"steam-uuid","cmd":"steam://open/bigpicture","image-path":"covers/custom/Épopée.png",
            "prep-cmd":[{"do":"","undo":"steam://close/bigpicture","elevated":false}],
            "gamepad":"vhf_ds4","config-overrides":{"max_bitrate":"60000"}}
    ]});
    state::write_json(&profile.join("apps.json"), &apps)?;
    for (name, document) in [
        (
            "display-recovery.json",
            json!({"pid":1234,"started":123456789,"entries":{"device-1":{
            "output":"\\\\.\\DISPLAY1","mode":[[1920,1080,60],[2560,1440,120]],"hdr":[false,true]}}}),
        ),
        (
            "frame-limiter-recovery.json",
            json!({"version":1,"rtss":{"root":"C:\\RTSS",
            "before":{"FramerateLimit":0},"applied":{"FramerateLimit":117},"disabled":false},"nvidia":[]}),
        ),
        (
            "audio-recovery.json",
            json!({"before":["speakers","speakers","headset"],"applied":"virtual-sink","format":null}),
        ),
        (
            "display-state.json",
            json!({"golden":{"device":"device-1","width":1920,"height":1080}}),
        ),
        (
            "extensions/future.json",
            json!({"unknown":[1,{"keep":true}]}),
        ),
        (
            "update-result.json",
            json!({"version":"2.0.0-rc.23","phase":"installed","error":null}),
        ),
    ] {
        state::write_json(&profile.join(name), &document)?;
    }
    for (name, bytes) in [
        ("butterpollo.log", b"rc.23 host log\r\n".as_slice()),
        ("logs/session-rc23.log", b"previous streaming session\r\n"),
        (
            "sunshine.conf.bak",
            b"# user backup\r\nencoder = software\r\n",
        ),
        ("updates/downloads/previous-setup.exe", b"cached installer"),
        ("update.lock", b""),
    ] {
        std::fs::write(profile.join(name), bytes)?;
    }
    assert_usable(profile)
}

pub fn snapshot(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = PathBuf::from(entry.file_name());
        if entry.file_type()?.is_dir() {
            for (relative, bytes) in snapshot(&entry.path())? {
                files.insert(name.join(relative), bytes);
            }
        } else {
            files.insert(name, std::fs::read(entry.path())?);
        }
    }
    Ok(files)
}

pub fn package(root: &Path, version: &str, extra: &str) -> Result<Vec<crate::payload::Entry>> {
    std::fs::create_dir_all(root.join("assets/web"))?;
    let mut entries = Vec::new();
    for name in [
        "butterpollo.exe",
        "butterpollo-service.exe",
        "Start Butterpollo.exe",
        "assets/web/index.html",
        extra,
    ] {
        let bytes = format!("{version}: {name}");
        std::fs::write(root.join(name), &bytes)?;
        entries
            .push(json!({"path":name,"sha256":format!("{:x}", Sha256::digest(bytes.as_bytes()))}));
    }
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&entries)?,
    )?;
    crate::payload::verify(root)
}

pub fn assert_usable(profile: &Path) -> Result<()> {
    let config = Config::load(&profile.join("sunshine.conf"))?;
    let files = state::ProfileFiles::new(&config, profile);
    let paired = state::PairedState::load(&files.paired)?;
    assert_eq!(paired.unique_id, "30000000-0000-4000-8000-000000000001");
    assert_eq!(paired.clients.len(), 3);
    assert_eq!(
        serde_json::to_value(&paired.clients)?,
        paired.document["root"]["named_devices"]
    );
    assert!(Identity::read(&files.certificate, &files.key)?.is_some());
    assert!(
        state::Credentials::load(&files.credentials)?
            .unwrap()
            .verifies("admin", "rc23-test-password")
    );
    let aliases = state::load_json(&files.aliases, Value::Null)?;
    let tokens = auth::read(&aliases)?;
    assert_eq!(tokens.len(), 2);
    assert!(auth::permits(
        &tokens,
        "rc23-apps-token",
        "admin",
        "/api/apps",
        "GET"
    ));
    assert!(auth::permits(
        &tokens,
        "rc23-config-token",
        "admin",
        "/api/config",
        "POST"
    ));
    assert!(!auth::permits(
        &tokens,
        "rc23-apps-token",
        "admin",
        "/api/config",
        "POST"
    ));
    assert_eq!(
        aliases["root"]["session_tokens"].as_array().unwrap().len(),
        2
    );
    let library = state::load_json(&files.apps, Value::Null)?;
    let apps: Vec<state::App> = serde_json::from_value(library["apps"].clone())?;
    assert_eq!(apps.len(), 3);
    assert_eq!(
        apps[1].extra["config-overrides"]["frame_limiter_fps_limit"],
        "90"
    );
    Ok(())
}
