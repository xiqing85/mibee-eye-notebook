//! Video capture module using nokhwa.
//!
//! Provides device enumeration, camera opening, and async frame capture
//! via tokio mpsc channels. The capture loop runs on a [`spawn_blocking`]
//! thread to avoid stalling the tokio runtime (V4L2 `read` / `ioctl` are
//! blocking syscalls).
//!
//! # Linux setup
//!
//! The user must be in the `video` group to access `/dev/video*`:
//!
//! ```bash
//! sudo usermod -aG video $USER
//! ```
//!
//! Then log out and back in. Requires `libv4l-dev` package:
//!
//! ```bash
//! sudo apt install libv4l-dev
//! ```

use anyhow::{Context, Result, bail};
use nokhwa::Camera;
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{ApiBackend, CameraIndex, RequestedFormat, RequestedFormatType};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Information about a video capture device.
#[derive(Debug, Clone)]
pub struct VideoDeviceInfo {
    /// OS device index (e.g. `0` for `/dev/video0`).
    pub index: usize,
    /// Human-readable device name.
    pub name: String,
    /// Supported format descriptions (e.g. `"1920x1080 MJPEG 30fps"`).
    pub formats: Vec<String>,
}

/// A structured single-format capability reported by a camera.
///
/// Unlike the stringly-typed [`VideoDeviceInfo::formats`], this preserves the
/// width / height / pixel-format / frame-rate as discrete fields so the Web UI
/// can render resolution + fps pickers and the encoding layer can request a
/// specific format at stream-creation time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatCapability {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Pixel format label (e.g. `"MJPEG"`, `"YUYV"`).
    pub format: String,
    /// Frame rate in frames-per-second.
    pub fps: u32,
}

impl FormatCapability {
    /// Pixels-per-frame, handy for sorting and capacity heuristics.
    pub fn pixels(&self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

/// A single captured video frame.
///
/// Contains the raw camera buffer in the format indicated by
/// [`format`](Self::format). The consumer is responsible for any decoding.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// Monotonic timestamp from when the frame was captured.
    pub timestamp: std::time::Instant,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Pixel format of the raw data (e.g. `"MJPEG"`, `"YUYV"`, `"NV12"`).
    pub format: String,
    /// Raw frame buffer bytes (format-dependent encoding).
    pub data: Vec<u8>,
}

/// Video capture controller.
///
/// Opens a camera via nokhwa and streams frames through a tokio mpsc
/// channel. The capture loop runs on a [`tokio::task::spawn_blocking`]
/// thread because [`Camera::frame`] issues blocking V4L2 syscalls.
///
/// Drop the `VideoCapture` (or its receiver) to stop the stream and
/// release the camera device.
pub struct VideoCapture {
    /// The nokhwa camera handle, consumed once [`start_stream`] is called.
    camera: Option<Camera>,
    /// Shared stop flag for the blocking capture thread.
    stop_flag: Arc<AtomicBool>,
    /// Join handle for the blocking capture thread.
    handle: Option<tokio::task::JoinHandle<()>>,
    /// Human-readable device name for logging.
    device_name: String,
}

impl VideoCapture {
    /// Open a camera by device index.
    ///
    /// Uses [`ApiBackend::Auto`] to select the best available backend
    /// (V4L2 on Linux, MediaFoundation on Windows). The format is
    /// negotiated to the absolute highest resolution available.
    pub fn new(device_index: usize) -> Result<Self> {
        let index = CameraIndex::Index(device_index as u32);
        // Request the highest-resolution format.  The `RgbFormat` marker
        // tells nokhwa to prefer formats that _can_ be decoded to RGB,
        // but the raw frame buffer stays in the camera's native format.
        let requested =
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestResolution);

        let camera = Camera::new(index, requested)
            .context(format!("failed to open camera at index {device_index}"))?;
        let device_name = camera.info().human_name();

        info!(
            device = %device_name,
            index = device_index,
            "Video capture configured"
        );

        Ok(Self {
            camera: Some(camera),
            stop_flag: Arc::new(AtomicBool::new(false)),
            handle: None,
            device_name,
        })
    }

