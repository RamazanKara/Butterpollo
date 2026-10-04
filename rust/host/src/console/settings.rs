use super::*;
#[derive(Clone, Copy)]
pub enum Control {
    Bool,
    Choice(&'static [&'static str]),
    Text,
    Number,
    Json,
}
pub struct Setting {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub control: Control,
}
macro_rules! settings { ($($group:literal => [$($key:literal, $label:literal, $control:expr;)*])*) => { &[$($(Setting { key: $key, label: $label, group: $group, control: $control },)*)*] }; }
use Control::*;
pub const GLOBAL: &[Setting] = settings! {
    "Audio" => [
        "virtual_sink", "Virtual audio endpoint", Text;
        "stream_audio", "Stream audio", Bool;
        "audio_sink_capture_only", "Capture selected audio without changing defaults", Bool;
        "auto_capture_sink", "Follow audio device changes", Bool;
        "keep_sink_default", "Keep virtual speakers as the default during streaming", Bool;
        "install_steam_audio_drivers", "Install Steam streaming speakers when needed", Bool;
    ]
    "Displays and HDR" => [
        "virtual_display_layout", "Virtual display layout", Choice(&["exclusive","extended","extended_primary","extended_isolated","extended_primary_isolated"]);
        "dd_configuration_option", "Display activation", Choice(&["disabled","verify_only","ensure_active","ensure_primary","ensure_only_display"]);
        "dd_resolution_option", "Resolution policy", Choice(&["disabled","auto","manual"]);
        "dd_manual_resolution", "Manual resolution (WIDTHxHEIGHT)", Text;
        "dd_refresh_rate_option", "Refresh policy", Choice(&["disabled","auto","manual","prefer_highest"]);
        "dd_manual_refresh_rate", "Manual refresh rate (supports 59.94, 119.88)", Number;
        "dd_hdr_option", "HDR policy", Choice(&["disabled","auto"]);
        "dd_hdr_request_override", "Client HDR request", Choice(&["auto","force_on","force_off"]);
        "dd_config_revert_on_disconnect", "Restore displays when the stream disconnects", Bool;
        "dd_config_revert_delay", "Display restore delay (milliseconds)", Number;
        "dd_paused_virtual_display_timeout_secs", "Paused display timeout (seconds; 0 keeps it)", Number;
        "dd_always_restore_from_golden", "Restore the saved display baseline", Bool;
        "dd_activate_virtual_display", "Activate virtual display", Bool;
        "dd_virtual_display_scale", "Virtual display scale (%)", Number;
        "dd_virtual_display_permanent_count", "Permanent virtual displays (0–4)", Number;
        "dd_snapshot_exclude_devices", "Devices excluded from baseline restore", Json;
        "dd_mode_remapping", "Resolution and refresh remapping", Json;
        "vulkan_hdr_layer", "Vulkan virtual HDR support", Bool;
        "fallback_mode", "Default launch mode (WIDTHxHEIGHTxFPS)", Text;
    ]
    "Frame generation and limiting" => [
        "frame_limiter_enable", "Enable frame limiter", Bool;
        "frame_limiter_provider", "Limiter provider", Choice(&["auto","rtss","nvidia-control-panel","none"]);
        "frame_limiter_fps_limit", "Frame limit (0 uses the client rate)", Number;
        "frame_limiter_auto_virtual_framegen", "Virtual display frame generation", Choice(&["legacy","enabled","fixed-1000hz","disabled"]);
        "frame_limiter_disable_vsync", "Disable driver vertical sync during streaming", Bool;
        "rtss_install_path", "RTSS installation directory", Text;
        "rtss_frame_limit_type", "RTSS limiter mode", Choice(&["async","front-edge-sync","back-edge-sync","nvidia-reflex"]);
        "rtss_allow_virtual_display_override", "Allow virtual display limiter overrides", Bool;
    ]
    "Video and capture" => [
        "adapter_name", "Encoding graphics adapter", Text;
        "adapter_pnp_id", "Encoding adapter device ID", Text;
        "hevc_mode", "HEVC support (0 auto, 1 disabled, 2 SDR, 3 HDR)", Number;
        "av1_mode", "AV1 support (0 auto, 1 disabled, 2 SDR, 3 HDR)", Number;
        "max_bitrate", "Maximum bitrate (Kbps; 0 has no configured limit)", Number;
        "limit_framerate", "Use the launch frame rate for encoding", Bool;
        "minimum_fps_target", "Minimum frame rate for an unchanged desktop", Number;
        "wgc_pacing_smoothing", "Smooth capture pacing", Bool;
        "wgc_direct_encoder_input", "Pass GPU capture directly to the encoder", Bool;
        "gpu_compute_conversion", "Copy and convert captures on a compute queue", Bool;
        "wgc_slot_aligned_publish", "Align capture publication to frame slots", Bool;
        "fec_percentage", "Forward error correction (%)", Number;
        "pyrowave", "Enable PyroWave for compatible clients", Bool;
        "pyrowave_critical_fec_percentage", "PyroWave protection for required image data (%)", Number;
        "packetsize", "Video packet size (0 uses the client request)", Number;
        "video_max_batch_size_kb", "Video send batch limit (KiB)", Number;
        "pacing_max_bitrate_kbps", "Network pacing limit (Kbps; 0 uses 1 Gbps)", Number;
    ]
    "AMD encoding" => [
        "amd_usage", "AMF usage", Choice(&["auto","transcoding","ultralowlatency","lowlatency","webcam","high_quality","lowlatency_high_quality"]);
        "amd_quality", "AMF quality", Choice(&["auto","speed","balanced","quality"]);
        "amd_rc", "AMF rate control", Choice(&["auto","cqp","cbr","vbr_latency","vbr_peak","qvbr","hqvbr","hqcbr"]);
        "amd_preanalysis", "Pre-analysis", Bool;
        "amd_vbaq", "Adaptive quantization", Bool;
        "amd_enforce_hrd", "Enforce decoder buffer limits", Bool;
        "amd_coder", "H.264 entropy coder", Choice(&["auto","cabac","cavlc"]);
        "amd_high_motion_quality_boost", "High motion quality boost", Bool;
        "amd_low_latency_internal", "AMF internal low latency", Bool;
        "amd_query_timeout", "AMF query timeout (milliseconds)", Number;
        "amd_input_queue_size", "AMF input queue size", Number;
        "amd_ltr_frames", "Long-term reference frames", Number;
    ]
    "NVIDIA and Intel encoding" => [
        "nvenc_preset", "NVENC preset (1–7)", Number;
        "nvenc_twopass", "NVENC multi-pass", Choice(&["disabled","quarter_res","full_res"]);
        "nvenc_spatial_aq", "NVENC spatial adaptive quantization", Bool;
        "nvenc_temporal_aq", "NVENC temporal adaptive quantization", Bool;
        "nvenc_vbv_increase", "NVENC decoder buffer increase (%)", Number;
        "nvenc_h264_cavlc", "NVENC H.264 CAVLC", Bool;
        "nvenc_split_encode", "NVENC split encoding", Choice(&["auto","disabled","forced"]);
        "nvenc_realtime_hags", "NVENC real-time scheduling with HAGS", Bool;
        "nvenc_latency_over_power", "Prefer low NVENC latency over power savings", Bool;
        "nvenc_opengl_vulkan_on_dxgi", "Driver DXGI presentation for OpenGL and Vulkan", Bool;
        "qsv_preset", "Quick Sync preset", Choice(&["veryfast","faster","fast","medium","slow","slower","veryslow"]);
        "qsv_coder", "Quick Sync H.264 entropy coder", Choice(&["auto","cabac","cavlc"]);
        "qsv_slow_hevc", "Use Quick Sync software HEVC when needed", Bool;
    ]
    "Software encoding and TrueHDR" => [
        "sw_preset", "Software encoder preset", Choice(&["ultrafast","superfast","veryfast","faster","fast","medium","slow","slower","veryslow"]);
        "sw_tune", "Software encoder tuning", Text;
        "min_threads", "Software encoder threads", Number;
        "crf", "Constant rate factor", Number;
        "qp", "Constant quantizer", Number;
        "rtx_hdr", "NVIDIA TrueHDR", Bool;
        "rtx_hdr_sdr_brightness", "SDR brightness for TrueHDR", Number;
        "rtx_hdr_contrast", "TrueHDR contrast", Number;
        "rtx_hdr_saturation", "TrueHDR saturation", Number;
        "rtx_hdr_middle_gray", "TrueHDR middle gray", Number;
        "rtx_hdr_peak_brightness", "TrueHDR peak brightness (nits)", Number;
    ]
    "Input" => [
        "keyboard", "Keyboard input", Bool;
        "mouse", "Mouse input", Bool;
        "controller", "Controller input", Bool;
        "native_pen_touch", "Native pen and touch", Bool;
        "gamepad", "Controller profile", Choice(&["auto","vhf_xbox","vhf_xbox_one","vhf_ds4","vhf_ds5","vhf_switch"]);
        "keybindings", "Keyboard remapping pairs", Json;
        "always_send_scancodes", "Send keyboard scan codes", Bool;
        "key_rightalt_to_key_win", "Map right Alt to Windows key", Bool;
        "key_repeat_delay", "Keyboard repeat delay (milliseconds)", Number;
        "key_repeat_frequency", "Keyboard repeat frequency (Hz)", Number;
        "high_resolution_scrolling", "High resolution scrolling", Bool;
        "forward_rumble", "Controller vibration", Bool;
        "motion_as_ds4", "Use PlayStation profile for motion input", Bool;
        "touchpad_as_ds4", "Use PlayStation profile for touchpad input", Bool;
        "back_button_timeout", "Hold Back to press Guide (milliseconds; -1 disables)", Number;
        "enable_input_only_mode", "Allow input-only sessions", Bool;
    ]
    "Remote monitors" => [
        "remote_monitor_mute_audio", "Mute remote monitor audio", Bool;
        "remote_monitor_disconnect_on_stream_end", "Remove remote monitor when its stream ends", Bool;
        "remote_monitor_disconnect_on_client_disconnect", "Remove remote monitor on client disconnect", Bool;
        "remote_monitor_terminate_on_first_request", "Replace the current game on the first monitor request", Bool;
        "remote_monitor_confirm_app_replacement", "Require confirmation to replace a game with a monitor", Bool;
    ]
    "Network, administration and commands" => [
        "locale", "Language", Choice(&["en","en_GB","en_US","bg","cs","de","es","fr","hu","it","ja","ko","pl","pt","pt_BR","ru","sv","tr","uk","vi","zh","zh_TW"]);
        "port", "Base Moonlight port", Number;
        "bind_address", "Network interface (blank uses all)", Text;
        "address_family", "IP support", Choice(&["ipv4","both"]);
        "enable_discovery", "Moonlight LAN discovery", Bool;
        "enable_pairing", "Allow new client pairings", Bool;
        "upnp", "UPnP router port forwarding", Bool;
        "lan_encryption_mode", "LAN encryption (0 never, 1 optional, 2 required)", Number;
        "wan_encryption_mode", "WAN encryption (0 never, 1 optional, 2 required)", Number;
        "ping_timeout", "Client timeout (milliseconds)", Number;
        "origin_web_ui_allowed", "Administration access", Choice(&["pc","lan","wan"]);
        "csrf_allowed_origins", "Additional trusted administration origins", Json;
        "session_token_ttl_seconds", "Sign-in session lifetime (seconds)", Number;
        "remember_me_refresh_token_ttl_seconds", "Remembered device lifetime (seconds)", Number;
        "system_tray", "Show system tray icon", Bool;
        "hide_tray_controls", "Hide tray shutdown controls", Bool;
        "min_log_level", "Log level (0 verbose, 1 debug, 2 info, 3 warning, 4 error, 6 off)", Number;
        "log_path", "Log file path", Text;
        "notify_pre_releases", "Include prerelease updates", Bool;
        "update_check_interval", "Update check interval (seconds; 0 disables)", Number;
        "legacy_ordering", "Use legacy application ordering", Bool;
        "envvar_compatibility_mode", "Set legacy SUNSHINE client environment variables", Bool;
        "global_prep_cmd", "Global preparation and undo commands", Json;
        "global_state_cmd", "Global application state commands", Json;
        "server_cmd", "Client-triggered server commands", Json;
    ]
};
pub const APP: &[Setting] = settings! {
    "Application behavior" => [
        "elevated", "Run as administrator", Bool;
        "auto-detach", "Keep streaming after an application launcher exits", Bool;
        "wait-all", "Wait for all application processes", Bool;
        "exit-timeout", "Application shutdown timeout (seconds)", Number;
        "terminate-on-pause", "Close the application when streaming pauses", Bool;
        "allow-client-commands", "Allow per-client connection commands", Bool;
        "exclude-global-state-cmd", "Exclude global pause and resume commands", Bool;
        "state-cmd", "Application resume and pause commands", Json;
        "output", "Application log path (null discards output)", Text;
        "gamepad", "Controller profile override", Choice(&["auto","vhf_xbox","vhf_xbox_one","vhf_ds4","vhf_ds5","vhf_switch"]);
    ]
    "Application display and frame generation" => [
        "display-output", "Display override (sunshine:virtual_display selects a virtual display)", Text;
        "virtual-display", "Use a virtual display", Bool;
        "virtual-screen", "Use application virtual display preferences", Bool;
        "virtual-display-mode", "Virtual display mode", Choice(&["disabled","per_client","shared"]);
        "virtual-display-layout", "Virtual display layout", Choice(&["exclusive","extended","extended_primary","extended_isolated","extended_primary_isolated"]);
        "virtual-display-primary", "Make the virtual display primary", Bool;
        "dd-configuration-option", "Display activation override", Choice(&["disabled","verify_only","ensure_active","ensure_primary","ensure_only_display"]);
        "scale-factor", "Display resolution scale (%)", Number;
        "use-app-identity", "Keep one virtual display identity for this application", Bool;
        "per-client-app-identity", "Keep a display identity for each application and client", Bool;
        "prefer-10bit-sdr", "Prefer ten-bit SDR", Bool;
        "frame-generation-mode", "Frame generation", Choice(&["none","game-provided","nvidia-smooth-motion"]);
        "gen1-framegen-fix", "Legacy frame generation capture fix", Bool;
        "frame-gen-limiter-fix", "Frame generation limiter fix", Bool;
    ]
    "Application TrueHDR" => [
        "rtx-hdr", "TrueHDR override", Bool;
        "rtx-hdr-sdr-brightness", "SDR brightness override", Number;
        "rtx-hdr-contrast", "TrueHDR contrast override", Number;
        "rtx-hdr-saturation", "TrueHDR saturation override", Number;
        "rtx-hdr-middle-gray", "TrueHDR middle gray override", Number;
        "rtx-hdr-peak-brightness", "TrueHDR peak brightness override (nits)", Number;
    ]
};
pub const CLIENT: &[Setting] = settings! {
    "Device preferences" => [
        "output_name_override", "Display override", Text;
        "always_use_virtual_display", "Always use a virtual display", Bool;
        "virtual_display_mode", "Virtual display mode", Choice(&["disabled","per_client","shared"]);
        "virtual_display_layout", "Virtual display layout", Choice(&["exclusive","extended","extended_primary","extended_isolated","extended_primary_isolated"]);
        "prefer_10bit_sdr", "Prefer ten-bit SDR", Bool;
        "hdr_profile", "HDR ICC profile filename", Text;
        "allow_client_commands", "Run this device's connection commands", Bool;
        "do", "Connection commands", Json;
        "undo", "Disconnect commands", Json;
    ]
};
pub fn render(prefix: &str, schema: &[Setting], values: &Value) -> String {
    let mut output = String::new();
    let mut previous = "";
    for setting in schema {
        if setting.group != previous {
            if !previous.is_empty() {
                output += "</div></details>";
            }
            output += &format!(
                "<details><summary>{}</summary><div class=\"settings-grid\">",
                i18n::message(setting.group)
            );
            previous = setting.group;
        }
        let name = format!("{prefix}{}", setting.key);
        let value = values
            .get(setting.key)
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| raw_value(value))
            })
            .unwrap_or_default();
        output += &match setting.control {
            Bool | Choice(_) => {
                let choices = match setting.control {
                    Bool => &["true", "false"][..],
                    Choice(choices) => choices,
                    _ => unreachable!(),
                };
                let mut options = vec![("", "Use default")];
                options.extend(choices.iter().map(|&choice| {
                    (
                        choice,
                        match choice {
                            "true" => "Enabled",
                            "false" => "Disabled",
                            _ if setting.key == "locale" => i18n::language_name(choice),
                            _ => choice,
                        },
                    )
                }));
                if !value.is_empty() && !choices.contains(&value.as_str()) {
                    options.push((&value, &value));
                }
                select(&name, setting.label, &value, &options)
            }
            Json => area(&name, setting.label, &value),
            Number | Text => field(&name, setting.label, &value, "text"),
        };
    }
    if !previous.is_empty() {
        output += "</div></details>";
    }
    output
}
fn raw_value(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}
pub fn apply(
    prefix: &str,
    schema: &[Setting],
    fields: &Fields,
    values: &mut serde_json::Map<String, Value>,
) -> Result<(), String> {
    for setting in schema {
        if let Some(value) = fields.get(&format!("{prefix}{}", setting.key)) {
            let value = value.trim();
            if value.is_empty() {
                values.insert(setting.key.into(), Value::Null);
            } else if matches!(setting.control, Json) {
                values.insert(
                    setting.key.into(),
                    serde_json::from_str(value)
                        .map_err(|error| format!("{}: {error}", setting.label))?,
                );
            } else {
                values.insert(setting.key.into(), Value::String(value.into()));
            }
        }
    }
    Ok(())
}
