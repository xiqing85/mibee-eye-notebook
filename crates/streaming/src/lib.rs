#![cfg_attr(test, deny(warnings))]

/// Stream pipeline: capture -> encode -> distribute
///
/// Central hub connecting media sources to outputs with fan-out,
/// buffer management, and resource control.
pub mod buffer;
pub mod capture_source;
/// Native (ffmpeg-free) codec stack: H.264 encode (openh264), pixel-format
/// conversion (MJPEG/YUYV → YUV420p), and audio encode (G.711 / AAC).
pub mod encoder;
pub mod hub;
pub mod mibee;
pub mod output;
pub mod resource;
pub mod source;
