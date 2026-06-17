#![cfg_attr(test, deny(warnings))]

/// Video (nokhwa) + Audio (cpal) capture
pub mod audio;
/// USB camera hot-plug detection via udev
pub mod hotplug;
pub mod video;
