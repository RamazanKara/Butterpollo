use super::*;
use serde_json::json;

fn card(title: &str, content: &str) -> String {
    format!(
        "<section class=\"card\"><h2>{}</h2>{content}</section>",
        i18n::message(title)
    )
}
fn rows<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}
fn raw(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}
fn timestamp(value: &Value) -> String {
    let seconds = value.as_i64().or_else(|| value.as_str()?.parse().ok());
    if let Some(date) =
        seconds.and_then(|seconds| time::OffsetDateTime::from_unix_timestamp(seconds).ok())
    {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02} UTC",
            date.year(),
            date.month() as u8,
            date.day(),
            date.hour(),
            date.minute()
        )
    } else {
        value.as_str().unwrap_or("Not available").to_owned()
    }
}
fn scope_list(scopes: &[Value]) -> String {
    let mut html = "<ul class=\"permissions\">".to_owned();
    for scope in scopes {
        let methods = rows(scope, "methods")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let path = text(scope, "path")
            .replace("[^/]+", "{id}")
            .replace("[0-9]+", "{number}");
        html += &format!(
            "<li><code>{}</code> <span>{}</span></li>",
            i18n::data(&path),
            i18n::data(&methods)
        );
    }
    html + "</ul>"
}
fn readiness(meta: &Value) -> String {
    let video = meta["encoder_status"]["h264"] == true;
    let checking = meta["encoder_status"]["state"] == "checking";
    let displays = rows(&meta["capture_status"], "displays");
    let virtual_requested = meta["capture_status"]["virtual_display_configured"] == true;
    let display_ready = if virtual_requested {
        meta["virtual_display"]["capable"] == true
    } else {
        !displays.is_empty()
    };
    let audio_ready = !rows(meta, "audio_sinks").is_empty();
    let mut out = String::new();
    for warning in rows(meta, "warnings") {
        out += &format!(
            "<p class=\"notice\" role=\"status\">{}</p>",
            i18n::data(text(warning, "message"))
        );
    }
    out += "<ul class=\"readiness\">";
    for (ready, label, detail) in [
        (
            video,
            "Video",
            if video {
                "Ready to stream. Choose a codec in Moonlight."
            } else if checking {
                "Checking your graphics card. Reload in a few seconds."
            } else {
                "No working encoder found. Retrying automatically."
            },
        ),
        (
            display_ready,
            "Display",
            if display_ready {
                "Ready to capture."
            } else if virtual_requested {
                "Virtual display is unavailable. Start the installed Rubylight service or select your physical display in Settings."
            } else {
                "No active display found. Turn on a monitor or set up a virtual display in Settings."
            },
        ),
        (
            audio_ready || meta["audio_enabled"] == false,
            "Audio",
            if meta["audio_enabled"] == false {
                "Audio streaming is switched off in Settings."
            } else if audio_ready {
                "An audio output is available. Choose the output used by your game in Settings."
            } else {
                "No active audio output found. Connect headphones or speakers, then check Audio in Settings."
            },
        ),
    ] {
        out += &format!(
            "<li><span class=\"badge{}\">{}</span><div><strong>{label}</strong><p>{detail}</p></div></li>",
            if ready { " ready" } else { "" },
            if ready {
                "Ready"
            } else if label == "Video" && checking {
                "Checking"
            } else {
                "Needs attention"
            }
        );
    }
    out += "</ul>";
    for (label, detail) in [
        ("Virtual display", text(&meta["virtual_display"], "reason")),
        ("Audio", text(meta, "audio_error")),
        ("Screen capture", text(&meta["capture_status"], "error")),
    ] {
        if !detail.is_empty() {
            out += &format!(
                "<p class=\"notice\"><strong>{label}:</strong> {}</p>",
                i18n::data(detail)
            );
        }
    }
    card("Connection checklist", &out)
}
fn credentials(csrf: &str, setup: bool) -> String {
    let mut content = if setup {
        "<p>Create your administrator account to pair devices and manage this host.</p>".into()
    } else {
        field("currentUsername", "Current username", "", "text")
            + &field("currentPassword", "Current password", "", "password")
    };
    content += &field("newUsername", "Username", "", "text");
    content += &field(
        "newPassword",
        "New password (at least 8 characters)",
        "",
        "password",
    );
    content += &field("confirmNewPassword", "Confirm password", "", "password");
    content += if setup {
        "<button>Create account</button>"
    } else {
        "<button>Change password</button>"
    };
    card(
        if setup {
            "Set up your account"
        } else {
            "Change password"
        },
        &form(
            if setup { "setup" } else { "password" },
            csrf,
            if setup { "/setup" } else { "/settings" },
            &content,
        ),
    )
}
pub(super) async fn render(
    h: &Shared,
    connection: &Connection,
    headers: &HeaderMap,
    path: &str,
    csrf: &str,
    query: &Fields,
) -> Result<String, String> {
    match path {
        "/login" => Ok(login(csrf)),
        "/setup" => Ok(setup(csrf)),
        "/" => overview(h, connection, headers, csrf, query).await,
        "/library" => apps(h, connection, headers, csrf, query).await,
        "/devices" => devices(h, connection, headers, csrf).await,
        "/settings" => settings(h, connection, headers, csrf).await,
        "/logs" => logs(h, connection, headers).await,
        "/integrations" => integrations(h, connection, headers).await,
        "/api-tokens" => api_tokens(h, connection, headers, csrf).await,
        "/maintenance" => maintenance(h, connection, headers, csrf).await,
        _ => Err("Page not found".into()),
    }
}

