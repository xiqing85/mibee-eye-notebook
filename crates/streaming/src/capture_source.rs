//! Capture source adapters: local webcam/audio → encoded [`MediaFrame`] stream.
//!
//! This module bridges the [`capture`] crate (V4L2 webcam via nokhwa, ALSA
//! audio via cpal) with the streaming pipeline. Unlike the previous
//! ffmpeg-subprocess design, encoding now happens entirely in-process:
//!
//! ```text
//! Camera → VideoCapture (nokhwa) → VideoFrame{format,data}
//!       → encoder::convert (MJPEG/YUYV → YUV420p)
//!       → encoder::h264 (openh264) → Vec<NalUnit>
//!       → one MediaFrame::Video per NAL (start code stripped)
//!
//! Microphone → AudioCapture (cpal) → AudioFrame{i16 PCM}
//!           → encoder::audio (G.711 / AAC) → MediaFrame::Audio
//! ```
//!
//! # Snapshot / preview JPEG tap
//!
//! [`VideoCaptureSource`] also maintains a "latest JPEG" snapshot for the web
//! UI's snapshot and live-preview endpoints. When the camera delivers MJPG the
//! JPEG bytes are passed through untouched (zero-cost); for YUYV-only cameras
//! a JPEG is re-encoded periodically.

#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use crate::encoder::audio::{AudioEncoder, G711Encoder};
use crate::encoder::convert::{Yuv420p, mjpeg_to_yuv420p, yuyv_to_yuv420p};
use crate::encoder::h264::{H264Encoder, H264EncoderConfig, NalUnit};
use crate::capability::QualityPreset;
use crate::source::{MediaFrame, Source};
use capture::audio::{AudioCapture, AudioFrame};
use capture::video::{VideoCapture, VideoFrame};
use cpal::traits::{DeviceTrait, HostTrait};

/// Capacity of the JPEG preview broadcast (drop-oldest under backpressure).
const JPEG_BROADCAST_CAPACITY: usize = 8;
/// Re-encode a preview JPEG every Nth frame for YUYV cameras (to bound CPU).
const YUYV_PREVIEW_EVERY_N_FRAMES: u64 = 6;

// ---------------------------------------------------------------------------
// VideoCaptureSource
// ---------------------------------------------------------------------------

/// Bridge between a local camera and the streaming pipeline.
///
/// Opens a camera via [`VideoCapture`] (nokhwa/V4L2), encodes each frame to
/// H.264 via OpenH264, and emits individual NAL units as
/// [`MediaFrame::Video`]. The first NAL of each access unit has
/// `keyframe = true`.
///
/// A JPEG tap is maintained in parallel for the web UI: callers can obtain the
/// most recent JPEG via [`VideoCaptureSource::latest_jpeg`] or subscribe to a
/// stream of them via [`VideoCaptureSource::subscribe_jpeg`].
pub struct VideoCaptureSource {
    /// Camera device index (0 = `/dev/video0`).
    device_index: usize,
    /// Video capture controller, populated after `start()`.
    capture: Option<VideoCapture>,
    /// Frame channel receiver, created by [`VideoCapture::start_stream()`].
    frame_rx: Option<mpsc::Receiver<VideoFrame>>,
    /// H.264 encoder (OpenH264).
    encoder: Option<H264Encoder>,
    /// NALs produced by the last `encode()` call, drained one per `next_frame()`.
    pending_nals: std::collections::VecDeque<NalUnit>,
    /// Whether the source has been started.
    running: bool,
    /// Stream start time, for presentation timestamps.
    start_time: Option<Instant>,
    /// Negotiated frame width (for the encoder config + diagnostics).
    width: u32,
    /// Negotiated frame height.
    height: u32,
    /// Negotiated frame rate (Hz).
    fps: f32,
    /// Most recent JPEG frame for snapshot/preview subscribers.
    /// `Arc<[u8]>` so subscribers can share without copying.
    latest_jpeg: Arc<Mutex<Option<Arc<[u8]>>>>,
    /// Negotiated stream dimensions (width, height, fps), populated when the
    /// encoder is lazily initialised on the first frame. Shared so downstream
    /// consumers (e.g. FileOutput) can read the real resolution before they
    /// build their muxer track metadata.
    dimensions: Arc<Mutex<Option<StreamDimensions>>>,
    /// Requested capture frame rate (0.0 = fall back to 30.0). The actual
    /// delivered rate is bounded by the camera + USB bandwidth, but this value
    /// drives the encoder's `max_frame_rate` + GOP size so the keyframe cadence
    /// matches reality rather than a hardcoded 30 fps.
    target_fps: f32,
    /// Quality preset handed to the OpenH264 encoder. Defaults to
    /// [`QualityPreset::Medium`]; the Web UI will override this via
    /// [`VideoCaptureSource::with_quality_preset`].
    quality_preset: QualityPreset,
    /// Broadcast sender for live-preview subscribers.
    jpeg_tx: Option<broadcast::Sender<Arc<[u8]>>>,
    /// Frame counter, used to throttle YUYV→JPEG re-encoding.
    frame_count: u64,
}

