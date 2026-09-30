//! Native Windows capture, input, audio, display and codec integrations.
#[cfg(windows)]
pub mod amf;
#[cfg(windows)]
pub mod audio;
#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod clipboard;
#[cfg(windows)]
pub mod color;
#[cfg(windows)]
pub mod crash;
#[cfg(windows)]
pub mod display;
#[cfg(windows)]
pub mod display_recovery;
#[cfg(windows)]
pub mod encoder;
#[cfg(windows)]
pub mod hdr_profile;
#[cfg(windows)]
pub mod input;
#[cfg(windows)]
pub mod net;
#[cfg(windows)]
pub mod process;
#[cfg(windows)]
pub mod pyrowave;
#[cfg(windows)]
pub mod service;
#[cfg(windows)]
pub mod timing;
#[cfg(windows)]
pub mod tray;
#[cfg(windows)]
pub mod truehdr;

#[cfg(windows)]
#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    clippy::type_complexity
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
