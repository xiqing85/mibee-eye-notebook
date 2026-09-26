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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use crate::capability::QualityPreset;
use crate::encoder::audio::{AudioEncoder, G711Encoder};
use crate::encoder::convert::{Yuv420p, mjpeg_to_yuv420p, rotated_dims, yuyv_to_yuv420p};
use crate::encoder::h264::{H264Encoder, H264EncoderConfig, NalUnit};
use crate::source::{MediaFrame, Source};
use capture::audio::{AudioCapture, AudioFrame};
use capture::video::{VideoCapture, VideoFrame};
use cpal::traits::{DeviceTrait, HostTrait};

/// Capacity of the JPEG preview broadcast (drop-oldest under backpressure).
const JPEG_BROADCAST_CAPACITY: usize = 8;
/// Re-encode a preview JPEG every Nth frame for YUYV cameras (to bound CPU).
const JPEG_REENCODE_EVERY_N_FRAMES: u64 = 6;

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
/// Low-resolution bandwidth-saving substream settings (SPEC appendix
/// A #20): a second H.264 encoder session fed by the downscaled
/// (already rotated/flipped/watermarked) main frames.
#[derive(Debug, Clone)]
pub struct SubstreamSettings {
    pub width: u32,
    pub height: u32,
    /// Target frame rate; `0.0` follows the main encoder rate.
    pub fps: f32,
    pub bitrate_bps: u32,
}

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
    /// Substream (SPEC appendix A #20): settings + the second encoder
    /// session + its own frame broadcast (one NAL per message, mirroring
    /// the main pipeline's granularity). The sender survives stream
    /// restarts like `jpeg_tx`; the encoder re-inits per run.
    substream: Option<SubstreamSettings>,
    sub_encoder: Option<H264Encoder>,
    sub_frame_tx: Option<broadcast::Sender<Arc<crate::source::MediaFrame>>>,
    /// Emit every N-th main frame into the sub encoder (fps ratio).
    sub_frame_div: u32,
    sub_frame_count: u32,
    /// Frame counter, used to throttle YUYV→JPEG re-encoding.
    frame_count: u64,
    /// Device-level horizontal mirror, applied to each frame before encoding
    /// so every consumer (RTSP, MSE, recordings, snapshots) sees it.
    hflip: bool,
    /// Device-level vertical flip (upside-down mount compensation).
    vflip: bool,
    /// Device-level rotation in degrees clockwise (0|90|180|270, SPEC v1
    /// appendix A #19), baked into each frame before the flips — 90/270
    /// swap the effective stream dimensions. Boot-static per stream (the
    /// frontend cycles stop→start to apply a change, like flips).
    rotation: u32,
    /// Runtime mirror flags from the GB28181 DeviceConfig FrameMirror
    /// control (A.2.3.2.9), composed with the static `hflip`/`vflip`
    /// above — see [`effective_flips`].
    gb_flips: Option<Arc<Flips>>,
    /// Video watermark (SPEC v1 §5.2) burned into each frame after the
    /// DeviceControl IFrameCmd latch: the GB28181 control handler sets
    /// this; the encode loop consumes it (swap false) and forces the next
    /// encoded frame to an IDR via the OpenH264 encoder.
    force_idr: Option<Arc<AtomicBool>>,
    /// flips, before the JPEG tap and the encoder — same "baked into
    /// everything downstream" semantics.
    watermark: Option<crate::watermark::Watermark>,
}

/// Runtime mirror flags driven by the GB/T 28181 DeviceConfig FrameMirror
/// control (A.2.3.2.9): the platform sets the mode, every capture loop
/// reads it per frame. Atomic so the config handler writes race-free
/// against the capture threads (relaxed ordering — per-frame reads
/// tolerate tearing on the exact transition frame).
#[derive(Debug, Default)]
pub struct Flips {
    hflip: AtomicBool,
    vflip: AtomicBool,
}

impl Flips {
    #[must_use]
    pub fn new(hflip: bool, vflip: bool) -> Self {
        Self {
            hflip: AtomicBool::new(hflip),
            vflip: AtomicBool::new(vflip),
        }
    }

    /// Update both flags (absolute — mode semantics per A.2.1.22).
    pub fn set(&self, hflip: bool, vflip: bool) {
        self.hflip.store(hflip, Ordering::Relaxed);
        self.vflip.store(vflip, Ordering::Relaxed);
    }

    /// Current flags.
    #[must_use]
    pub fn load(&self) -> (bool, bool) {
        (
            self.hflip.load(Ordering::Relaxed),
            self.vflip.load(Ordering::Relaxed),
        )
    }
}