/// Negotiated stream geometry, shared from the capture source to downstream
/// outputs so MP4 muxer metadata matches the actual encoded frames.
#[derive(Debug, Clone, Copy)]
pub struct StreamDimensions {
    pub width: u32,
    pub height: u32,
    pub fps: f32,
}

impl VideoCaptureSource {
    /// Create a new video capture source for the given camera device.
    ///
    /// `device_index` is the OS device index (e.g. `0` for `/dev/video0`).
    /// The camera is not opened until [`start()`](Self::start) is called.
    ///
    /// The JPEG-tap channels (latest-JPEG snapshot + preview broadcast) are
    /// created at construction time so callers can obtain subscription handles
    /// before the source has been started.
    #[tracing::instrument(skip_all, fields(device_index))]
    pub fn new(device_index: usize) -> Self {
        let (jpeg_tx, _) = broadcast::channel::<Arc<[u8]>>(JPEG_BROADCAST_CAPACITY);
        Self {
            device_index,
            capture: None,
            frame_rx: None,
            encoder: None,
            pending_nals: Default::default(),
            running: false,
            start_time: None,
            width: 0,
            height: 0,
            fps: 0.0,
            latest_jpeg: Arc::new(Mutex::new(None)),
            dimensions: Arc::new(Mutex::new(None)),
            target_fps: 0.0,
            quality_preset: QualityPreset::Medium,
            jpeg_tx: Some(jpeg_tx),
            frame_count: 0,
        }
    }

    /// Return whether the source has been started.
    #[tracing::instrument(skip_all)]
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Return the camera device index.
    #[tracing::instrument(skip_all)]
    pub fn device_index(&self) -> usize {
        self.device_index
    }

    /// Return a clone of the latest-JPEG handle.
    ///
    /// Callers (the web UI snapshot endpoint) read this to serve a single
    /// frame without joining the encode loop.
    pub fn latest_jpeg_handle(&self) -> Arc<Mutex<Option<Arc<[u8]>>>> {
        Arc::clone(&self.latest_jpeg)
    }

    /// Return the JPEG broadcast sender, if any subscribers should be created.
    ///
    /// Returns `None` before [`start()`](Self::start) has been called.
    pub fn jpeg_sender(&self) -> Option<broadcast::Sender<Arc<[u8]>>> {
        self.jpeg_tx.clone()
    }

    /// Return the most recent JPEG bytes, if available.
    pub fn latest_jpeg(&self) -> Option<Arc<[u8]>> {
        self.latest_jpeg.lock().clone()
    }

    /// Return a clone of the stream-dimensions handle.
    ///
    /// Populated with the negotiated `(width, height, fps)` once the encoder
    /// is lazily initialised on the first frame. Downstream outputs (e.g.
    /// [`FileOutput`](crate::output::FileOutput)) read this so MP4 track
    /// metadata matches the actual encoded frames instead of a hardcoded
    /// default.
    pub fn dimensions_handle(&self) -> Arc<Mutex<Option<StreamDimensions>>> {
        Arc::clone(&self.dimensions)
    }

    /// Override the encoder quality preset.
    ///
    /// Should be called before [`start`](Source::start). The preset selects the
    /// OpenH264 profile / complexity / rate-control / QP range. Defaults to
    /// [`QualityPreset::Medium`]; choose [`QualityPreset::UltraFast`] on weak
    /// CPUs and [`QualityPreset::High`] when image clarity matters most.
    pub fn with_quality_preset(mut self, preset: QualityPreset) -> Self {
        self.quality_preset = preset;
        self
    }

