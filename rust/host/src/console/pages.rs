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
    let mut out = "<ul class=\"readiness\">".to_owned();
    for (ready, label, detail) in [
        (
            video,
            "Video",
            if video {
                "Ready to stream. Choose a codec in Moonlight."
            } else if checking {
                "Checking your graphics card. Reload in a few seconds."
            } else {
                "No working encoder found. Check that your graphics card is enabled and its driver is installed, then restart Butterpollo."
            },
        ),
        (
            display_ready,
            "Display",
            if display_ready {
                "Ready to capture."
            } else if virtual_requested {
                "Virtual display is unavailable. Start the installed Butterpollo service or select your physical display in Settings."
            } else {
                "No active display found. Turn on a monitor or set up a virtual display in Settings."
            },
        ),
        (
            audio_ready || meta["audio_enabled"] == false,
            "Sound",
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
    if !display_ready || !video || !audio_ready {
        out += &format!(
            "<details><summary>Details for troubleshooting</summary><pre>{}</pre></details>",
            pretty(
                &json!({"display":meta["virtual_display"],"audio":meta["audio_error"],"capture":meta["capture_status"]["error"]})
            )
        );
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
        "<button>Change credentials</button>"
    };
    card(
        if setup {
            "Set up your account"
        } else {
            "Administrator account"
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
    Ok(match path {
        "/login" => card(
            "Welcome back",
            &form(
                "login",
                csrf,
                "/login",
                &(field("username", "Username", "", "text")
                    + &field("password", "Password", "", "password")
                    + "<label><input type=\"checkbox\" name=\"remember_me\" value=\"true\"> Remember this device</label>"
                    + "<button>Sign in</button>"),
            ),
        ),
        "/setup" => credentials(csrf, true),
        "/" => {
            let meta = get(h, connection, headers, "/api/metadata").await?;
            let sessions = get(h, connection, headers, "/api/rtsp/sessions").await?;
            let active = rows(&sessions, "sessions");
            let mut content = format!(
                "<p class=\"intro\">Manage your library, pair devices, and tune your stream.</p><div class=\"metrics\"><section class=\"card\"><span>Host</span><strong>{}</strong><small>Version {}</small></section><section class=\"card\"><span>Active streams</span><strong>{}</strong><small>Connected right now</small></section><section class=\"card\"><span>Encoder</span><strong>{}</strong><small>Capture: {}</small></section></div>",
                i18n::data(text(&meta, "host_name")),
                i18n::data(text(&meta, "version")),
                active.len(),
                i18n::data(text(&meta["encoder_status"], "state")),
                i18n::data(text(&meta["capture_status"], "configured_backend"))
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
                    "<div class=\"badges\">{badges}</div><p>H.264, HEVC and AV1 work with Moonlight. PyroWave and VRR require <a href=\"https://github.com/Nonary/moonlight-qt\" target=\"_blank\" rel=\"noopener noreferrer\">Nonary’s Moonlight client</a>. PyroWave is best suited to a fast wired LAN.</p>"
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
                "<div class=\"quick-links\"><a href=\"/devices\"><strong>Pair a device</strong><span>Connect Moonlight to this host</span></a><a href=\"/library\"><strong>Your library</strong><span>Manage applications and desktop streaming</span></a><a href=\"/settings\"><strong>Stream settings</strong><span>Choose your encoder, display, and audio</span></a></div>",
            );
            if active.is_empty() {
                content += &card(
                    "Active sessions",
                    "<p>No devices are streaming. Connect from Moonlight to start.</p>",
                );
            } else {
                let mut table = "<div class=\"table-scroll\"><table><thead><tr><th>Device</th><th>Video</th><th>Recent rate</th><th>Encode p95</th><th></th></tr></thead><tbody>".to_owned();
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
                            "<tr><td>{}</td><td>Remote Input</td><td colspan=\"2\">Input connection active</td><td>{disconnect}</td></tr>",
                            i18n::data(text(session, "device_name")),
                        );
                        continue;
                    }
                    table += &format!(
                        "<tr><td>{}</td><td>{}×{} · {} fps{}{}{}</td><td>{:.1} fps<br>{:.1} Mbps</td><td>{:.2} ms</td><td>{disconnect}</td></tr>",
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
                            "<br>Remote Monitor"
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
                content += &card("Active sessions", &(table + "</tbody></table></div>"));
                for session in active {
                    let skipped = session["frames_replaced"].as_u64().unwrap_or(0);
                    if skipped > 0 {
                        content += &format!(
                            "<p>{}: {skipped} older frames skipped to keep the stream current. If this keeps increasing, lower the PyroWave bitrate or check your wired connection.</p>",
                            i18n::data(text(session, "device_name"))
                        );
                    }
                }
                content += "<p class=\"muted\">Rates cover the last two seconds. An unchanged desktop may send fewer frames. Encode latency covers the host encoder; client decoding and network delay add to it.</p>";
                for session in active {
                    let history = rows(&session["performance"], "history");
                    if history.is_empty() {
                        continue;
                    }
                    let mut table = "<details><summary>Recent performance history</summary><div class=\"table-scroll\"><table><thead><tr><th>Sample</th><th>Frames/sec</th><th>Mbps</th><th>Mean encode</th><th>Slowest encode</th></tr></thead><tbody>".to_owned();
                    for (i, sample) in history.iter().enumerate().rev().take(30) {
                        table += &format!(
                            "<tr><td>{}</td><td>{:.1}</td><td>{:.1}</td><td>{:.2} ms</td><td>{:.2} ms</td></tr>",
                            i + 1,
                            sample["fps"].as_f64().unwrap_or(0.),
                            sample["bitrate_mbps"].as_f64().unwrap_or(0.),
                            sample["encode_mean_ms"].as_f64().unwrap_or(0.),
                            sample["encode_max_ms"].as_f64().unwrap_or(0.)
                        );
                    }
                    table += "</tbody></table></div><p>The latest 30 completed intervals are shown. The session API retains up to two minutes.</p></details>";
                    content += &card(text(session, "device_name"), &table);
                }
            }
            content
                + if query.get("live").is_some_and(|v| v == "1") {
                    "<p class=\"muted\">Updating every five seconds. <a href=\"/\">Pause updates</a></p>"
                } else {
                    "<p class=\"muted\"><a href=\"/?live=1\">Update every five seconds</a></p>"
                }
        }
        "/library" => {
            let document = get(h, connection, headers, "/api/apps").await?;
            let apps = rows(&document, "apps");
            let mut content = "<p class=\"intro\">Launch your desktop or an application from any paired device.</p><div class=\"library-grid\">".to_owned();
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
                + &field("name", "Application name", text(&edit, "name"), "text")
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
                    "<details><summary>Preparation commands and advanced options</summary>{}</details>",
                    area("advanced", "Application options (JSON)", &raw(&advanced))
                )
                + "<button>Save application</button> <a href=\"/library\">Cancel</a>";
            content += &card(
                if text(&edit, "uuid").is_empty() {
                    "Add application"
                } else {
                    "Edit application"
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
                    "Live TrueHDR settings",
                    &form(
                        "app-live",
                        csrf,
                        "/library",
                        &(hidden("uuid", text(&edit, "uuid"))
                            + "<p>Apply brightness and colour settings to this running application without saving them. An empty object restores inherited settings.</p>"
                            + &area(
                                "overrides",
                                "TrueHDR settings (JSON)",
                                &raw(&Value::Object(overrides)),
                            )
                            + "<button>Apply to running application</button>"),
                    ),
                );
            }
            content
                + &button(
                    "app-close",
                    csrf,
                    "/library",
                    "",
                    "",
                    "Close running application",
                )
        }
        "/devices" => {
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
                "<p class=\"intro\">Pair Moonlight clients and choose what each device can do.</p>"
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
                let mut editor = hidden("uuid", uuid)
                    + &field("name", "Device name", text(client, "name"), "text");
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
                for key in ["name", "uuid", "perm", "enabled", "connected"] {
                    advanced.as_object_mut().unwrap().remove(key);
                }
                editor += &format!(
                    "<details><summary>Display overrides, HDR profile, and connection commands</summary>{}</details>",
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
            content
                + &card(
                    "Remote monitor layout",
                    &format!(
                        "<p>Set positions, sizes, and enabled states for retained remote monitors.</p><details><summary>Edit display layout</summary>{}</details>",
                        form(
                            "layout",
                            csrf,
                            "/devices",
                            &(area("layout", "Display layout (JSON)", &raw(&layout))
                                + "<button>Save layout</button>")
                        )
                    ),
                )
        }
        "/settings" => {
            let config = get(h, connection, headers, "/api/config").await?;
            let value = |key: &str, default: &str| {
                config
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or(default)
                    .to_owned()
            };
            let mut content = "<p class=\"intro\">Choose the capture, encoder, display, and sound for your stream.</p>".to_owned();
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
                    ("auto", "Automatic"),
                    ("wgc", "Windows Graphics Capture"),
                    ("dxgi", "Desktop Duplication"),
                ],
            ) + &field(
                "output_name",
                "Display (blank for primary)",
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
                    ("per_client", "One per client"),
                    ("shared", "Shared"),
                ],
            ) + &field(
                "audio_sink",
                "Audio device (blank for system default)",
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
                            "<details><summary>All other settings</summary>{}</details><button>Save settings</button>",
                            area(
                                "advanced",
                                "Additional configuration (JSON)",
                                &raw(&advanced)
                            )
                        )),
                ),
            );
            content += &credentials(csrf, false);
            content
                + &card(
                    "Apply changes",
                    &format!(
                        "<p>Restart after changing encoder, capture, network, or display settings. Active streams will disconnect.</p>{}",
                        button("restart", csrf, "/settings", "", "", "Restart host")
                    ),
                )
        }
        "/logs" => {
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
            card(
                "Host log",
                &format!(
                    "<div class=\"actions\"><a class=\"button secondary\" href=\"/logs\">Reload</a><a class=\"button secondary\" href=\"/api/logs/export\">Download log</a><a class=\"button secondary\" href=\"/api/logs/export_crash\">Download support bundle</a></div><pre class=\"log\">{}</pre><small>Showing the most recent 256 KiB.</small>",
                    i18n::data(&logs)
                ),
            )
        }
        "/integrations" => {
            let meta = get(h, connection, headers, "/api/metadata").await?;
            let limiter = get(h, connection, headers, "/api/frame-limiter/status").await?;
            let mut features = "<dl>".to_owned();
            for (key, label) in [
                ("virtual_display", "Virtual displays"),
                ("pyrowave", "PyroWave"),
                ("truehdr_runtime", "NVIDIA TrueHDR runtime"),
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
            card("Runtime integrations", &features)
                + &card(
                    "External frame limiter",
                    &format!(
                        "<p>{}</p><dl><div><dt>Configured provider</dt><dd>{}</dd></div><div><dt>RTSS</dt><dd>{}</dd></div></dl>",
                        i18n::data(text(&limiter, "message")),
                        i18n::data(text(&limiter, "configured_provider")),
                        if limiter["rtss_available"] == true {
                            "Detected"
                        } else {
                            "Not detected"
                        }
                    ),
                )
        }
        "/api-tokens" => {
            let tokens = get(h, connection, headers, "/api/tokens").await?;
            let sessions = get(h, connection, headers, "/api/auth/sessions").await?;
            let routes = get(h, connection, headers, "/api/token/routes").await?;
            let mut content = "<p class=\"intro\">Grant scoped access to an integration and manage administrator sessions.</p>".to_owned();
            let mut list = String::new();
            for token in rows(&tokens, "tokens") {
                list += &format!(
                    "<div class=\"token-row\"><div><strong>{}</strong><small>Created: {}</small><pre>{}</pre></div>{}</div>",
                    i18n::data(text(token, "username")),
                    i18n::data(&token["created_at"].to_string()),
                    pretty(&token["scopes"]),
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
                list += "<p>No API tokens issued.</p>";
            }
            content += &card("API tokens", &list);
            content += &card("Create token", &form("token-create", csrf, "/api-tokens", &("<p>Select only the paths and methods this integration needs. Copy the token after creating it.</p>".to_owned() + &area("scopes", "Token scopes (JSON)", "[{\"path\":\"/api/metadata\",\"methods\":[\"GET\"]}]") + &format!("<details><summary>Available routes</summary><pre>{}</pre></details><button>Create token</button>", pretty(&routes["routes"])) )));
            let mut list = String::new();
            for session in rows(&sessions, "sessions") {
                list += &format!(
                    "<div class=\"token-row\"><div><strong>{}{}</strong><small>Created: {}</small></div>{}</div>",
                    i18n::data(text(session, "username")),
                    if session["current"] == true {
                        " · This session"
                    } else {
                        ""
                    },
                    i18n::data(&session["created_at"].to_string()),
                    button(
                        "session-revoke",
                        csrf,
                        "/api-tokens",
                        "id",
                        text(session, "id"),
                        "Revoke"
                    )
                );
            }
            content + &card("Administrator sessions", &list)
        }
        "/maintenance" => {
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
                "Remove saved layout",
            );
            content += &card(
                "Display recovery",
                &format!(
                    "<p>Save a known working display layout before changing your setup.</p><div class=\"actions\">{actions}</div><details><summary>Saved layout status</summary><pre>{}</pre></details>",
                    pretty(&golden)
                ),
            );
            content += &card(
                "Virtual monitors",
                &format!(
                    "<p>Disconnect remote monitors and release the displays owned by this host.</p>{}",
                    button(
                        "vdd-terminate",
                        csrf,
                        "/maintenance",
                        "",
                        "",
                        "Disconnect virtual monitors"
                    )
                ),
            );
            content += &card(
                "Updates",
                &(button(
                    "check-update",
                    csrf,
                    "/maintenance",
                    "",
                    "",
                    "Check for updates",
                ) + &format!("<pre>{}</pre>", pretty(&updates))),
            );
            let mut crash_content = format!("<pre>{}</pre>", pretty(&crash));
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
            content
                + &card(
                    "Support",
                    "<p>The support bundle includes host logs and diagnostics for troubleshooting.</p><a class=\"button secondary\" href=\"/api/logs/export_crash\">Download support bundle</a>",
                )
        }
        _ => return Err("Page not found".into()),
    })
}
