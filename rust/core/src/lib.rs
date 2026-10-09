//! Portable core of the Rubylight host.
//!
//! The Moonlight protocol (pairing, RTSP, packet framing, FEC, input decoding),
//! the host's durable state and configuration, and the policy that decides
//! what the host does: encoder settings, capture pacing, display planning,
//! frame generation, library sync for Steam and Playnite, and updates.
//!
//! Nothing here calls a vendor SDK, so the crate builds and is tested on any
//! OS. `butterpollo-windows` applies these decisions on Windows and the
//! `butterpollo` host crate wires both into its servers. The only Windows
//! calls are the few file operations that need `MoveFileExW`, behind
//! `cfg(windows)`.
#![warn(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::undocumented_unsafe_blocks
)]
pub mod audio;
pub mod auth;
pub mod bitstream;
pub mod browse;
pub mod capture_policy;
pub mod catalog;
pub mod config;
pub mod crypto;
pub mod display_policy;
pub mod edid;
pub mod encoder_policy;
pub mod fec_status;
pub mod framegen;
pub mod hdr;
pub mod hotkey;
pub mod input;
pub mod input_policy;
pub mod logfile;
pub mod lossless;
pub mod ltr;
pub mod migration;
pub mod network_pacing;
pub mod nvenc;
pub mod packet;
pub mod pairing;
pub mod paths;
pub mod performance;
pub mod playnite;
pub mod present_timing;
pub mod pyrowave;
pub mod remote;
pub mod rtsp;
pub mod rtx_policy;
pub mod session;
pub mod state;
pub mod steam;
pub mod steam_deck;
pub mod stream_policy;
pub mod topology;
pub mod tray;
mod update_files;
pub mod update_recovery;
pub mod usbip;
pub mod version;