/// Compose the static mount-compensation flips with the platform's
/// runtime FrameMirror flags. XOR: two mirrors of the same axis cancel
/// (a platform mirror on top of an already-compensated mount restores
/// the platform's intended view), and `None` platform flags leave the
/// static flips untouched.
#[must_use]
pub fn effective_flips(static_hflip: bool, static_vflip: bool, gb: Option<&Flips>) -> (bool, bool) {
    match gb {
        None => (static_hflip, static_vflip),
        Some(f) => {
            let (gh, gv) = f.load();
            (static_hflip ^ gh, static_vflip ^ gv)
        }
    }
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
            substream: None,
            sub_encoder: None,
            sub_frame_tx: None,
            sub_frame_div: 1,
            sub_frame_count: 0,
            force_idr: None,
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
            hflip: false,
            vflip: false,
            rotation: 0,
            gb_flips: None,
            watermark: None,
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

    /// Enable the low-resolution substream (SPEC appendix A #20): a second
    /// encoder session at `settings` geometry, fed by the downscaled main
    /// frames. Applies on stream start.
    #[must_use]
    pub fn with_substream(mut self, settings: SubstreamSettings) -> Self {
        let (tx, _) = broadcast::channel::<Arc<crate::source::MediaFrame>>(64);
        self.sub_frame_tx = Some(tx);
        self.substream = Some(settings);
        self
    }

    /// The substream frame broadcast sender (one H.264 NAL per message).
    /// `None` when the substream is not enabled.
    #[must_use]
    pub fn sub_frames_sender(&self) -> Option<broadcast::Sender<Arc<crate::source::MediaFrame>>> {
        self.sub_frame_tx.clone()
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

    /// Share the DeviceControl IFrameCmd latch. One flag spans all
    /// cameras of this process; whichever camera encodes next consumes it.
    pub fn with_force_idr_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.force_idr = Some(flag);
        self
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

    /// Enable device-level flips (permanent, baked into the encoded stream
    /// and the snapshot JPEG tap). Should be called before
    /// [`start`](Source::start). Runtime platform mirrors
    /// ([`with_gb_flips`]) compose with these via XOR.
    pub fn with_flips(mut self, hflip: bool, vflip: bool) -> Self {
        self.hflip = hflip;
        self.vflip = vflip;
        self
    }

    /// Set the device-level rotation in degrees clockwise (0|90|180|270,
    /// SPEC v1 appendix A #19); other values normalize to 0 (validation
    /// rejects them upstream). Applied before the flips; 90/270 swap the
    /// encoder/dimensions geometry. Boot-static: a change requires a
    /// stream (re)start, like flips.
    pub fn with_rotation(mut self, degrees: u32) -> Self {
        self.rotation = match degrees {
            90 | 180 | 270 => degrees,
            _ => 0,
        };
        self
    }

    /// Share the GB/T 28181 DeviceConfig FrameMirror flags (A.2.3.2.9).
    /// One set spans all cameras of this process; the platform flips it
    /// at runtime and every capture loop composes it with its static
    /// mount-compensation flips (see [`effective_flips`]).
    pub fn with_gb_flips(mut self, flips: Arc<Flips>) -> Self {
        self.gb_flips = Some(flips);
        self
    }

    /// Attach a watermark renderer (permanent, burned into the encoded
    /// stream and the JPEG tap). Should be called before
    /// [`start`](Source::start).
    pub fn with_watermark(mut self, watermark: crate::watermark::Watermark) -> Self {
        self.watermark = Some(watermark);
        self
    }
}

/// Whether the JPEG tap may pass the camera's native MJPEG bytes through
/// untouched. Any pre-encode pixel modification (rotation, flips,
/// watermark) means the raw MJPEG would no longer match the encoded
/// orientation/content, so the tap must re-encode from the modified YUV
/// instead.
fn mjpeg_passthrough(
    mjpeg_native: bool,
    rotation: u32,
    hflip: bool,
    vflip: bool,
    watermark: bool,
) -> bool {
    mjpeg_native && rotation == 0 && !hflip && !vflip && !watermark
}

impl Source for VideoCaptureSource {
    #[tracing::instrument(skip_all)]
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let device_index = self.device_index;
        let latest_jpeg = Arc::clone(&self.latest_jpeg);

        Box::pin(async move {
            let device_path = format!("/dev/video{}", device_index); // hardcode-ok: V4L2 设备枚举惯例（逐 index 探测），非固定部署路径
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
                    // Post-rotation effective geometry (SPEC v1 appendix A
                    // #19): the encoder consumes rotated frames, so 90/270
                    // swap its configured dims and the published stream
                    // dimensions downstream outputs (FileOutput muxer) read.
                    let (eff_w, eff_h) = rotated_dims(self.width, self.height, self.rotation);
                    let config = H264EncoderConfig {
                        width: eff_w,
                        height: eff_h,
                        fps: self.fps,
                        bitrate_bps: bitrate_for_dimensions(eff_w, eff_h),
                        quality_preset: self.quality_preset,
                    };
                    self.encoder = Some(H264Encoder::new(config).context("encoder init failed")?);
                    // Publish the negotiated dimensions so downstream outputs
                    // (e.g. FileOutput) can build correct muxer track metadata.
                    *self.dimensions.lock() = Some(StreamDimensions {
                        width: eff_w,
                        height: eff_h,
                        fps: self.fps,
                    });
                    info!(
                        width = eff_w,
                        height = eff_h,
                        fps = self.fps,
                        preset = ?self.quality_preset,
                        format = %video_frame.format,
                        "encoder initialized for camera format"
                    );
                }

                // 3b. DeviceControl IFrameCmd: consume a pending
                // force-keyframe request so the next encoded frame is an IDR.
                if let Some(flag) = &self.force_idr
                    && flag.swap(false, std::sync::atomic::Ordering::SeqCst)
                    && let Some(enc) = self.encoder.as_mut()
                {
                    enc.force_keyframe();
                    tracing::info!("DeviceControl IFrameCmd: next frame forced to IDR");
                }

                // 4. Convert the raw frame to planar YUV420p.
                let mut yuv: Yuv420p = if video_frame.format.contains("MJPEG") {
                    mjpeg_to_yuv420p(&video_frame.data).context("MJPEG → YUV420p decode failed")?
                } else if video_frame.format.contains("YUYV") {
                    yuyv_to_yuv420p(&video_frame.data, video_frame.width, video_frame.height)
                        .context("YUYV → YUV420p conversion failed")?
                } else {
                    debug!(format = %video_frame.format, "unknown camera format, attempting MJPEG decode");
                    mjpeg_to_yuv420p(&video_frame.data).context("MJPEG → YUV420p decode failed")?
                };

                // Device-level transform (SPEC v1 appendix A #9/#19):
                // rotation first, then flips — baked into everything
                // downstream (the encoder → RTSP/MSE/recordings, and the
                // JPEG tap alike). Static mount compensation XOR the
                // platform's runtime FrameMirror flags; 180° rotation
                // folds into the flip flags (same group element as
                // hflip+vflip), 90°/270° transpose the frame.
                let (hflip, vflip) =
                    effective_flips(self.hflip, self.vflip, self.gb_flips.as_deref());
                let (hflip, vflip) = if self.rotation == 180 {
                    (!hflip, !vflip)
                } else {
                    (hflip, vflip)
                };
                if self.rotation == 90 || self.rotation == 270 {
                    yuv = yuv.rotated(self.rotation == 90);
                }
                if hflip || vflip {
                    yuv.flip(hflip, vflip);
                }

                // Watermark (SPEC v1 §5.2): burned in after the flips,
                // before the JPEG tap and the encoder.
                if let Some(watermark) = &mut self.watermark {
                    watermark.render_into(&mut yuv);
                }

                // 4b. Substream (SPEC appendix A #20): second encoder session
                //     at reduced geometry over the downscaled — already
                //     transformed — frame. Fail-open: an encoder init
                //     failure disables the substream for this run, never
                //     the main pipeline.
                if let Some(settings) = self.substream.clone() {
                    let timestamp_ms = self
                        .start_time
                        .map(|t| t.elapsed().as_millis() as u64)
                        .unwrap_or(0);
                    if self.sub_encoder.is_none() {
                        let sub_fps = if settings.fps > 0.0 {
                            settings.fps
                        } else {
                            self.fps
                        };
                        let config = H264EncoderConfig {
                            width: settings.width,
                            height: settings.height,
                            fps: sub_fps,
                            bitrate_bps: settings.bitrate_bps,
                            quality_preset: self.quality_preset,
                        };
                        match H264Encoder::new(config) {
                            Ok(enc) => {
                                self.sub_frame_div = ((self.fps / sub_fps).round() as u32).max(1);
                                info!(
                                    width = settings.width,
                                    height = settings.height,
                                    fps = sub_fps,
                                    bitrate_bps = settings.bitrate_bps,
                                    "substream encoder initialized"
                                );
                                self.sub_encoder = Some(enc);
                            }
                            Err(e) => {
                                warn!(error = %e, "substream encoder init failed; substream disabled for this run");
                                self.substream = None;
                            }
                        }
                    }
                    if let Some(enc) = self.sub_encoder.as_mut() {
                        self.sub_frame_count = self.sub_frame_count.wrapping_add(1);
                        if self.sub_frame_div <= 1
                            || self.sub_frame_count.is_multiple_of(self.sub_frame_div)
                        {
                            let sub_yuv = yuv.downscaled(settings.width, settings.height);
                            if let Ok(nals) = enc.encode(&sub_yuv, timestamp_ms)
                                && let Some(tx) = &self.sub_frame_tx
                            {
                                for nal in nals {
                                    let _ = tx.send(Arc::new(crate::source::MediaFrame::Video {
                                        keyframe: nal.is_keyframe,
                                        data: nal.data,
                                        timestamp: timestamp_ms,
                                    }));
                                }
                            }
                        }
                    }
                }

                // 5. Update the JPEG tap. With flips or a watermark active
                //    the raw MJPEG bytes would NOT match the encoded frame,
                //    so re-encode from the modified YUV (throttled) instead.
                self.frame_count += 1;
                let mjpeg_native = video_frame.format.contains("MJPEG");
                let jpeg_bytes: Option<Arc<[u8]>> = if mjpeg_passthrough(
                    mjpeg_native,
                    self.rotation,
                    hflip,
                    vflip,
                    self.watermark.is_some(),
                ) {
                    Some(Arc::from(video_frame.data.as_slice()))
                } else if self
                    .frame_count
                    .is_multiple_of(JPEG_REENCODE_EVERY_N_FRAMES)
                {
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
            self.sub_encoder = None;
            self.sub_frame_count = 0;
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
        p if p >= 1280 * 720 => 2_500_000, // 720p+
        p if p >= 640 * 480 => 1_200_000,  // VGA
        p if p >= 320 * 240 => 400_000,    // QVGA
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
    if out.is_empty() { None } else { Some(out) }
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
            let encoder: Box<dyn AudioEncoder> =
                Box::new(G711Encoder::mulaw(sample_rate, channels));

            self.sample_rate = sample_rate;
            self.channels = channels;
            self.capture = Some(capture);
            self.frame_rx = Some(b_rx);
            self.encoder = Some(encoder);
            self.running = true;

            info!(
                sample_rate,
                channels,
                codec = "G.711 μ-law",
                "AudioCaptureSource started"
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
    #[test]
    fn with_substream_installs_frame_broadcast() {
        let base = VideoCaptureSource::new(0);
        assert!(base.sub_frames_sender().is_none());
        let with_sub = base.with_substream(SubstreamSettings {
            width: 640,
            height: 360,
            fps: 0.0,
            bitrate_bps: 400_000,
        });
        let tx = with_sub.sub_frames_sender().expect("sender installed");
        let mut rx = tx.subscribe();
        let frame = crate::source::MediaFrame::Video {
            keyframe: true,
            data: vec![0x67],
            timestamp: 1,
        };
        std::sync::Arc::new(frame.clone());
        tx.send(std::sync::Arc::new(frame)).unwrap();
        assert!(rx.try_recv().is_ok());
        // The sender handle outlives the source (subscribers survive a
        // stream restart, mirroring jpeg_tx).
        drop(with_sub);
        assert!(
            tx.receiver_count() == 1,
            "the test subscriber is still attached"
        );
    }

    use super::*;

    #[test]
    fn mjpeg_passthrough_requires_pristine_pipeline_including_rotation() {
        // Native MJPEG with no pre-encode pixel edits passes through untouched.
        assert!(mjpeg_passthrough(true, 0, false, false, false));
        // Rotation, any flip or an active watermark forces the re-encode path.
        assert!(!mjpeg_passthrough(true, 90, false, false, false));
        assert!(!mjpeg_passthrough(true, 270, false, false, false));
        assert!(!mjpeg_passthrough(true, 0, true, false, false));
        assert!(!mjpeg_passthrough(true, 0, false, true, false));
        assert!(!mjpeg_passthrough(true, 0, false, false, true));
        // Non-MJPEG cameras always re-encode.
        assert!(!mjpeg_passthrough(false, 0, false, false, false));
    }

    #[test]
    fn with_rotation_normalizes_and_publishes_effective_dims() {
        // Out-of-enum values normalize to 0 (validation rejects upstream).
        let src = VideoCaptureSource::new(0).with_rotation(45);
        assert_eq!(src.rotation, 0);
        let src = VideoCaptureSource::new(0).with_rotation(90);
        assert_eq!(src.rotation, 90);
        assert_eq!(rotated_dims(1280, 720, src.rotation), (720, 1280));
    }

    // ── GB FrameMirror runtime flags ───────────────────────────────────────

    #[test]
    fn flips_store_and_load_roundtrip() {
        let flips = Flips::default();
        assert_eq!(flips.load(), (false, false));
        flips.set(true, false);
        assert_eq!(flips.load(), (true, false));
        flips.set(false, true);
        assert_eq!(flips.load(), (false, true));
        flips.set(true, true);
        assert_eq!(flips.load(), (true, true));
    }

    #[test]
    fn effective_flips_composes_by_xor() {
        let gb = Flips::new(false, false);
        // No platform flags set → static flips unchanged.
        assert_eq!(effective_flips(true, false, Some(&gb)), (true, false));
        // Platform mirror composes: same-axis mirrors cancel.
        gb.set(true, false);
        assert_eq!(effective_flips(true, false, Some(&gb)), (false, false));
        assert_eq!(effective_flips(false, false, Some(&gb)), (true, false));
        gb.set(true, true);
        assert_eq!(effective_flips(true, false, Some(&gb)), (false, true));
        // No shared handle at all → static flips pass through.
        assert_eq!(effective_flips(true, true, None), (true, true));
    }

    #[test]
    fn with_gb_flips_shares_the_handle() {
        let flips = Arc::new(Flips::new(false, false));
        let source = VideoCaptureSource::new(0).with_gb_flips(Arc::clone(&flips));
        assert_eq!(
            effective_flips(false, false, source.gb_flips.as_deref()),
            (false, false)
        );
        // A runtime platform update is visible through the source's handle.
        flips.set(false, true);
        assert_eq!(
            effective_flips(false, false, source.gb_flips.as_deref()),
            (false, true)
        );
    }

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

#[cfg(test)]
mod fold_repro_tests {
    use super::*;
    use crate::encoder::convert::Yuv420p;

    // Replicates the capture-loop transform sequence verbatim (lines ~505-517)
    // so the 180° fold is pinned by a direct frame-level test.
    fn loop_transform(
        yuv: &mut Yuv420p,
        rotation: u32,
        hflip: bool,
        vflip: bool,
        gb: Option<&Flips>,
    ) {
        let (hflip, vflip) = effective_flips(hflip, vflip, gb);
        let (hflip, vflip) = if rotation == 180 {
            (!hflip, !vflip)
        } else {
            (hflip, vflip)
        };
        if rotation == 90 || rotation == 270 {
            *yuv = yuv.rotated(rotation == 90);
        }
        if hflip || vflip {
            yuv.flip(hflip, vflip);
        }
    }

    fn gradient_frame() -> Yuv420p {
        let w = 8u32;
        let h = 6u32;
        let mut f = Yuv420p::new(w, h);
        for y in 0..h as usize {
            for x in 0..w as usize {
                f.data[y * w as usize + x] = (x * 30 + y * 4) as u8;
            }
        }
        let (cw, ch) = ((w / 2) as usize, (h / 2) as usize);
        for i in 0..cw * ch {
            f.data[(w * h) as usize + i] = (i * 7 % 256) as u8;
            f.data[(w * h) as usize + cw * ch + i] = (i * 13 % 256) as u8;
        }
        f
    }

    #[test]
    fn rotation_180_fold_reverses_each_plane() {
        let src = gradient_frame();
        let mut f = gradient_frame();
        loop_transform(&mut f, 180, false, false, None);
        let (w, h) = (8usize, 6usize);
        let (cw, ch) = (w / 2, h / 2);
        // Per-plane 2D 180° reversal: out[y][x] = src[h-1-y][w-1-x].
        for (off, pw, ph) in [(0, w, h), (w * h, cw, ch), (w * h + cw * ch, cw, ch)] {
            for y in 0..ph {
                for x in 0..pw {
                    assert_eq!(
                        f.data[off + y * pw + x],
                        src.data[off + (ph - 1 - y) * pw + (pw - 1 - x)],
                        "plane at offset {off} pixel ({x},{y})"
                    );
                }
            }
        }
    }

    #[test]
    fn rotation_0_no_transform() {
        let f = gradient_frame();
        let mut g = gradient_frame();
        loop_transform(&mut g, 0, false, false, None);
        assert_eq!(f.data, g.data);
    }
}