    /// Override the target frame rate used for encoder configuration.
    ///
    /// A value of `0.0` (the default) defers to 30 Hz. Set this to the actual
    /// capture rate (e.g. 10.0 for a USB-2 webcam at 720p) so the GOP size
    /// produces a one-second keyframe interval and the muxer's
    /// `max_frame_rate` matches reality.
    pub fn with_target_fps(mut self, fps: f32) -> Self {
        self.target_fps = fps;
        self
    }
}

impl Source for VideoCaptureSource {
    #[tracing::instrument(skip_all)]
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let device_index = self.device_index;
        let latest_jpeg = Arc::clone(&self.latest_jpeg);

        Box::pin(async move {
            let device_path = format!("/dev/video{}", device_index);
            if !std::path::Path::new(&device_path).exists() {
                bail!("Video device {} not found", device_path);
            }

            // Open the camera via nokhwa. `VideoCapture::new` negotiates the
            // highest-resolution format; the actual fourcc is reported per
            // frame via `VideoFrame.format`.
            let mut capture = VideoCapture::new(device_index)
                .context(format!("failed to open camera at index {device_index}"))?;
            let rx = capture
                .start_stream()
                .context("failed to start video capture stream")?;

            // We don't know width/height/fps until the first frame arrives;
            // the encoder is created lazily on the first next_frame() call.
            self.capture = Some(capture);
            self.frame_rx = Some(rx);
            // jpeg_tx was created in new(); keep it so existing subscribers
            // continue to receive frames across a stop/start cycle.
            // Re-attach the latest_jpeg handle in case start() is called twice.
            self.latest_jpeg = latest_jpeg;
            self.pending_nals.clear();
            self.frame_count = 0;
            self.start_time = Some(Instant::now());
            self.running = true;

            info!(
                device = %device_path,
                "VideoCaptureSource started (native nokhwa + openh264 pipeline)"
            );
            Ok(())
        })
    }

    #[tracing::instrument(skip_all)]
    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>> {
        Box::pin(async move {
            if !self.running {
                bail!("VideoCaptureSource not started");
            }

            // 1. Drain any NALs left over from the previous encode() call.
            if let Some(nal) = self.pending_nals.pop_front() {
                let timestamp = self
                    .start_time
                    .map(|t| t.elapsed().as_millis() as u64)
                    .unwrap_or(0);
                return Ok(MediaFrame::Video {
                    keyframe: nal.is_keyframe,
                    data: nal.data,
                    timestamp,
                });
            }

            // 2-7: Read a camera frame, convert, encode, emit NALs.
            // Wrapped in a loop because openh264 occasionally returns an empty
            // bitstream (FrameType::Skip) for rate control — we skip that frame
            // and read the next one rather than killing the stream.
            loop {
                // 2. Receive the next raw camera frame.
                let video_frame = self
                    .frame_rx
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("frame receiver not available"))?
                    .recv()
                    .await
                    .ok_or_else(|| anyhow::anyhow!("video frame channel closed"))?;

                // 3. Lazily initialize the encoder on the first frame.
                if self.encoder.is_none() {
                    self.width = video_frame.width;
                    self.height = video_frame.height;
                    // Use the caller-requested target fps, falling back to 30
                    // when unset. This drives the GOP size (1-second keyframe
                    // interval) so e.g. a 10 fps USB-2 webcam gets a keyframe
                    // every 10 frames, not every 30.
                    self.fps = if self.target_fps > 0.0 {
                        self.target_fps
                    } else {
                        30.0
                    };
                    let config = H264EncoderConfig {
                        width: self.width,
                        height: self.height,
                        fps: self.fps,
                        bitrate_bps: bitrate_for_dimensions(self.width, self.height),
                        quality_preset: self.quality_preset,
                    };
                    self.encoder = Some(H264Encoder::new(config).context("encoder init failed")?);
                    // Publish the negotiated dimensions so downstream outputs
                    // (e.g. FileOutput) can build correct muxer track metadata.
                    *self.dimensions.lock() = Some(StreamDimensions {
                        width: self.width,
                        height: self.height,
                        fps: self.fps,
                    });
                    info!(
                        width = self.width,
                        height = self.height,
                        fps = self.fps,
                        preset = ?self.quality_preset,
                        format = %video_frame.format,
                        "encoder initialized for camera format"
                    );
                }

                // 4. Convert the raw frame to planar YUV420p.
                let yuv: Yuv420p = if video_frame.format.contains("MJPEG") {
                    mjpeg_to_yuv420p(&video_frame.data).context("MJPEG → YUV420p decode failed")?
                } else if video_frame.format.contains("YUYV") {
                    yuyv_to_yuv420p(&video_frame.data, video_frame.width, video_frame.height)
                        .context("YUYV → YUV420p conversion failed")?
                } else {
                    debug!(format = %video_frame.format, "unknown camera format, attempting MJPEG decode");
                    mjpeg_to_yuv420p(&video_frame.data).context("MJPEG → YUV420p decode failed")?
                };

                // 5. Update the JPEG tap.
                self.frame_count += 1;
                let jpeg_bytes: Option<Arc<[u8]>> = if video_frame.format.contains("MJPEG") {
                    Some(Arc::from(video_frame.data.as_slice()))
                } else if self.frame_count % YUYV_PREVIEW_EVERY_N_FRAMES == 0 {
                    jpeg_encode_yuv(&yuv).map(Arc::from)
                } else {
                    None
                };
                if let Some(jpeg) = jpeg_bytes {
                    *self.latest_jpeg.lock() = Some(Arc::clone(&jpeg));
                    if let Some(tx) = &self.jpeg_tx {
                        let _ = tx.send(jpeg);
                    }
                }

                // 6. Encode the YUV frame to H.264 NALs.
                let timestamp_ms = self
                    .start_time
                    .map(|t| t.elapsed().as_millis() as u64)
                    .unwrap_or(0);
                let encoder = self
                    .encoder
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("encoder not initialized"))?;
                let nals = encoder
                    .encode(&yuv, timestamp_ms)
                    .context("H.264 encode failed")?;

                // 7. Queue all NALs. OpenH264 may return an empty bitstream
                //    (FrameType::Skip) for rate control — that's normal; just
                //    `continue` the loop to read the next camera frame.
                for nal in nals {
                    self.pending_nals.push_back(nal);
                }
                if self.pending_nals.is_empty() {
                    continue;
                }

                let first = self
                    .pending_nals
                    .pop_front()
                    .expect("just checked non-empty");

                return Ok(MediaFrame::Video {
                    keyframe: first.is_keyframe,
                    data: first.data,
                    timestamp: timestamp_ms,
                });
            }
        })
    }

    #[tracing::instrument(skip_all)]
    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // Drop the capture — its Drop impl signals the stop flag and
            // releases the V4L2 device. The frame channel closes automatically.
            self.capture = None;
            self.frame_rx = None;
            self.encoder = None;
            self.pending_nals.clear();
            // Note: keep jpeg_tx alive so subscribers survive a restart cycle.
            *self.latest_jpeg.lock() = None;
            self.running = false;
            self.start_time = None;
            info!("VideoCaptureSource stopped");
            Ok(())
        })
    }
}

