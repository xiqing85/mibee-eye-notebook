#![cfg_attr(test, deny(warnings))]

/// Stream pipeline: capture -> encode -> distribute
///
/// Central hub connecting media sources to outputs with fan-out,
/// buffer management, and resource control.
/// On-device AI object detection (NanoDet-Plus ONNX, SPEC v1 §4.6).
pub mod ai;
pub mod buffer;
pub mod capability;
pub mod capture_source;
/// Native (ffmpeg-free) codec stack: H.264 encode (openh264), pixel-format
/// conversion (MJPEG/YUYV → YUV420p), and audio encode (G.711 / AAC).
pub mod encoder;
/// Fragmented-MP4 remuxer for MSE / `MediaSource` playback.
pub mod fmp4;
pub mod hub;
pub mod mibee;
pub mod output;
pub mod resource;
pub mod source;