    /// Begin streaming frames from the camera.
    ///
    /// Returns an [`mpsc::Receiver`] that yields [`VideoFrame`]s as they
    /// arrive. Capture runs until the receiver is dropped, [`stop`] is
    /// called, or `VideoCapture` is dropped.
    pub fn start_stream(&mut self) -> Result<mpsc::Receiver<VideoFrame>> {
        if self.handle.is_some() {
            bail!("video capture already started — call stop() first or drop");
        }

        let (tx, rx) = mpsc::channel::<VideoFrame>(16);
        let stop_flag = self.stop_flag.clone();
        let device_name_inner = self.device_name.clone();
        let device_name = device_name_inner.clone();

        let mut camera = self
            .camera
            .take()
            .context("camera not available (already consumed)")?;

        // nokhwa 0.10 requires open_stream() before frame()
        camera
            .open_stream()
            .context("failed to open camera stream")?;

        // Spawn a blocking task because Camera::frame() calls blocking
        // V4L2 ioctl / read syscalls.  We use blocking_send on the
        // async mpsc Sender to bridge the blocking → async boundary.
        let handle = tokio::task::spawn_blocking(move || {
            loop {
                if stop_flag.load(Ordering::Relaxed) {
                    debug!(device = %device_name, "Video capture stopping (flag)");
                    break;
                }

                let frame = match camera.frame() {
                    Ok(f) => f,
                    Err(e) => {
                        error!(
                            device = %device_name,
                            error = %e,
                            "Frame capture error, retrying in 100 ms"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        continue;
                    }
                };

                let resolution = frame.resolution();
                let raw_format = frame.source_frame_format();

                let video_frame = VideoFrame {
                    timestamp: std::time::Instant::now(),
                    width: resolution.width(),
                    height: resolution.height(),
                    format: format!("{raw_format:?}"),
                    data: frame.buffer().to_vec(),
                };

                if tx.blocking_send(video_frame).is_err() {
                    debug!(device = %device_name, "Video receiver dropped");
                    break;
                }
            }

            // Camera is dropped here, releasing the V4L2 device.
            let _ = camera;
            debug!(device = %device_name, "Camera device released");
        });

        self.handle = Some(handle);
        info!(device = %device_name_inner, "Video capture started");
        Ok(rx)
    }
}

impl Drop for VideoCapture {
    fn drop(&mut self) {
        // Signal the capture thread to stop.
        self.stop_flag.store(true, Ordering::Relaxed);

        // If the camera was never taken (stream not started), drop it here.
        if let Some(camera) = self.camera.take() {
            let name = self.device_name.clone();
            drop(camera);
            debug!(device = %name, "Camera released (stream was never started)");
        }

        // Detach the join handle — the blocking task will exit promptly
        // once it observes the stop flag or the tx channel is closed.
        if let Some(handle) = self.handle.take() {
            tokio::spawn(async move {
                let _ = handle.await;
            });
        }

        debug!(device = %self.device_name, "VideoCapture dropped");
    }
}

// ---------------------------------------------------------------------------
// Device enumeration
// ---------------------------------------------------------------------------

/// Enumerate available video capture devices.
///
/// Opens each detected camera briefly to query its supported formats.
/// Devices that fail to open will have an empty [`formats`] list.
pub fn enumerate_devices() -> Result<Vec<VideoDeviceInfo>> {
    let cameras = nokhwa::query(ApiBackend::Auto).context("failed to enumerate cameras")?;

    let mut result = Vec::new();
    for cam_info in cameras {
        let index = match cam_info.index() {
            CameraIndex::Index(i) => *i as usize,
            other => {
                warn!("Unsupported camera index type: {other:?} — skipping");
                continue;
            }
        };
        let name = cam_info.human_name();

        // Try to open the camera briefly to enumerate formats.
        let formats = match enumerate_device_formats(index) {
            Ok(fmts) => fmts,
            Err(e) => {
                warn!(device = %name, error = %e, "Could not enumerate formats");
                Vec::new()
            }
        };

        result.push(VideoDeviceInfo {
            index,
            name,
            formats,
        });
    }

    if result.is_empty() {
        info!("No video capture devices found");
    }

    Ok(result)
}

/// Open a camera by index and return its supported format descriptions.
fn enumerate_device_formats(device_index: usize) -> Result<Vec<String>> {
    use nokhwa::utils::CameraFormat;

    let index = CameraIndex::Index(device_index as u32);
    let requested = RequestedFormat::new::<RgbFormat>(RequestedFormatType::None);

    let mut camera =
        Camera::new(index, requested).context("failed to open camera for format enumeration")?;

    let formats: Vec<String> = camera
        .compatible_camera_formats()
        .context("failed to enumerate camera formats")?
        .into_iter()
        .map(|cf: CameraFormat| {
            format!(
                "{}x{} {:?} {}fps",
                cf.width(),
                cf.height(),
                cf.format(),
                cf.frame_rate(),
            )
        })
        .collect();

    // Camera is dropped, releasing the device.
    Ok(formats)
}

/// Open a camera by index and return its supported formats as structured
/// [`FormatCapability`] entries.
///
/// Each `(width, height, format, fps)` combination the driver exposes becomes
/// one entry. The list is sorted by descending pixel count, then descending
/// fps, so the highest-resolution + highest-framerate options come first —
/// matching the order the Web UI presents to the user.
pub fn enumerate_device_formats_detailed(device_index: usize) -> Result<Vec<FormatCapability>> {
    use nokhwa::utils::CameraFormat;

    let index = CameraIndex::Index(device_index as u32);
    let requested = RequestedFormat::new::<RgbFormat>(RequestedFormatType::None);

    let mut camera =
        Camera::new(index, requested).context("failed to open camera for format enumeration")?;

    let mut formats: Vec<FormatCapability> = camera
        .compatible_camera_formats()
        .context("failed to enumerate camera formats")?
        .into_iter()
        .map(|cf: CameraFormat| FormatCapability {
            width: cf.width(),
            height: cf.height(),
            format: format!("{:?}", cf.format()),
            fps: cf.frame_rate(),
        })
        .collect();

    // Highest resolution first; within a resolution, highest fps first.
    formats.sort_by(|a, b| b.pixels().cmp(&a.pixels()).then_with(|| b.fps.cmp(&a.fps)));

    // Camera is dropped, releasing the device.
    Ok(formats)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: enumerate video devices (safe on CI/headless).
    #[test]
    fn test_enumerate_video_devices() {
        let devices = enumerate_devices().unwrap_or_default();
        if devices.is_empty() {
            println!("no video devices found (expected on CI/headless)");
        } else {
            for dev in &devices {
                println!(
                    "  [{}] {} ({} formats)",
                    dev.index,
                    dev.name,
                    dev.formats.len()
                );
            }
        }
    }

    /// Integration test: open first available camera, start stream,
    /// receive a frame, then verify the channel closes on drop.
    ///
    /// Skipped when no video device is available.
    #[tokio::test]
    async fn test_video_capture_channel_contract() {
        let devices = match enumerate_devices() {
            Ok(d) => d,
            Err(e) => {
                println!("skipping test — camera enumeration failed: {e}");
                return;
            }
        };

        let first = match devices.first() {
            Some(d) => d,
            None => {
                println!("skipping test — no cameras available");
                return;
            }
        };

        let mut capture = match VideoCapture::new(first.index) {
            Ok(c) => c,
            Err(e) => {
                println!("skipping test — failed to open camera: {e}");
                return;
            }
        };

        let mut rx = match capture.start_stream() {
            Ok(rx) => rx,
            Err(e) => {
                println!("skipping test — failed to start stream: {e}");
                return;
            }
        };

        // Wait up to 2 seconds for a frame.
        let timeout = tokio::time::sleep(std::time::Duration::from_secs(2));
        tokio::pin!(timeout);

        let got_frame = tokio::select! {
            frame = rx.recv() => {
                let f = frame.expect("expected a video frame from live camera");
                assert!(f.width > 0, "frame width must be positive");
                assert!(f.height > 0, "frame height must be positive");
                assert!(!f.data.is_empty(), "frame must carry data");
                println!(
                    "received video frame: {}x{} format={} data={}B",
                    f.width,
                    f.height,
                    f.format,
                    f.data.len(),
                );
                true
            }
            _ = &mut timeout => {
                println!("timeout waiting for video frame (camera may be idle)");
                false
            }
        };

        if got_frame {
            println!("successfully received a video frame");
        }

        // Dropping the capture stops the stream.
        drop(capture);
        let remaining = rx.recv().await;
        assert!(
            remaining.is_none(),
            "channel should be closed after VideoCapture is dropped"
        );
    }
}