/// Pick a sane target bitrate for the given resolution.
///
/// Mirrors typical surveillance streaming defaults — biased toward low
/// latency over visual quality.
fn bitrate_for_dimensions(width: u32, height: u32) -> u32 {
    let pixels = (width as u64) * (height as u64);
    // ~0.1 bits per pixel per frame at 30fps → reasonable starting point.
    match pixels {
        p if p >= 1280 * 720 => 2_500_000,   // 720p+
        p if p >= 640 * 480 => 1_200_000,   // VGA
        p if p >= 320 * 240 => 400_000,     // QVGA
        _ => 150_000,
    }
}

/// Encode a YUV420p frame to baseline JPEG for the preview/snapshot path.
///
/// Used only for cameras that don't deliver MJPG (YUYV-only devices).
/// `jpeg_encoder::Encoder::encode` consumes `self` and writes to the writer
/// passed at construction without returning it, so we use a [`SharedBuffer`]
/// (writes go to an `Rc<RefCell<Vec<u8>>>` we can read back afterward).
fn jpeg_encode_yuv(yuv: &Yuv420p) -> Option<Vec<u8>> {
    use crate::encoder::convert::yuv420p_to_rgb8;
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;

    /// A writer that forwards into a shared `Vec`, so the encoded bytes can be
    /// recovered after `Encoder::encode` (which consumes the encoder) returns.
    struct SharedBuffer(Rc<RefCell<Vec<u8>>>);
    impl Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let rgb = yuv420p_to_rgb8(yuv);
    let buf = Rc::new(RefCell::new(Vec::with_capacity(rgb.len() / 4)));
    let encoder = jpeg_encoder::Encoder::new(SharedBuffer(Rc::clone(&buf)), 70);
    // jpeg-encoder takes u16 dimensions.
    let w: u16 = yuv.width.try_into().ok()?;
    let h: u16 = yuv.height.try_into().ok()?;
    encoder
        .encode(&rgb, w, h, jpeg_encoder::ColorType::Rgb)
        .ok()?;
    // Extract the accumulated JPEG bytes.
    let out = buf.borrow().clone();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// AudioCaptureSource
// ---------------------------------------------------------------------------

/// Bridge between a local audio input device and the streaming pipeline.
///
/// Opens a microphone via [`AudioCapture`] (cpal/ALSA), encodes the i16 PCM
/// samples to G.711 μ-law (default) or AAC (`aac` feature), and emits each
/// encoded chunk as [`MediaFrame::Audio`].
pub struct AudioCaptureSource {
    /// Optional device name override. If set, enumerate devices to find match.
    device_name: Option<String>,
    /// Sample rate in Hz (discovered from device config at start).
    sample_rate: u32,
    /// Number of audio channels (1 = mono, 2 = stereo).
    channels: u16,
    /// Audio capture controller, kept alive to hold the stream.
    capture: Option<AudioCapture>,
    /// Frame channel receiver, created by [`AudioCapture::start()`].
    frame_rx: Option<broadcast::Receiver<AudioFrame>>,
    /// Audio encoder (G.711 by default; AAC under `aac` feature).
    encoder: Option<Box<dyn AudioEncoder>>,
    /// Whether the source has been started.
    running: bool,
}

impl AudioCaptureSource {
    /// Create a new audio capture source using the default input device.
    #[tracing::instrument(skip_all)]
    pub fn new() -> Self {
        Self {
            device_name: None,
            sample_rate: 0,
            channels: 0,
            capture: None,
            frame_rx: None,
            encoder: None,
            running: false,
        }
    }

    /// Create a new audio capture source for a specific device by name.
    #[tracing::instrument(skip_all)]
    pub fn with_device(device_name: String) -> Self {
        Self {
            device_name: Some(device_name),
            sample_rate: 0,
            channels: 0,
            capture: None,
            frame_rx: None,
            encoder: None,
            running: false,
        }
    }

    /// Return whether the source has been started.
    #[tracing::instrument(skip_all)]
    pub fn is_running(&self) -> bool {
        self.running
    }
}

impl Default for AudioCaptureSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for AudioCaptureSource {
    #[tracing::instrument(skip_all)]
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let target_device = self.device_name.clone();

        Box::pin(async move {
            // 1. Open the audio device.
            let host = cpal::default_host();
            let device = if let Some(ref name) = target_device {
                let devices = host
                    .input_devices()
                    .context("failed to enumerate audio input devices")?;
                devices
                    .into_iter()
                    .find(|d: &cpal::Device| {
                        d.description()
                            .map(|desc| desc.name() == name.as_str())
                            .unwrap_or(false)
                    })
                    .ok_or_else(|| anyhow::anyhow!("audio device '{name}' not found"))?
            } else {
                host.default_input_device()
                    .ok_or_else(|| anyhow::anyhow!("no default audio input device available"))?
            };

            let config = device
                .default_input_config()
                .context("failed to get default audio input config")?;

            let sample_rate = config.sample_rate();
            let channels = config.channels();

            // 2. Start cpal capture.
            let mut capture =
                AudioCapture::new(&device, &config).context("failed to create AudioCapture")?;
            let mut rx = capture
                .start(&device, &config)
                .context("failed to start audio capture")?;

            // Bridge the mpsc receiver to a broadcast so multiple consumers work.
            let (b_tx, b_rx) = broadcast::channel::<AudioFrame>(120);
            tokio::spawn(async move {
                while let Some(frame) = rx.recv().await {
                    if b_tx.send(frame).is_err() {
                        break;
                    }
                }
            });

            // 3. Construct the encoder. Default to G.711 μ-law.
            //    (AAC requires the `aac` feature and explicit opt-in elsewhere.)
            let encoder: Box<dyn AudioEncoder> = Box::new(G711Encoder::mulaw(
                sample_rate,
                channels,
            ));

            self.sample_rate = sample_rate;
            self.channels = channels;
            self.capture = Some(capture);
            self.frame_rx = Some(b_rx);
            self.encoder = Some(encoder);
            self.running = true;

            info!(
                sample_rate,
                channels, codec = "G.711 μ-law", "AudioCaptureSource started"
            );
            Ok(())
        })
    }

    #[tracing::instrument(skip_all)]
    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>> {
        Box::pin(async move {
            if !self.running {
                bail!("AudioCaptureSource not started");
            }

            // Receive the next AudioFrame from the capture channel.
            let frame: AudioFrame = loop {
                match self
                    .frame_rx
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("frame receiver not available"))?
                    .recv()
                    .await
                {
                    Ok(frame) => break frame,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "audio frame buffer full, dropped oldest frame(s)");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        bail!("audio frame channel closed");
                    }
                }
            };

            let timestamp = frame.timestamp.elapsed().as_millis() as u64;

            // Encode the i16 PCM samples in-place.
            let encoder = self
                .encoder
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("audio encoder not available"))?;
            let encoded = encoder
                .encode(&frame.samples)
                .context("audio encode failed")?;

            Ok(MediaFrame::Audio {
                data: encoded,
                timestamp,
            })
        })
    }

    #[tracing::instrument(skip_all)]
    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // Drop capture → drops the cpal stream → releases the audio device.
            self.capture = None;
            self.frame_rx = None;
            self.encoder = None;
            self.running = false;
            info!("AudioCaptureSource stopped");
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Constructor tests ──────────────────────────────────────────────────

    #[test]
    fn test_video_capture_source_new() {
        let source = VideoCaptureSource::new(0);
        assert_eq!(source.device_index, 0);
        assert!(!source.running);
        assert!(source.capture.is_none());
        assert!(source.frame_rx.is_none());
        assert!(source.pending_nals.is_empty());
    }

    #[test]
    fn test_video_capture_source_new_with_nonzero_index() {
        let source = VideoCaptureSource::new(3);
        assert_eq!(source.device_index(), 3);
        assert!(!source.is_running());
    }

    #[test]
    fn test_capture_source_no_hardware() {
        let source = VideoCaptureSource::new(99);
        assert_eq!(source.device_index(), 99);
        assert!(!source.is_running());
        assert!(source.latest_jpeg().is_none());
    }

    #[test]
    fn test_bitrate_for_dimensions() {
        assert_eq!(bitrate_for_dimensions(1280, 720), 2_500_000);
        assert_eq!(bitrate_for_dimensions(640, 480), 1_200_000);
        assert_eq!(bitrate_for_dimensions(320, 240), 400_000);
        assert_eq!(bitrate_for_dimensions(160, 120), 150_000);
    }

    #[test]
    fn test_jpeg_encode_yuv_produces_valid_jpeg() {
        let mut yuv = Yuv420p::new(8, 8);
        // Fill with a simple pattern.
        for (i, b) in yuv.y_plane_mut().iter_mut().enumerate() {
            *b = (i * 10) as u8;
        }
        let jpeg = jpeg_encode_yuv(&yuv).expect("JPEG encode should succeed");
        // JPEG magic bytes.
        assert_eq!(jpeg[0..2], [0xFF, 0xD8]);
        assert_eq!(jpeg[jpeg.len() - 2..], [0xFF, 0xD9]);
    }

    // ── AudioCaptureSource tests ───────────────────────────────────────────

    #[test]
    fn test_audio_capture_source_new() {
        let source = AudioCaptureSource::new();
        assert!(source.device_name.is_none());
        assert!(!source.running);
        assert!(source.capture.is_none());
        assert!(source.frame_rx.is_none());
    }

    #[test]
    fn test_audio_capture_source_with_device() {
        let source = AudioCaptureSource::with_device("test-device".to_string());
        assert_eq!(source.device_name.as_deref(), Some("test-device"));
        assert!(!source.running);
    }

    #[test]
    fn test_audio_capture_source_default() {
        let source = AudioCaptureSource::default();
        assert!(source.device_name.is_none());
        assert!(!source.running);
    }
}