fn login(csrf: &str) -> String {
    card(
        "Sign in",
        &form(
            "login",
            csrf,
            "/login",
            &(field("username", "Username", "", "text")
                + &field("password", "Password", "", "password")
                + "<label><input type=\"checkbox\" name=\"remember_me\" value=\"true\"> Keep me signed in on this browser</label>"
                + "<button>Sign in</button>"),
        ),
    )
}

fn setup(csrf: &str) -> String {
    credentials(csrf, true)
}

async fn overview(
    h: &Shared,
    connection: &Connection,
    headers: &HeaderMap,
    csrf: &str,
    query: &Fields,
) -> Result<String, String> {
    let meta = get(h, connection, headers, "/api/metadata").await?;
    let sessions = get(h, connection, headers, "/api/rtsp/sessions").await?;
    let active = rows(&sessions, "sessions");
    let mut content = format!(
        "<p class=\"intro\">Manage your library, pair devices, and tune your stream.</p><div class=\"metrics\"><section class=\"card\"><span>Host</span><strong>{}</strong><small>Version {}</small></section><section class=\"card\"><span>Active streams</span><strong>{}</strong><small>Connected right now</small></section><section class=\"card\"><span>Encoder</span><strong>{}</strong><small>Capture: {}</small></section></div>",
        i18n::data(text(&meta, "host_name")),
        i18n::data(text(&meta, "version")),
        active.len(),
        match text(&meta["encoder_status"], "state") {
            "ready" => "Ready",
            "checking" => "Checking",
            "failed" => "Unavailable",
            _ => "Not available",
        },
        match text(&meta["capture_status"], "configured_backend") {
            "wgc" => "Windows Graphics Capture",
            "dxgi" | "ddx" => "Desktop Duplication",
            "auto" | "" => "Automatic",
            _ => "Configured capture method",
        }
    );
    let mut badges = String::new();
    for (key, name) in [
        ("h264", "H.264"),
        ("hevc", "HEVC"),
        ("av1", "AV1"),
        ("pyrowave", "PyroWave"),
    ] {
        badges += &format!(
            "<span class=\"badge{}\">{} · {}</span>",
            if meta["encoder_status"][key] == true {
                " ready"
            } else {
                ""
            },
            name,
            if meta["encoder_status"][key] == true {
                "Ready"
            } else if meta["encoder_status"]["state"] == "checking" {
                "Checking"
            } else {
                "Unavailable"
            }
        );
    }
    content += &card(
        "Streaming capabilities",
        &format!(
            "<div class=\"badges\">{badges}</div><p>H.264, HEVC and AV1 work with Moonlight. PyroWave works with <a href=\"https://github.com/RamazanKara/rubylight-android\" target=\"_blank\" rel=\"noopener noreferrer\">Rubylight Android</a>, our own client, and on a PC with <a href=\"https://github.com/Nonary/moonlight-qt\" target=\"_blank\" rel=\"noopener noreferrer\">Nonary’s version of Moonlight</a>, which VRR also requires. PyroWave is best suited to a fast local network.</p>"
        ),
    );
    content += &readiness(&meta);
    if meta["paired_devices"].as_u64().unwrap_or(0) == 0 {
        content += &card(
            "Your first stream",
            &format!(
                "<ol class=\"setup-steps\"><li><strong>Open Moonlight on your other device.</strong> Keep both devices on the same network. Add <code>{}</code> if this PC does not appear automatically.</li><li><strong>Enter the PIN shown by Moonlight.</strong> Open <a href=\"/devices\">Devices</a>, reload to see its pairing request, and enter that PIN.</li><li><strong>Open Desktop in Moonlight.</strong> Start with 1080p at 60 fps, then choose your preferred resolution, frame rate and HDR. Add games in <a href=\"/library\">Library</a>.</li></ol>",
                i18n::data(text(&meta, "pc_address"))
            ),
        );
        let addresses = rows(&meta, "pc_addresses");
        if addresses.len() > 1 {
            content += &format!(
                "<p>Other active network addresses: {}. Use the address on the same network as your Moonlight device.</p>",
                addresses
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|address| *address != text(&meta, "pc_address"))
                    .map(|address| format!("<code>{}</code>", i18n::data(address)))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        content += "<p class=\"muted\">If Moonlight cannot connect, allow Butterpollo.exe through Windows Firewall for your private network, then check for a pairing request in Devices.</p>";
    }
    content += &card(
        "Get started",
        "<div class=\"quick-links\"><a href=\"/devices\"><strong>Pair a device</strong><span>Connect Moonlight to this host</span></a><a href=\"/library\"><strong>Your library</strong><span>Manage apps and desktop streaming</span></a><a href=\"/settings\"><strong>Stream settings</strong><span>Choose your encoder, display, and audio</span></a></div>",
    );
    if active.is_empty() {
        content += &card(
            "Connections",
            "<p>No devices are streaming. Connect from Moonlight to start.</p>",
        );
    } else {
        let mut table = "<div class=\"table-scroll\"><table><thead><tr><th>Device</th><th>Video</th><th>Recent rate</th><th>Encoding time (95th percentile)</th><th></th></tr></thead><tbody>".to_owned();
        for session in active {
            let disconnect = button(
                "disconnect",
                csrf,
                "/",
                "uuid",
                text(session, "uuid"),
                "Disconnect",
            );
            if session["role"] == "input_only" {
                table += &format!(
                    "<tr><td data-label=\"Device\"><span>{}</span></td><td data-label=\"Connection\"><span>Remote input</span></td><td data-label=\"Status\" colspan=\"2\"><span>Input connection active</span></td><td class=\"table-actions\">{disconnect}</td></tr>",
                    i18n::data(text(session, "device_name")),
                );
                continue;
            }
            table += &format!(
                "<tr><td data-label=\"Device\"><span>{}</span></td><td data-label=\"Video\"><span>{}×{} · {} fps{}{}{}</span></td><td data-label=\"Recent rate\"><span>{:.1} fps<br>{:.1} Mbps</span></td><td data-label=\"Encoding time\"><span>{:.2} ms</span></td><td class=\"table-actions\">{disconnect}</td></tr>",
                i18n::data(text(session, "device_name")),
                session["width"],
                session["height"],
                session["fps"],
                if session["hdr"] == true {
                    " · HDR"
                } else {
                    ""
                },
                if session["vrr"] == true {
                    " · VRR"
                } else {
                    ""
                },
                if session["role"] == "remote_monitor" {
                    "<br>Remote monitor"
                } else {
                    ""
                },
                session["performance"]["fps"].as_f64().unwrap_or(0.),
                session["performance"]["bitrate_mbps"]
                    .as_f64()
                    .unwrap_or(0.),
                session["performance"]["encode_p95_ms"]
                    .as_f64()
                    .unwrap_or(0.),
            );
        }
        content += &card("Connections", &(table + "</tbody></table></div>"));
        for session in active {
            let skipped = session["frames_replaced"].as_u64().unwrap_or(0);
            if skipped > 0 {
                content += &format!(
                    "<p>{}: {skipped} older frames skipped to keep the stream current. If this keeps increasing, lower the PyroWave bitrate or check your wired connection.</p>",
                    i18n::data(text(session, "device_name"))
                );
            }
        }
        content += "<p class=\"muted\">Rates cover the last two seconds. An unchanged desktop may send fewer frames. Encoding time covers the host encoder; 95% of frames are encoded faster than the value shown. Decoding on the device and network delay add to it.</p>";
        for session in active {
            let history = rows(&session["performance"], "history");
            if history.is_empty() {
                continue;
            }
            let mut table = "<details><summary>Recent performance history</summary><div class=\"table-scroll\"><table><thead><tr><th>Sample</th><th>Frame rate (fps)</th><th>Mbps</th><th>Average encoding time</th><th>Slowest encoding time</th></tr></thead><tbody>".to_owned();
            for (i, sample) in history.iter().enumerate().rev().take(30) {
                table += &format!(
                    "<tr><td data-label=\"Sample\"><span>{}</span></td><td data-label=\"Frame rate\"><span>{:.1} fps</span></td><td data-label=\"Data rate\"><span>{:.1} Mbps</span></td><td data-label=\"Average encoding\"><span>{:.2} ms</span></td><td data-label=\"Slowest encoding\"><span>{:.2} ms</span></td></tr>",
                    i + 1,
                    sample["fps"].as_f64().unwrap_or(0.),
                    sample["bitrate_mbps"].as_f64().unwrap_or(0.),
                    sample["encode_mean_ms"].as_f64().unwrap_or(0.),
                    sample["encode_max_ms"].as_f64().unwrap_or(0.)
                );
            }
            table += "</tbody></table></div><p>The latest 30 completed intervals are shown. Each sample covers a completed measurement interval.</p></details>";
            content += &card(text(session, "device_name"), &table);
        }
    }
    Ok(content
        + if query.get("live").is_some_and(|v| v == "1") {
            "<p class=\"muted\">Updating every five seconds. <a href=\"/\">Pause updates</a></p>"
        } else {
            "<p class=\"muted\"><a href=\"/?live=1\">Update every five seconds</a></p>"
        })
}

async fn apps(
    h: &Shared,
    connection: &Connection,
    headers: &HeaderMap,
    csrf: &str,
    query: &Fields,
) -> Result<String, String> {
    let document = get(h, connection, headers, "/api/apps").await?;
    let apps = rows(&document, "apps");
    let mut content = "<p class=\"intro\">Launch your desktop or an app from any paired device.</p><div class=\"library-grid\">".to_owned();
    if apps.is_empty() {
        content += "<p>No apps yet. Add a game or program below, or leave its command empty to stream the desktop.</p>";
    }
    for app in apps {
        let uuid = text(app, "uuid");
        content += &format!(
            "<section class=\"card app\"><h2>{}</h2><p>{}</p><div class=\"actions\"><a class=\"button secondary\" href=\"/library?edit={}\">Edit</a>{}{}{}{}</div></section>",
            i18n::data(text(app, "name")),
            if text(app, "cmd").is_empty() {
                "Desktop streaming".into()
            } else {
                i18n::data(text(app, "cmd"))
            },
            encoded(uuid),
            button("app-launch", csrf, "/library", "uuid", uuid, "Launch"),
            button("app-delete", csrf, "/library", "uuid", uuid, "Remove"),
            form(
                "app-move",
                csrf,
                "/library",
                &(hidden("uuid", uuid)
                    + &hidden("direction", "up")
                    + "<button class=\"secondary\">Move up</button>")
            ),
            form(
                "app-move",
                csrf,
                "/library",
                &(hidden("uuid", uuid)
                    + &hidden("direction", "down")
                    + "<button class=\"secondary\">Move down</button>")
            )
        );
    }
    content += "</div>";
    let edit = query
        .get("edit")
        .and_then(|uuid| apps.iter().find(|a| text(a, "uuid") == uuid))
        .cloned()
        .unwrap_or(json!({"name":"","cmd":"","working-dir":""}));
    let mut advanced = edit.clone();
    let mut settings = edit.clone();
    for key in crate::stream::RTX_KEYS {
        if let Some(value) = edit.get("config-overrides").and_then(|v| v.get(*key)) {
            settings[key.replace('_', "-")] = value.clone();
        }
    }
    for key in ["name", "cmd", "working-dir", "uuid"] {
        advanced.as_object_mut().unwrap().remove(key);
    }
    let editor = hidden("uuid", text(&edit, "uuid"))
        + &field("name", "App name", text(&edit, "name"), "text")
        + &field(
            "cmd",
            "Command (leave empty for desktop)",
            text(&edit, "cmd"),
            "text",
        )
        + &field(
            "working-dir",
            "Working directory",
            text(&edit, "working-dir"),
            "text",
        )
        + &super::settings::render("app_", super::settings::APP, &settings)
        + &format!(
            "<details><summary>Preparation commands and advanced options</summary><p>Edit these options as JSON. Use double quotes around text and matching brackets. Other saved options are kept when you save.</p>{}</details>",
            area("advanced", "App options (JSON)", &raw(&advanced))
        )
        + "<button>Save app</button> <a href=\"/library\">Cancel</a>";
    content += &card(
        if text(&edit, "uuid").is_empty() {
            "Add app"
        } else {
            "Edit app"
        },
        &form("app-save", csrf, "/library", &editor),
    );
    if !text(&edit, "uuid").is_empty() {
        let mut overrides = edit
            .get("config-overrides")
            .and_then(Value::as_object)
            .map(|values| {
                values
                    .iter()
                    .filter(|(key, _)| crate::stream::RTX_KEYS.contains(&key.as_str()))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<serde_json::Map<_, _>>()
            })
            .unwrap_or_default();
        for &key in crate::stream::RTX_KEYS {
            if let Some(value) = edit
                .get(key.replace('_', "-"))
                .filter(|v| !v.is_null() && v.as_str() != Some(""))
            {
                overrides.entry(key).or_insert_with(|| value.clone());
            }
        }
        content += &card(
            "Live RTX HDR settings",
            &form(
                "app-live",
                csrf,
                "/library",
                &(hidden("uuid", text(&edit, "uuid"))
                    + "<p>Apply brightness and color settings to this running app without saving them. An empty object restores inherited settings.</p>"
                    + &area(
                        "overrides",
                        "RTX HDR settings (JSON)",
                        &raw(&Value::Object(overrides)),
                    )
                    + "<button>Apply to running app</button>"),
            ),
        );
    }
    Ok(content + &button("app-close", csrf, "/library", "", "", "Close running app"))
}

async fn devices(
    h: &Shared,
    connection: &Connection,
    headers: &HeaderMap,
    csrf: &str,
) -> Result<String, String> {
    let clients = get(h, connection, headers, "/api/clients/list").await?;
    let layout = get(h, connection, headers, "/api/clients/display-layout").await?;
    let pending: Vec<_> = h
        .pins
        .lock()
        .unwrap()
        .iter()
        .map(|(id, pin)| (id.clone(), pin.name.clone()))
        .collect();
    let mut content =
        "<p class=\"intro\">Pair Moonlight devices and choose what each device can do.</p>"
            .to_owned();
    if pending.is_empty() {
        content += &card(
            "Pair a device",
            "<p>Add this host in Moonlight. Moonlight shows a four-digit PIN. Reload this page to see the pairing request, then enter that PIN here.</p><a class=\"button secondary\" href=\"/devices\">Check for a pairing request</a>",
        );
    }
    for (id, name) in pending {
        content += &card(
            "Pairing request",
            &form(
                "pair",
                csrf,
                "/devices",
                &(hidden("uniqueid", &id)
                    + &field("name", "Device name", &name, "text")
                    + &field("pin", "Moonlight PIN", "", "text")
                    + "<button>Pair device</button>"),
            ),
        );
    }
    let paired = rows(&clients, "clients");
    if paired.is_empty() {
        content += &card("Paired devices", "<p>No devices paired yet.</p>");
    }
    for client in paired {
        let uuid = text(client, "uuid");
        let perm = client["perm"].as_u64().unwrap_or(0) as u32;
        let mut editor =
            hidden("uuid", uuid) + &field("name", "Device name", text(client, "name"), "text");
        editor += &format!(
            "<label class=\"check\"><input type=\"checkbox\" name=\"enabled\"{}>Allow this device to connect</label><fieldset><legend>Permissions</legend><div class=\"checks\">",
            if client["enabled"] == true {
                " checked"
            } else {
                ""
            }
        );
        for (bit, label) in PERMISSIONS {
            editor += &format!(
                "<label class=\"check\"><input type=\"checkbox\" name=\"perm_{bit}\"{}>{label}</label>",
                if perm & bit != 0 { " checked" } else { "" }
            );
        }
        editor += "</div></fieldset>";
        editor += &super::settings::render("client_", super::settings::CLIENT, client);
        let mut advanced = client.clone();
        for key in ["name", "uuid", "perm", "enabled", "connected", "cert"] {
            advanced.as_object_mut().unwrap().remove(key);
        }
        editor += &format!(
            "<details><summary>Display preferences and connection commands</summary><p>Edit these options as JSON. Use double quotes around text and matching brackets.</p>{}</details>",
            area("advanced", "Device options (JSON)", &raw(&advanced))
        );
        editor += "<button>Save device</button>";
        let actions = button("disconnect", csrf, "/devices", "uuid", uuid, "Disconnect")
            + &button("unpair", csrf, "/devices", "uuid", uuid, "Unpair");
        content += &card(
            text(client, "name"),
            &format!(
                "<p><span class=\"badge\">{}</span></p>{}<div class=\"actions\">{actions}</div>",
                if client["connected"] == true {
                    "Connected"
                } else {
                    "Paired"
                },
                form("client-save", csrf, "/devices", &editor)
            ),
        );
    }
    Ok(content
        + &card(
            "Remote monitor layout",
            &format!(
                "<p>Set the position, size and availability of remote monitors kept between connections. Edit the layout as JSON, using double quotes around text and matching brackets.</p><details><summary>Edit display layout</summary>{}</details>",
                form(
                    "layout",
                    csrf,
                    "/devices",
                    &(area("layout", "Display layout (JSON)", &raw(&layout))
                        + "<button>Save layout</button>")
                )
            ),
        ))
}

async fn settings(
    h: &Shared,
    connection: &Connection,
    headers: &HeaderMap,
    csrf: &str,
) -> Result<String, String> {
    let config = get(h, connection, headers, "/api/config").await?;
    let value = |key: &str, default: &str| {
        config
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .to_owned()
    };
    let mut content =
        "<p class=\"intro\">Choose the capture, encoder, display, and sound for your stream.</p>"
            .to_owned();
    content += "<p class=\"muted\">Automatic chooses an encoder when the stream starts: AMF on AMD GPUs or NVENC on NVIDIA GPUs. If that encoder cannot start, it uses another hardware encoder when one works and shows a warning on the stream card; it never falls back to software. Software encoding requires selecting Software explicitly. Encoder settings below apply only when that encoder is used. The stream card and log show the encoder actually used.</p>";
    let editor = field(
        "sunshine_name",
        "Host name",
        &value(
            "sunshine_name",
            &crate::network::host_name(&h.config.read().unwrap()),
        ),
        "text",
    ) + &select(
        "encoder",
        "Video encoder",
        &value("encoder", "auto"),
        &[
            ("auto", "Automatic"),
            ("amf", "AMD AMF"),
            ("nvenc", "NVIDIA NVENC"),
            ("qsv", "Intel Quick Sync"),
            ("software", "Software"),
            ("pyrowave", "PyroWave"),
        ],
    ) + &select(
        "capture",
        "Screen capture",
        &value("capture", "auto"),
        &[
            ("auto", "Automatic (prefer Windows Graphics Capture)"),
            ("wgc", "Windows Graphics Capture"),
            ("dxgi", "Desktop Duplication"),
        ],
    ) + &field(
        "output_name",
        "Display (leave empty for the primary display)",
        &value("output_name", ""),
        "text",
    ) + &select(
        "virtual_display_mode",
        "Virtual display",
        &value(
            "virtual_display_mode",
            if butterpollo_windows::display::windows_11() {
                "per_client"
            } else {
                "disabled"
            },
        ),
        &[
            ("disabled", "Disabled"),
            ("per_client", "One for each device"),
            ("shared", "Shared"),
        ],
    ) + &field(
        "audio_sink",
        "Audio device (leave empty for the Windows default)",
        &value("audio_sink", ""),
        "text",
    );
    let mut advanced = config.clone();
    for key in [
        "status",
        "platform",
        "version",
        "sunshine_name",
        "encoder",
        "capture",
        "output_name",
        "virtual_display_mode",
        "audio_sink",
    ] {
        advanced.as_object_mut().unwrap().remove(key);
    }
    content += &card(
        "Stream settings",
        &form(
            "config",
            csrf,
            "/settings",
            &(editor
                + &super::settings::render("cfg_", super::settings::GLOBAL, &config)
                + &format!(
                    "<details><summary>Other settings</summary><p>Edit these settings as JSON. Keep the configuration keys unchanged and use double quotes around text.</p>{}</details><button>Save settings</button>",
                    area(
                        "advanced",
                        "Additional configuration (JSON)",
                        &raw(&advanced)
                    )
                )),
        ),
    );
    content += &credentials(csrf, false);
    Ok(content
        + &card(
            "Apply changes",
            &format!(
                "<p>Restart after changing encoder, capture, network, or display settings. Active streams will disconnect.</p>{}",
                button("restart", csrf, "/settings", "", "", "Restart host")
            ),
        ))
}

async fn logs(h: &Shared, connection: &Connection, headers: &HeaderMap) -> Result<String, String> {
    let response = web::api(
        State(h.clone()),
        Extension(connection.clone()),
        Method::GET,
        "/api/logs".parse().unwrap(),
        headers.clone(),
        Bytes::new(),
    )
    .await;
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .map_err(|e| e.to_string())?;
    let start = bytes.len().saturating_sub(256 * 1024);
    let logs = String::from_utf8_lossy(&bytes[start..]);
    Ok(card(
        "Host log",
        &format!(
            "<div class=\"actions\"><a class=\"button secondary\" href=\"/logs\">Reload</a><a class=\"button secondary\" href=\"/api/logs/export\">Download log</a><a class=\"button secondary\" href=\"/api/logs/export_crash\">Download support bundle</a></div><pre class=\"log\">{}</pre><small>Showing the most recent 256 KiB.</small>",
            i18n::data(&logs)
        ),
    ))
}

async fn integrations(
    h: &Shared,
    connection: &Connection,
    headers: &HeaderMap,
) -> Result<String, String> {
    let meta = get(h, connection, headers, "/api/metadata").await?;
    let limiter = get(h, connection, headers, "/api/frame-limiter/status").await?;
    let mut features = "<dl>".to_owned();
    for (key, label) in [
        ("virtual_display", "Virtual displays"),
        ("pyrowave", "PyroWave"),
        ("truehdr_runtime", "NVIDIA RTX HDR"),
    ] {
        features += &format!(
            "<div><dt>{label}</dt><dd>{}</dd></div>",
            if meta["features"][key] == true {
                "Available"
            } else {
                "Unavailable on this computer"
            }
        );
    }
    features += "</dl>";
    Ok(card("Available integrations", &features)
        + &card(
            "External frame limiter",
            &format!(
                "<p>{}</p><dl><div><dt>Configured limiter</dt><dd>{}</dd></div><div><dt>RTSS</dt><dd>{}</dd></div></dl>",
                i18n::data(text(&limiter, "message")),
                match text(&limiter, "configured_provider") {
                    "auto" => "Automatic",
                    "rtss" => "RivaTuner Statistics Server (RTSS)",
                    "nvidia-control-panel" => "NVIDIA driver",
                    "none" | "" => "None",
                    _ => "Other limiter",
                },
                if limiter["rtss_available"] == true {
                    "Detected"
                } else {
                    "Not detected"
                }
            ),
        ))
}

async fn api_tokens(
    h: &Shared,
    connection: &Connection,
    headers: &HeaderMap,
    csrf: &str,
) -> Result<String, String> {
    let tokens = get(h, connection, headers, "/api/tokens").await?;
    let sessions = get(h, connection, headers, "/api/auth/sessions").await?;
    let routes = get(h, connection, headers, "/api/token/routes").await?;
    let mut content = "<p class=\"intro\">Give scripts access to selected API paths and manage signed-in browsers.</p>".to_owned();
    let mut list = String::new();
    for token in rows(&tokens, "tokens") {
        list += &format!(
            "<div class=\"token-row\"><div><strong>{}</strong><small>Created: {}</small>{}</div>{}</div>",
            i18n::data(text(token, "username")),
            i18n::data(&timestamp(&token["created_at"])),
            scope_list(rows(token, "scopes")),
            button(
                "token-delete",
                csrf,
                "/api-tokens",
                "id",
                text(token, "hash"),
                "Revoke"
            )
        );
    }
    if list.is_empty() {
        list += "<p>No API tokens. Create one below for each script that needs access.</p>";
    }
    content += &card("API tokens", &list);
    content += &card("Create token", &form("token-create", csrf, "/api-tokens", &("<p>Select only the API paths and request methods the script needs. Enter permissions as JSON, using the example below. Copy the token after creating it.</p>".to_owned() + &area("scopes", "Token permissions (JSON)", "[{\"path\":\"/api/metadata\",\"methods\":[\"GET\"]}]") + &format!("<details><summary>Available API paths and methods</summary>{}</details><button>Create token</button>", scope_list(rows(&routes, "routes"))) )));
    let mut list = String::new();
    for session in rows(&sessions, "sessions") {
        list += &format!(
            "<div class=\"token-row\"><div><strong>{}{}</strong><small>Created: {}</small></div>{}</div>",
            i18n::data(text(session, "username")),
            if session["current"] == true {
                " · This browser"
            } else {
                ""
            },
            i18n::data(&timestamp(&session["created_at"])),
            button(
                "session-revoke",
                csrf,
                "/api-tokens",
                "id",
                text(session, "id"),
                "Sign out"
            )
        );
    }
    if list.is_empty() {
        list += "<p>No browsers are signed in.</p>";
    }
    Ok(content + &card("Signed-in browsers", &list))
}

async fn maintenance(
    h: &Shared,
    connection: &Connection,
    headers: &HeaderMap,
    csrf: &str,
) -> Result<String, String> {
    let golden = get(
        h,
        connection,
        headers,
        "/api/display/golden_status?compare_current=1",
    )
    .await?;
    let updates = get(h, connection, headers, "/api/updates").await?;
    let crash = get(h, connection, headers, "/api/health/crashdump").await?;
    let mut content = "<p class=\"intro\">Recover displays, inspect updates, and collect support information.</p>".to_owned();
    let actions = button(
        "golden-capture",
        csrf,
        "/maintenance",
        "",
        "",
        "Save current layout",
    ) + &button(
        "golden-restore",
        csrf,
        "/maintenance",
        "",
        "",
        "Restore saved layout",
    ) + &button(
        "golden-delete",
        csrf,
        "/maintenance",
        "",
        "",
        "Delete saved layout",
    );
    content += &card(
        "Display recovery",
        &format!(
            "<p>Save a working display layout before changing your setup.</p><p>{}</p><div class=\"actions\">{actions}</div>",
            if golden["exists"] != true {
                "No display layout is saved."
            } else if golden["comparison_available"] != true {
                "A display layout is saved. It cannot be compared with the current layout right now."
            } else if !text(&golden, "current_mismatch_reason").is_empty() {
                "The saved layout differs from the current layout. Restore it to go back."
            } else {
                "The saved layout matches the current layout."
            }
        ),
    );
    content += &card(
        "Virtual displays",
        &format!(
            "<p>Disconnect remote monitors and release the displays owned by this host.</p>{}",
            button(
                "vdd-terminate",
                csrf,
                "/maintenance",
                "",
                "",
                "Disconnect virtual displays"
            )
        ),
    );
    let mut update_status = format!(
        "<p>{}</p><dl><div><dt>Latest version</dt><dd>{}</dd></div><div><dt>Automatic installation</dt><dd>{}</dd></div></dl>",
        match text(&updates, "phase") {
            "waiting" | "ready" => "The update is waiting for streams and apps to stop.",
            "downloading" => "Downloading the update.",
            "installing" => "Installing the update. Rubylight will restart shortly.",
            "failed" => "The update could not be installed.",
            _ if updates["checking"] == true => "Checking for updates.",
            _ if updates["update_available"] == true => "An update is available.",
            _ => "No update is queued.",
        },
        i18n::data(
            updates["latest_version"]
                .as_str()
                .unwrap_or("Not checked yet")
        ),
        if updates["auto_update"] == true {
            "On"
        } else {
            "Off"
        },
    );
    for key in ["check_error", "error"] {
        if let Some(error) = updates[key].as_str().filter(|error| !error.is_empty()) {
            update_status += &format!("<p class=\"notice error\">{}</p>", i18n::data(error));
        }
    }
    content += &card(
        "Updates",
        &("<div class=\"actions\">".to_owned()
            + &button(
                "check-update",
                csrf,
                "/maintenance",
                "",
                "",
                "Check for updates",
            )
            + &button(
                "install-update",
                csrf,
                "/maintenance",
                "",
                "",
                "Install when idle",
            )
            + &button(
                "cancel-update",
                csrf,
                "/maintenance",
                "",
                "",
                "Cancel queued update",
            )
            + &format!(
                "</div><p>Updates wait until streams and host apps stop. Automatic installation is off by default; enable it in Settings.</p>{update_status}",
            )),
    );
    let mut crash_content = if crash["available"] == true {
        format!(
            "<p>{}</p><dl><div><dt>File</dt><dd>{}</dd></div><div><dt>Captured</dt><dd>{}</dd></div></dl>",
            if crash["dismissed"] == true {
                "No new crash reports. The last report was dismissed."
            } else {
                "A crash report is available in the support bundle."
            },
            i18n::data(text(&crash, "filename")),
            i18n::data(text(&crash, "captured_at")),
        )
    } else {
        "<p>No crash reports from the last 7 days.</p>".to_owned()
    };
    if crash["available"] == true && crash["dismissed"] != true {
        let fields = hidden("filename", crash["filename"].as_str().unwrap_or(""))
            + &hidden("captured_at", crash["captured_at"].as_str().unwrap_or(""));
        crash_content += &form(
            "crash-dismiss",
            csrf,
            "/maintenance",
            &(fields + "<button type=\"submit\">Dismiss this report</button>"),
        );
    }
    content += &card("Crash reports", &crash_content);
    Ok(content
        + &card(
            "Support",
            "<p>The support bundle includes host logs and diagnostics for troubleshooting.</p><a class=\"button secondary\" href=\"/api/logs/export_crash\">Download support bundle</a>",
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamps_accept_numeric_and_saved_text_values() {
        assert_eq!(timestamp(&json!(0)), "1970-01-01 00:00 UTC");
        assert_eq!(timestamp(&json!("60")), "1970-01-01 00:01 UTC");
        assert_eq!(timestamp(&Value::Null), "Not available");
    }
    #[test]
    fn token_permissions_are_readable_and_escaped() {
        let scopes = json!([{"path":"/api/apps/[^/]+/<script>","methods":["GET","DELETE"]}]);
        let html = i18n::render(&scope_list(scopes.as_array().unwrap()), "en");
        assert!(html.contains("/api/apps/{id}/&lt;script&gt;"));
        assert!(html.contains("GET, DELETE"));
        assert!(!html.contains("<pre>"));
        assert!(!html.contains("<script>"));
    }
    #[test]
    fn readiness_shows_escaped_encoder_warnings_until_recovery() {
        let mut meta = json!({
            "encoder_status":{"h264":false,"state":"failed"},
            "warnings":[{"code":"video_encoder","message":"No video encoder available: <driver error>. Retrying."}]
        });
        for state in ["failed", "checking"] {
            meta["encoder_status"]["state"] = json!(state);
            let html = i18n::render(&readiness(&meta), "en");
            assert!(html.contains("No video encoder available: &lt;driver error&gt;. Retrying."));
            assert!(html.contains("role=\"status\""));
            assert!(!html.contains("<driver error>"));
            assert!(!html.contains("<pre>"));
        }
        meta["encoder_status"] = json!({"h264":true,"state":"ready"});
        meta["warnings"] = json!([]);
        assert!(!readiness(&meta).contains("No video encoder available"));
    }
}
