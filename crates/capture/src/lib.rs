#![cfg_attr(test, deny(warnings))]

/// Video (nokhwa) + Audio (cpal) capture
pub mod audio;
/// Always-on 16 kHz monitor stream (audio AI / voice interaction input).
pub mod audio_monitor;
/// USB camera hot-plug detection via udev
pub mod hotplug;
pub mod video;
