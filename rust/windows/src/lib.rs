//! Native Windows capture, input, audio, display and codec integrations.
#[cfg(windows)]
pub mod amf;
#[cfg(windows)]
mod amf_gpu;
#[cfg(windows)]
pub mod audio;
#[cfg(windows)]
pub mod audio_route;
#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod clipboard;
#[cfg(windows)]
pub mod color;
#[cfg(windows)]
pub mod compute;
#[cfg(windows)]
pub mod crash;
#[cfg(windows)]
mod cursor;
#[cfg(windows)]
pub mod display;
#[cfg(windows)]
pub mod display_arrangement;
#[cfg(windows)]
pub mod display_recovery;
#[cfg(windows)]
pub mod encoder;
#[cfg(windows)]
mod ffmpeg_gpu;
#[cfg(windows)]
pub mod foreground;
#[cfg(windows)]
mod gpu_color;
#[cfg(windows)]
pub mod gpu_priority;
#[cfg(windows)]
pub mod hdr_profile;
#[cfg(windows)]
pub mod hotkey;
#[cfg(windows)]
pub mod image;
#[cfg(windows)]
pub mod input;
#[cfg(windows)]
mod keylayout;
#[cfg(windows)]
pub mod launcher;
#[cfg(windows)]
pub mod limiter;
#[cfg(windows)]
pub mod lossless;
#[cfg(windows)]
pub mod net;
#[cfg(windows)]
mod nvapi;
#[cfg(windows)]
pub mod nvenc;
#[cfg(windows)]
mod nvenc_cuda;
#[cfg(windows)]
pub mod playnite;
#[cfg(windows)]
pub mod present_timing;
#[cfg(windows)]
pub mod process;
#[cfg(windows)]
pub mod pyrowave;
#[cfg(windows)]
pub mod rtss;
#[cfg(windows)]
pub mod rtx_profiles;
#[cfg(windows)]
pub mod service;
#[cfg(windows)]
pub mod steam;
#[cfg(windows)]
pub mod timing;
#[cfg(windows)]
pub mod tray;
#[cfg(windows)]
pub mod truehdr;
#[cfg(windows)]
pub mod vulkan;

#[cfg(windows)]
#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    clippy::all
)]
pub(crate) mod ff {
    include!(concat!(env!("OUT_DIR"), "/ffmpeg.rs"));
}
#[cfg(windows)]
#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    unused_imports,
    clippy::all
)]
pub(crate) mod nvenc_abi {
    include!(concat!(env!("OUT_DIR"), "/nvenc.rs"));
    include!(concat!(env!("OUT_DIR"), "/nvenc_guids.rs"));
}
#[cfg(windows)]
#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    unused_imports,
    clippy::all
)]
pub(crate) mod cuda_abi {
    include!(concat!(env!("OUT_DIR"), "/cuda.rs"));
}
#[cfg(windows)]
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
#[cfg(windows)]
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
