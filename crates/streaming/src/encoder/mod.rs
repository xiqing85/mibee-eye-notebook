//! Native (ffmpeg-free) codec stack.
//!
//! Replaces every former `ffmpeg` subprocess invocation with in-process Rust
//! crates:
//!
//! | Former ffmpeg role | Native replacement |
//! |--------------------|--------------------|
//! | H.264 encode (libx264) | [`openh264`] (Cisco BSD-2) |
//! | MJPEG → pixels decode | [`jpeg_decoder`] |
//! | YUYV → JPEG encode (preview) | [`jpeg_encoder`] |
//! | AAC encode | G.711 (default) / [`fdk_aac`] (`aac` feature) |
//! | MP4 mux | [`muxide`] (used in `output::file`) |
//!
//! All modules are Linux-only — the capture layer is POSIX-only (V4L2/ALSA),
//! and per AGENTS.md any new platform code must `#[cfg]` all three targets or
//! emit `compile_error!`.
//!
//! [`muxide`]: https://crates.io/crates/muxide

#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

pub mod audio;
pub mod backend;
pub mod convert;
pub mod h264;

// Re-export the most-used types at the module root for ergonomic imports.
#[cfg(feature = "aac")]
pub use audio::AacEncoder;
pub use audio::{AudioCodec, AudioEncoder, G711Encoder};
pub use backend::{BackendConfig, EncoderBackend, SoftwareBackend, select_best};
pub use convert::Yuv420p;
pub use h264::{H264Encoder, H264EncoderConfig, NalUnit};

#[cfg(not(any(target_os = "linux", target_os = "android")))]
compile_error!(
    "mibee-rec's native codec stack currently supports only Linux (V4L2/ALSA capture backends). \
     Adding Windows or macOS support requires implementing the capture layer for those platforms \
     first — see AGENTS.md \"Cross-platform guard\"."
);
