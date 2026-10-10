//! Windows platform layer of the Rubylight host.
//!
//! Everything that calls Win32, Direct3D, Windows Graphics Capture or a GPU
//! vendor SDK lives here: capture, Radeon compute colour conversion, the AMF,
//! NVENC, FFmpeg and PyroWave encoders, display and virtual display control,
//! input injection, audio, process and service management.
//!
//! The workspace is layered: `butterpollo-core` holds the portable protocol
//! and policy code and is tested on any OS; this crate turns those decisions
//! into Windows calls; the `butterpollo` host crate runs the servers and wires
//! the two together. The crate is empty on other targets.
#![cfg(windows)]
#![warn(clippy::undocumented_unsafe_blocks)]
#![warn(clippy::print_stdout, clippy::print_stderr)]

pub mod amf;
pub mod audio;
pub mod audio_route;
pub mod capture;
pub mod clipboard;
pub mod codec_probe;
pub mod color;
pub mod compute;
pub mod crash;
mod cursor;
pub mod device_loss;
pub mod display;
pub mod display_arrangement;
pub mod display_recovery;
pub mod encoder;
pub mod files;
pub mod firewall;
pub mod foreground;
mod gpu_color;
pub mod gpu_priority;
pub mod gpu_probe;
pub mod hdr_profile;
pub mod hotkey;
pub mod image;
pub mod input;
mod ipc;
mod keylayout;
pub mod launcher;
pub mod limiter;
pub mod lossless;
pub mod mic;
pub mod net;
mod nvapi;
pub mod nvenc;
pub mod playnite;
pub mod present_timing;
pub mod process;
pub mod pyrowave;
pub mod rtss;
pub mod rtx_profiles;
pub mod service;
pub mod steam;
mod text;
pub mod timing;
pub mod tray;
pub mod truehdr;
pub mod vulkan;

// Bindgen emits these unsafe blocks; the restriction lint is not covered by clippy::all.
#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    clippy::all,
    clippy::undocumented_unsafe_blocks
)]
pub(crate) mod ff {
    include!(concat!(env!("OUT_DIR"), "/ffmpeg.rs"));
}
#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    unused_imports,
    clippy::all,
    clippy::undocumented_unsafe_blocks
)]
pub(crate) mod nvenc_abi {
    include!(concat!(env!("OUT_DIR"), "/nvenc.rs"));
    include!(concat!(env!("OUT_DIR"), "/nvenc_guids.rs"));
}
#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    unused_imports,
    clippy::all,
    clippy::undocumented_unsafe_blocks
)]
pub(crate) mod cuda_abi {
    include!(concat!(env!("OUT_DIR"), "/cuda.rs"));
}
#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    unused_imports,
    clippy::upper_case_acronyms
)]
pub(crate) mod amf_abi {
    include!(concat!(env!("OUT_DIR"), "/amf.rs"));
}
#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    unused_imports
)]
pub(crate) mod pyro_abi {
    include!(concat!(env!("OUT_DIR"), "/pyrowave.rs"));
}
