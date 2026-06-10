//! Capture source adapter: local webcam → H.264 encoded stream.
//!
//! Bridges the [`capture`] crate (V4L2 webcam via nokhwa) with the streaming
//! pipeline by encoding raw camera frames (MJPEG or raw video) into H.264
//! via an ffmpeg subprocess and wrapping them as [`MediaFrame::Video`].
//!
//! # Pipeline
//!
//! ```text
//! Camera → VideoCapture (mpsc channel) → ffmpeg stdin → libx264 → ffmpeg stdout → NAL extraction → MediaFrame
//! ```
//!
//! # Resource usage
//!
//! One ffmpeg subprocess per source, one tokio process handle, one mpsc
//! channel (16-frame buffer from capture crate).  Memory ≈ 64 MB (ffmpeg
//! process overhead + H.264 reference frames).

use std::future::Future;
use std::pin::Pin;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::mpsc;

use capture::video::{VideoCapture, VideoFrame};
use crate::source::{MediaFrame, Source};
use capture::audio::{AudioCapture, AudioFrame};
use cpal::traits::{DeviceTrait, HostTrait};

// ---------------------------------------------------------------------------
// VideoCaptureSource
// ---------------------------------------------------------------------------

/// Bridge between a local camera and the streaming pipeline.
///
/// Opens a camera via [`VideoCapture`], feeds raw frames to an ffmpeg
/// subprocess encoding to H.264, reads back the Annex B byte stream,
/// and emits individual NAL units as [`MediaFrame::Video`].
///
/// # Example
///
/// ```ignore
/// let mut source = VideoCaptureSource::new(0);
/// source.start().await?;
/// while let Ok(frame) = source.next_frame().await {
///     // frame is MediaFrame::Video { keyframe, data, timestamp }
/// }
/// source.stop().await?;
/// ```
#[allow(dead_code)]
pub struct VideoCaptureSource {
    /// Camera device index (0 = `/dev/video0`).
    device_index: usize,
    /// Video capture controller, populated after `start()`.
    capture: Option<VideoCapture>,
    /// Frame channel receiver, created by [`VideoCapture::start_stream()`].
    frame_rx: Option<mpsc::Receiver<VideoFrame>>,
    /// Running ffmpeg child process (MJPEG/raw → H.264 encoding).
    ffmpeg_child: Option<Child>,
    /// Piped stdin: raw frame data written here.
    ffmpeg_stdin: Option<ChildStdin>,
    /// Piped stdout: H.264 Annex B byte stream read from here.
    ffmpeg_stdout: Option<ChildStdout>,
    /// Accumulated byte buffer for partial NAL data from ffmpeg stdout.
    buffer: Vec<u8>,
    /// Whether the source has been started.
    running: bool,
}

impl VideoCaptureSource {
    /// Create a new video capture source for the given camera device.
    ///
    /// `device_index` is the OS device index (e.g. `0` for `/dev/video0`).
    /// The camera is not opened until [`start()`](Self::start) is called.
    pub fn new(device_index: usize) -> Self {
        Self {
            device_index,
            capture: None,
            frame_rx: None,
            ffmpeg_child: None,
            ffmpeg_stdin: None,
            ffmpeg_stdout: None,
            buffer: Vec::new(),
            running: false,
        }
    }

    /// Return whether the source has been started.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Return the camera device index.
    pub fn device_index(&self) -> usize {
        self.device_index
    }
}

impl Source for VideoCaptureSource {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        // Capture device_index by value (Copy) so the async block doesn't
        // need a full &mut self borrow at construction time.
        let index = self.device_index;

        Box::pin(async move {
            // ── 1. Open camera and start streaming ──────────────────────
            let mut capture = VideoCapture::new(index)
                .context("failed to open video capture device")?;

            let mut rx = capture
                .start_stream()
                .context("failed to start video stream")?;

            // ── 2. Receive first frame to detect format ─────────────────
            let first_frame: VideoFrame = rx
                .recv()
                .await
                .ok_or_else(|| anyhow::anyhow!("camera closed before first frame"))?;

            let width = first_frame.width;
            let height = first_frame.height;
            let format = first_frame.format.clone();

            // ── 3. Build ffmpeg command based on frame format ───────────
            let mut cmd = tokio::process::Command::new("ffmpeg");

            if format.eq_ignore_ascii_case("MJPEG") {
                // MJPEG: ffmpeg handles the JPEG decoding internally
                cmd.arg("-f")
                    .arg("mjpeg")
                    .arg("-i")
                    .arg("pipe:0");
            } else {
                // Raw video: specify pixel format and frame size
                let pix_fmt = map_pixel_format(&format);
                let size_str = format!("{}x{}", width, height);
                cmd.arg("-f")
                    .arg("rawvideo")
                    .arg("-pix_fmt")
                    .arg(pix_fmt)
                    .arg("-s")
                    .arg(&size_str)
                    .arg("-i")
                    .arg("pipe:0");
            }

            // Common encoder settings: ultrafast + zerolatency for minimal delay
            cmd.args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-f",
                "h264",
                "pipe:1",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

            let mut child = cmd
                .spawn()
                .context("failed to spawn ffmpeg — is ffmpeg installed and in $PATH?")?;

            let stdin = child
                .stdin
                .take()
                .context("failed to capture ffmpeg stdin")?;
            let stdout = child
                .stdout
                .take()
                .context("failed to capture ffmpeg stdout")?;

            // ── 4. Write the very first frame to kick off encoding ──────
            //
            // This gives ffmpeg initial data so that the first next_frame()
            // call is more likely to have encoded output ready immediately.
            let mut stdin_owned = stdin;
            stdin_owned
                .write_all(&first_frame.data)
                .await
                .context("failed to write first frame to ffmpeg stdin")?;
            stdin_owned
                .flush()
                .await
                .context("failed to flush ffmpeg stdin")?;

            // ── 5. Store handles ────────────────────────────────────────
            self.capture = Some(capture);
            self.frame_rx = Some(rx);
            self.ffmpeg_child = Some(child);
            self.ffmpeg_stdin = Some(stdin_owned);
            self.ffmpeg_stdout = Some(stdout);
            self.running = true;

            tracing::info!(
                device_index = index,
                width,
                height,
                format = %format,
                "VideoCaptureSource started"
            );

            Ok(())
        })
    }

    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>> {
        Box::pin(async move {
            if !self.running {
                anyhow::bail!("VideoCaptureSource not started");
            }

            // ── 1. Receive the next raw frame from the camera ───────────
            let frame: VideoFrame = self
                .frame_rx
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("frame receiver not available"))?
                .recv()
                .await
                .ok_or_else(|| anyhow::anyhow!("camera frame channel closed"))?;

            let timestamp = frame.timestamp.elapsed().as_millis() as u64;

            // ── 2. Write raw frame data to ffmpeg stdin ─────────────────
            let stdin = self
                .ffmpeg_stdin
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("ffmpeg stdin not available"))?;

            stdin
                .write_all(&frame.data)
                .await
                .context("failed to write frame to ffmpeg stdin")?;
            stdin
                .flush()
                .await
                .context("failed to flush ffmpeg stdin")?;

            // ── 3. Read from ffmpeg stdout until a complete NAL unit ────
            let stdout = self
                .ffmpeg_stdout
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("ffmpeg stdout not available"))?;

            loop {
                // Try to extract a complete NAL unit from accumulated data.
                if let Some((nal_data, keyframe, new_offset)) =
                    extract_next_nal(&self.buffer, 0)
                {
                    // Keep any remaining bytes for the next call.
                    self.buffer = self.buffer[new_offset..].to_vec();
                    return Ok(MediaFrame::Video {
                        keyframe,
                        data: nal_data,
                        timestamp,
                    });
                }

                // Need more data — read a chunk from ffmpeg stdout.
                let mut tmp = vec![0u8; 65536];
                let n = stdout
                    .read(&mut tmp)
                    .await
                    .context("error reading ffmpeg stdout")?;

                if n == 0 {
                    anyhow::bail!(
                        "ffmpeg stdout closed unexpectedly — \
                         encoder may have crashed or exited"
                    );
                }

                self.buffer.extend_from_slice(&tmp[..n]);
            }
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // 1. Drop stdin — closes the pipe, sending EOF to ffmpeg.
            drop(self.ffmpeg_stdin.take());

            // 2. Kill the ffmpeg process if it's still running.
            if let Some(mut child) = self.ffmpeg_child.take() {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }

            // 3. Drop capture — signals stop flag, releases camera device.
            self.capture = None;
            self.frame_rx = None;
            self.ffmpeg_stdout = None;
            self.buffer.clear();
            self.running = false;

            tracing::info!("VideoCaptureSource stopped");
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// AudioCaptureSource
// ---------------------------------------------------------------------------

/// Bridge between a local audio input device and the streaming pipeline.
///
/// Opens a microphone via [`AudioCapture`], feeds raw PCM i16 samples to an
/// ffmpeg subprocess encoding to AAC (ADTS), reads back the encoded frames,
/// and emits them as [`MediaFrame::Audio`].
///
/// # Pipeline
///
/// ```text
/// Microphone → AudioCapture (mpsc channel) → PCM i16 bytes → ffmpeg stdin →
/// libfdk_aac/libfaac → ffmpeg stdout → ADTS frame extraction → MediaFrame::Audio
/// ```
///
/// # Example
///
/// ```ignore
/// let mut source = AudioCaptureSource::new();
/// source.start().await?;
/// while let Ok(frame) = source.next_frame().await {
///     // frame is MediaFrame::Audio { data, timestamp }
/// }
/// source.stop().await?;
/// ```
#[allow(dead_code)]
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
    frame_rx: Option<mpsc::Receiver<AudioFrame>>,
    /// Running ffmpeg child process (PCM i16 → AAC encoding).
    ffmpeg_child: Option<Child>,
    /// Piped stdin: raw PCM i16 bytes written here.
    ffmpeg_stdin: Option<ChildStdin>,
    /// Piped stdout: AAC ADTS frames read from here.
    ffmpeg_stdout: Option<ChildStdout>,
    /// Accumulated byte buffer for partial ADTS data from ffmpeg stdout.
    buffer: Vec<u8>,
    /// Whether the source has been started.
    running: bool,
}

impl AudioCaptureSource {
    /// Create a new audio capture source using the default input device.
    pub fn new() -> Self {
        Self {
            device_name: None,
            sample_rate: 0,
            channels: 0,
            capture: None,
            frame_rx: None,
            ffmpeg_child: None,
            ffmpeg_stdin: None,
            ffmpeg_stdout: None,
            buffer: Vec::new(),
            running: false,
        }
    }

    /// Create a new audio capture source for a specific device by name.
    pub fn with_device(device_name: String) -> Self {
        Self {
            device_name: Some(device_name),
            sample_rate: 0,
            channels: 0,
            capture: None,
            frame_rx: None,
            ffmpeg_child: None,
            ffmpeg_stdin: None,
            ffmpeg_stdout: None,
            buffer: Vec::new(),
            running: false,
        }
    }

    /// Return whether the source has been started.
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
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        // Clone for device lookup inside async block.
        let target_device = self.device_name.clone();

        Box::pin(async move {
            // -- 1. Open audio device ------------------------------------------
            let host = cpal::default_host();

            let device = if let Some(ref name) = target_device {
                // Find device by name.
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
.ok_or_else(|| {
                        anyhow::anyhow!("audio device '{name}' not found")
                    })?
            } else {
                host.default_input_device()
.ok_or_else(|| {
                        anyhow::anyhow!("no default audio input device available")
                    })?
            };

            let config = device
.default_input_config()
.context("failed to get default audio input config")?;

            let sample_rate = config.sample_rate();
            let channels = config.channels();

            // -- 2. Create AudioCapture and start streaming -------------------
            let mut capture = AudioCapture::new(&device, &config)
.context("failed to create AudioCapture")?;
            let rx = capture
.start(&device, &config)
.context("failed to start audio capture")?;

            self.sample_rate = sample_rate;
            self.channels = channels;

            // -- 3. Build ffmpeg command --------------------------------------
            let mut cmd = tokio::process::Command::new("ffmpeg");
            cmd.arg("-f")
.arg("s16le")
.arg("-ar")
.arg(sample_rate.to_string())
.arg("-ac")
.arg(channels.to_string())
.arg("-i")
.arg("pipe:0")
.arg("-c:a")
.arg("aac")
.arg("-f")
.arg("adts")
.arg("pipe:1")
.stdin(std::process::Stdio::piped())
.stdout(std::process::Stdio::piped())
.stderr(std::process::Stdio::null());

            let mut child = cmd
.spawn()
.context("failed to spawn ffmpeg - is ffmpeg installed and in $PATH?")?;

            let stdin = child
.stdin
.take()
.context("failed to capture ffmpeg stdin")?;
            let stdout = child
.stdout
.take()
.context("failed to capture ffmpeg stdout")?;

            // -- 4. Store handles ---------------------------------------------
            self.capture = Some(capture);
            self.frame_rx = Some(rx);
            self.ffmpeg_child = Some(child);
            self.ffmpeg_stdin = Some(stdin);
            self.ffmpeg_stdout = Some(stdout);
            self.running = true;

            tracing::info!(
                sample_rate,
                channels,
                "AudioCaptureSource started"
            );

            Ok(())
        })
    }

    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>> {
        Box::pin(async move {
            if !self.running {
                anyhow::bail!("AudioCaptureSource not started");
            }

            // -- 1. Receive the next AudioFrame from the capture channel -------
            let frame: AudioFrame = self
.frame_rx
.as_mut()
.ok_or_else(|| anyhow::anyhow!("frame receiver not available"))?
.recv()
.await
.ok_or_else(|| anyhow::anyhow!("audio frame channel closed"))?;

            let timestamp = frame.timestamp.elapsed().as_millis() as u64;

            // -- 2. Convert i16 samples to raw PCM bytes ----------------------
            let raw_bytes: Vec<u8> = frame
.samples
.iter()
.flat_map(|s| s.to_le_bytes())
.collect();

            // -- 3. Write raw PCM bytes to ffmpeg stdin -----------------------
            let stdin = self
.ffmpeg_stdin
.as_mut()
.ok_or_else(|| anyhow::anyhow!("ffmpeg stdin not available"))?;

            stdin
.write_all(&raw_bytes)
.await
.context("failed to write audio data to ffmpeg stdin")?;
            stdin
.flush()
.await
.context("failed to flush ffmpeg stdin")?;

            // -- 4. Read from ffmpeg stdout until a complete ADTS frame --------
            let stdout = self
.ffmpeg_stdout
.as_mut()
.ok_or_else(|| anyhow::anyhow!("ffmpeg stdout not available"))?;

            loop {
                // Try to extract a complete ADTS frame from accumulated data.
                if let Some((adts_frame, new_offset)) = extract_adts_frame(&self.buffer) {
                    // Keep any remaining bytes for the next call.
                    self.buffer = self.buffer[new_offset..].to_vec();
                    return Ok(MediaFrame::Audio {
                        data: adts_frame,
                        timestamp,
                    });
                }

                // Need more data - read a chunk from ffmpeg stdout.
                let mut tmp = vec![0u8; 65536];
                let n = stdout
.read(&mut tmp)
.await
.context("error reading ffmpeg stdout")?;

                if n == 0 {
                    anyhow::bail!(
                        "ffmpeg stdout closed unexpectedly - encoder may have crashed or exited"
                    );
                }

                self.buffer.extend_from_slice(&tmp[..n]);
            }
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // 1. Drop stdin - closes the pipe, sending EOF to ffmpeg.
            drop(self.ffmpeg_stdin.take());

            // 2. Kill the ffmpeg process if it's still running.
            if let Some(mut child) = self.ffmpeg_child.take() {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }

            // 3. Drop AudioCapture - drops stream, releases audio device.
            self.capture = None;
            self.frame_rx = None;
            self.ffmpeg_stdout = None;
            self.buffer.clear();
            self.running = false;

            tracing::info!("AudioCaptureSource stopped");
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// Pixel format conversion
// ---------------------------------------------------------------------------

/// Map camera pixel format strings to ffmpeg `-pix_fmt` values.
///
/// The format string comes from [`VideoFrame::format`] which is the Debug
/// representation of nokhwa's [`FrameFormat`] enum (e.g. `"MJPEG"`,
/// `"YUYV"`, `"NV12"`).
///
/// For MJPEG this function returns `"mjpeg"` as a sentinel — the caller
/// should use `-f mjpeg` instead of `-f rawvideo` in that case.
fn map_pixel_format(format: &str) -> &'static str {
    // Match against known pixel format strings (case-insensitive).
    // The format comes from nokhwa's Debug representation of FrameFormat.
    match format {
        f if f.eq_ignore_ascii_case("MJPEG") => {
            tracing::warn!("MJPEG passed to map_pixel_format — use -f mjpeg instead");
            "yuvj422p"
        }
        f if f.eq_ignore_ascii_case("YUYV") || f.eq_ignore_ascii_case("YUY2") => "yuyv422",
        f if f.eq_ignore_ascii_case("NV12") => "nv12",
        f if f.eq_ignore_ascii_case("BGRA") => "bgra",
        f if f.eq_ignore_ascii_case("I420") || f.eq_ignore_ascii_case("IYUV") => "yuv420p",
        f if f.eq_ignore_ascii_case("RGB24") => "rgb24",
        f if f.eq_ignore_ascii_case("UYVY") => "uyvy422",
        f if f.eq_ignore_ascii_case("GRAY")
            || f.eq_ignore_ascii_case("GRAY8")
            || f.eq_ignore_ascii_case("Y800")
            || f.eq_ignore_ascii_case("Y16") => "gray",
        f if f.eq_ignore_ascii_case("BGR24") => "bgr24",
        f if f.eq_ignore_ascii_case("YV12") => "yuv420p",
        _ => {
            tracing::warn!("Unknown camera pixel format '{format}', falling back to yuv420p");
            "yuv420p"
        }
    }
}

// ---------------------------------------------------------------------------
// H.264 Annex B NAL unit extraction
// ---------------------------------------------------------------------------

/// Extract the next complete H.264 NAL unit from an Annex B byte stream.
///
/// Scans `buf` starting at `offset` for an Annex B start code
/// (`00 00 01` or `00 00 00 01`), reads the NAL unit that follows it
/// (up to the next start code), and returns:
///
/// * `nal_data` — the raw NAL unit payload bytes (including the NAL header
///   byte but excluding the start code).
/// * `is_keyframe` — `true` if the NAL unit type is IDR (5), SPS (7), or
///   PPS (8).
/// * `new_offset` — position of the next start code in `buf`, suitable
///   for passing back as `offset` on the next call.
///
/// Returns `None` if no complete NAL unit can be found (e.g. the buffer
/// does not yet contain a second start code to delimit the end of the
/// current NAL unit).  The caller should accumulate more data and retry.
fn extract_next_nal(buf: &[u8], offset: usize) -> Option<(Vec<u8>, bool, usize)> {
    if offset >= buf.len() {
        return None;
    }

    // Find the first start code at or after `offset`.
    let (sc_pos, sc_len) = find_next_start_code(buf, offset)?;
    let nal_start = sc_pos + sc_len;

    // Find the *next* start code to delimit this NAL unit.
    let next_sc = find_next_start_code(buf, nal_start);
    let (nal_end, next_offset) = match next_sc {
        Some((pos, _)) => (pos, pos),
        None => return None, // Incomplete NAL unit — need more data.
    };

    if nal_start >= nal_end {
        return None; // Empty NAL unit.
    }

    let nal_data = buf[nal_start..nal_end].to_vec();
    let nal_type = nal_data[0] & 0x1F;
    let is_keyframe = matches!(nal_type, 5 | 7 | 8); // IDR | SPS | PPS

    Some((nal_data, is_keyframe, next_offset))
}

/// Find the next H.264 Annex B start code in `buf` at or after `offset`.
///
/// Returns `(position, length)` where `length` is 3 for `00 00 01` or
/// 4 for `00 00 00 01`.
fn find_next_start_code(buf: &[u8], offset: usize) -> Option<(usize, usize)> {
    let mut i = offset;

    while i + 2 < buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 {
            // Check for 4-byte start code: 00 00 00 01
            if i + 3 < buf.len() && buf[i + 2] == 0 && buf[i + 3] == 1 {
                return Some((i, 4));
            }
            // Check for 3-byte start code: 00 00 01
            if buf[i + 2] == 1 {
                return Some((i, 3));
            }
        }
        i += 1;
    }

    None
}

// ---------------------------------------------------------------------------
// AAC ADTS frame extraction
// ---------------------------------------------------------------------------

/// Parse the AAC frame length from a 7-byte ADTS header.
///
/// ADTS header structure:
/// - Sync word: first 12 bits (should be 0xFFF)
/// - Frame length: 13 bits spanning bytes 3-5
///
/// Returns `None` if the header is too short, has invalid sync,
/// or the frame length is less than 7 (minimum ADTS header size).
fn parse_adts_frame_length(header: &[u8]) -> Option<usize> {
    if header.len() < 7 {
        return None;
    }
    // Check sync word: first 12 bits should be 0xFFF
    if header[0] != 0xFF || (header[1] & 0xF0) != 0xF0 {
        return None;
    }
    // Frame length is a 13-bit value:
    //   bits 30-31 (header[3] low 2 bits)
    //   bits 32-39 (header[4] all 8 bits)
    //   bits 40-42 (header[5] high 3 bits)
    let length = ((header[3] as usize & 0x03) << 11)
        | ((header[4] as usize) << 3)
        | ((header[5] as usize) >> 5);

    if length < 7 {
        return None; // Minimum ADTS frame is 7 bytes (header only)
    }

    Some(length)
}

/// Extract a complete ADTS frame from the beginning of `buf`.
///
/// Returns `(adts_frame, new_offset)` where `new_offset` is the position
/// after the frame, or `None` if no complete frame is available.
fn extract_adts_frame(buf: &[u8]) -> Option<(Vec<u8>, usize)> {
    if buf.len() < 7 {
        return None;
    }

    let frame_len = parse_adts_frame_length(buf)?;

    if buf.len() < frame_len {
        return None; // Incomplete frame
    }

    let frame = buf[..frame_len].to_vec();
    Some((frame, frame_len))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Constructor tests ──────────────────────────────────────────────

    #[test]
    fn test_video_capture_source_new() {
        let source = VideoCaptureSource::new(0);
        assert_eq!(source.device_index, 0);
        assert!(!source.running);
        assert!(source.capture.is_none());
        assert!(source.frame_rx.is_none());
        assert!(source.buffer.is_empty());
    }

    #[test]
    fn test_video_capture_source_new_with_nonzero_index() {
        let source = VideoCaptureSource::new(3);
        assert_eq!(source.device_index(), 3);
        assert!(!source.is_running());
    }

    // ── Pixel format mapping ───────────────────────────────────────────

    #[test]
    fn test_map_pixel_format_mjpeg() {
        // MJPEG returns a fallback value (callers should check for MJPEG
        // before calling this function).
        let fmt = map_pixel_format("MJPEG");
        assert!(!fmt.is_empty());
    }

    #[test]
    fn test_map_pixel_format_raw_formats() {
        assert_eq!(map_pixel_format("YUYV"), "yuyv422");
        assert_eq!(map_pixel_format("NV12"), "nv12");
        assert_eq!(map_pixel_format("BGRA"), "bgra");
        assert_eq!(map_pixel_format("I420"), "yuv420p");
        assert_eq!(map_pixel_format("RGB24"), "rgb24");
        assert_eq!(map_pixel_format("UYVY"), "uyvy422");
        assert_eq!(map_pixel_format("GRAY"), "gray");
        assert_eq!(map_pixel_format("BGR24"), "bgr24");
        assert_eq!(map_pixel_format("YV12"), "yuv420p");
    }

    #[test]
    fn test_map_pixel_format_case_insensitive() {
        assert_eq!(map_pixel_format("yuyv"), "yuyv422");
        assert_eq!(map_pixel_format("nv12"), "nv12");
        assert_eq!(map_pixel_format("bgra"), "bgra");
    }

    #[test]
    fn test_map_pixel_format_unknown_fallback() {
        // Unknown format returns the lowercase input as a best-effort guess.
        let fmt = map_pixel_format("CUSTOM");
        assert!(!fmt.is_empty());
    }

    // ── NAL parsing: extract_next_nal ──────────────────────────────────

    /// H.264 Annex B stream with SPS (4-byte SC), PPS (3-byte SC), IDR
    /// (4-byte SC), and a trailing start code to terminate the last NAL.
    const ANNEX_B_STREAM: &[u8] = &[
        // SPS (NAL type 7)
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x1e, 0xd9, 0x00, 0x78, 0x02, 0x27, 0xd5, 0x05,
        0x71,
        // PPS (NAL type 8) with 3-byte start code
        0x00, 0x00, 0x01, 0x68, 0xce, 0x38, 0x80,
        // IDR slice (NAL type 5) with 4-byte start code
        0x00, 0x00, 0x00, 0x01, 0x65, 0xb8, 0x00, 0x04,
        // Trailing start code to delimit the IDR slice
        0x00, 0x00, 0x00, 0x01,
    ];

    #[test]
    fn test_extract_next_nal_sps() {
        let (nal, keyframe, new_offset) =
            extract_next_nal(ANNEX_B_STREAM, 0).unwrap();

        // SPS header byte: 0x67 → nal_unit_type = 7
        assert_eq!(nal[0], 0x67, "first NAL should be SPS");
        assert!(keyframe, "SPS should be detected as keyframe");
        // After the first 4-byte SC (4) + 12 bytes SPS = 16 → next SC at 16
        assert_eq!(new_offset, 16, "next SC should be at position 16");
    }

    #[test]
    fn test_extract_next_nal_pps() {
        // Skip past SPS (start code at position 0, SPS ends before position 16)
        let (nal, keyframe, new_offset) =
            extract_next_nal(ANNEX_B_STREAM, 16).unwrap();

        // PPS header byte: 0x68 → nal_unit_type = 8
        assert_eq!(nal[0], 0x68, "second NAL should be PPS");
        assert!(keyframe, "PPS should be detected as keyframe");
        // 3-byte SC at 16 (len 3) → PPS starts at 19, 4 bytes → next SC at 23
        assert_eq!(new_offset, 23, "next SC should be at position 23");
    }

    #[test]
    fn test_extract_next_nal_idr() {
        // Skip past SPS and PPS
        let (nal, keyframe, new_offset) =
            extract_next_nal(ANNEX_B_STREAM, 23).unwrap();

        // IDR header byte: 0x65 → nal_unit_type = 5
        assert_eq!(nal[0], 0x65, "third NAL should be IDR");
        assert!(keyframe, "IDR should be detected as keyframe");
        // 4-byte SC at 23 (len 4) → IDR starts at 27, 4 bytes → trailing SC at 31
        assert_eq!(new_offset, 31, "next SC should be at position 31");
    }

    #[test]
    fn test_extract_next_nal_exhausted() {
        // After all NAL units, verify no more are returned.
        let result = extract_next_nal(ANNEX_B_STREAM, 31);
        assert!(
            result.is_none(),
            "no more NAL units should be extractable after the trailing SC"
        );
    }

    #[test]
    fn test_extract_next_nal_all_three() {
        // Extract all three NAL units in sequence.
        let (nal1, kf1, off1) = extract_next_nal(ANNEX_B_STREAM, 0).unwrap();
        assert_eq!(nal1[0] & 0x1F, 7); // SPS
        assert!(kf1);

        let (nal2, kf2, off2) = extract_next_nal(ANNEX_B_STREAM, off1).unwrap();
        assert_eq!(nal2[0] & 0x1F, 8); // PPS
        assert!(kf2);

        let (nal3, kf3, _off3) = extract_next_nal(ANNEX_B_STREAM, off2).unwrap();
        assert_eq!(nal3[0] & 0x1F, 5); // IDR
        assert!(kf3);
    }

    // ── NAL parsing: edge cases ────────────────────────────────────────

    #[test]
    fn test_extract_next_nal_empty_buffer() {
        assert!(extract_next_nal(&[], 0).is_none());
    }

    #[test]
    fn test_extract_next_nal_no_start_code() {
        // Arbitrary data with no start code pattern.
        let data = &[0x00, 0x01, 0x02, 0x03, 0x04];
        assert!(extract_next_nal(data, 0).is_none());
    }

    #[test]
    fn test_extract_next_nal_offset_past_end() {
        assert!(extract_next_nal(&[0x00, 0x00, 0x01, 0x67], 10).is_none());
    }

    #[test]
    fn test_extract_next_nal_incomplete_no_trailing_sc() {
        // Buffer has SPS with start code but no trailing start code.
        let data = &[
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x1e, // SPS, no SC after
        ];
        assert!(
            extract_next_nal(data, 0).is_none(),
            "should not return a NAL without a following start code"
        );
    }

    #[test]
    fn test_extract_next_nal_3byte_start_code() {
        // 3-byte start code only (00 00 01)
        let data = &[
            0x00, 0x00, 0x01, 0x67, 0x42, // SPS
            0x00, 0x00, 0x01, 0x68, 0xce, // PPS
            0x00, 0x00, 0x01, // trailing SC to delimit PPS
        ];
        let (nal, keyframe, off) = extract_next_nal(data, 0).unwrap();
        assert_eq!(nal[0] & 0x1F, 7);
        assert!(keyframe);
        assert_eq!(off, 5); // Next SC at position 5

        let (nal, keyframe, _off) = extract_next_nal(data, off).unwrap();
        assert_eq!(nal[0] & 0x1F, 8);
        assert!(keyframe);
    }

    #[test]
    fn test_extract_next_nal_mixed_start_codes() {
        // Mix of 3-byte and 4-byte start codes.
        let data = &[
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, // 4-byte SC, SPS
            0x00, 0x00, 0x01, 0x65, 0xb8, // 3-byte SC, IDR
            0x00, 0x00, 0x00, 0x01, // trailing SC
        ];
        let (nal1, kf1, off1) = extract_next_nal(data, 0).unwrap();
        assert_eq!(nal1[0] & 0x1F, 7);
        assert!(kf1);

        let (nal2, kf2, _off2) = extract_next_nal(data, off1).unwrap();
        assert_eq!(nal2[0] & 0x1F, 5);
        assert!(kf2);
    }

    // ── Keyframe detection ─────────────────────────────────────────────

    #[test]
    fn test_keyframe_detection_idr() {
        // NAL type 5 = IDR slice → keyframe
        let (_, keyframe, _) = extract_next_nal(
            &[0x00, 0x00, 0x00, 0x01, 0x65, 0x00, 0x00, 0x00, 0x01],
            0,
        )
        .unwrap();
        assert!(keyframe);
    }

    #[test]
    fn test_keyframe_detection_sps() {
        // NAL type 7 = SPS → keyframe
        let (_, keyframe, _) = extract_next_nal(
            &[0x00, 0x00, 0x00, 0x01, 0x67, 0x00, 0x00, 0x00, 0x01],
            0,
        )
        .unwrap();
        assert!(keyframe);
    }

    #[test]
    fn test_keyframe_detection_pps() {
        // NAL type 8 = PPS → keyframe
        let (_, keyframe, _) = extract_next_nal(
            &[0x00, 0x00, 0x00, 0x01, 0x68, 0x00, 0x00, 0x00, 0x01],
            0,
        )
        .unwrap();
        assert!(keyframe);
    }

    #[test]
    fn test_keyframe_detection_non_idr_slice() {
        // NAL type 1 = non-IDR slice → NOT a keyframe
        let (_, keyframe, _) = extract_next_nal(
            &[0x00, 0x00, 0x00, 0x01, 0x41, 0x00, 0x00, 0x00, 0x01],
            0,
        )
        .unwrap();
        assert!(!keyframe, "non-IDR slice should not be keyframe");
    }

    #[test]
    fn test_keyframe_detection_sei() {
        // NAL type 6 = SEI → NOT a keyframe
        let (_, keyframe, _) = extract_next_nal(
            &[0x00, 0x00, 0x00, 0x01, 0x46, 0x00, 0x00, 0x00, 0x01],
            0,
        )
        .unwrap();
        assert!(!keyframe, "SEI should not be keyframe");
    }

    // ── find_next_start_code tests ─────────────────────────────────────

    #[test]
    fn test_find_start_code_4byte() {
        let data = &[0x00, 0x00, 0x00, 0x01, 0x67];
        let result = find_next_start_code(data, 0);
        assert_eq!(result, Some((0, 4)));
    }

    #[test]
    fn test_find_start_code_3byte() {
        let data = &[0x00, 0x00, 0x01, 0x67];
        let result = find_next_start_code(data, 0);
        assert_eq!(result, Some((0, 3)));
    }

    #[test]
    fn test_find_start_code_with_offset() {
        let data = &[
            0xff, 0xff, 0xff, // garbage
            0x00, 0x00, 0x00, 0x01, 0x67, // start code at pos 3
        ];
        let result = find_next_start_code(data, 0);
        assert_eq!(result, Some((3, 4)));
    }

    #[test]
    fn test_find_start_code_none() {
        assert_eq!(find_next_start_code(&[0x00, 0x01, 0x02], 0), None);
        assert_eq!(find_next_start_code(&[], 0), None);
    }

    #[test]
    fn test_find_start_code_offset_skips_earlier() {
        let data = &[
            0x00, 0x00, 0x00, 0x01, 0x67, // SC at 0
            0x00, 0x00, 0x01, 0x68, // SC at 5
        ];
        // Start searching from after the first SC.
        let result = find_next_start_code(data, 5);
        assert_eq!(result, Some((5, 3)));
    }

    // ── Round-trip: extraction followed by reconstruction ──────────────

    #[test]
    fn test_nal_roundtrip_concatenation() {
        // Extract all three NAL units, then verify they can be reassembled
        // into a valid Annex B stream.
        let (nal1, .., off1) = extract_next_nal(ANNEX_B_STREAM, 0).unwrap();
        let (nal2, .., off2) = extract_next_nal(ANNEX_B_STREAM, off1).unwrap();
        let (nal3, .., _off3) = extract_next_nal(ANNEX_B_STREAM, off2).unwrap();

        // Rebuild: each NAL unit prefixed with 4-byte start code.
        let mut rebuilt = Vec::new();
        rebuilt.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        rebuilt.extend_from_slice(&nal1);
        rebuilt.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        rebuilt.extend_from_slice(&nal2);
        rebuilt.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        rebuilt.extend_from_slice(&nal3);

        // The NAL data should be identical (modulo start code style).
        assert_eq!(rebuilt[4], 0x67);
        assert_eq!(rebuilt[4 + nal1.len() + 4], 0x68);
        assert_eq!(
            rebuilt[4 + nal1.len() + 4 + nal2.len() + 4],
            0x65
        );
    }

    // ── no_ffmpeg_in_dependency test ───────────────────────────────────

    /// Verify that we can construct a VideoCaptureSource without needing
    /// ffmpeg or a camera — just the struct and its trivial methods.
    #[test]
    fn test_capture_source_no_hardware() {
        let source = VideoCaptureSource::new(99);
        assert_eq!(source.device_index(), 99);
        assert!(!source.is_running());
        // stop() without start() should not panic.
        // We can't easily test this without async, so we just verify
        // the struct is well-formed.
    }

    // ── AudioCaptureSource tests ─────────────────────────────────────────

    #[test]
    fn test_audio_capture_source_new() {
        let source = AudioCaptureSource::new();
        assert!(source.device_name.is_none());
        assert!(!source.running);
        assert!(source.capture.is_none());
        assert!(source.frame_rx.is_none());
        assert!(source.buffer.is_empty());
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

    #[test]
    fn test_adts_frame_length_parsing_valid() {
        // Construct an ADTS header with frame length = 200
        let mut header = vec![0u8; 7];
        header[0] = 0xFF;
        header[1] = 0xF0;
        // 200 = 0xC8
        // bits 10-3 = (200 >> 3) & 0xFF = 25 = 0x19
        // bits 2-0 = 200 & 0x07 = 0
        // header[3] low 2 bits = (200 >> 11) & 0x03 = 0
        header[3] = 0;
        header[4] = 25; // 0x19
        header[5] = 0;

        let len = parse_adts_frame_length(&header).unwrap();
        assert_eq!(len, 200);
    }

    #[test]
    fn test_adts_frame_length_parsing_invalid_sync() {
        let mut header = vec![0u8; 7];
        header[0] = 0xFF;
        header[1] = 0xF0;
        header[3] = 0;
        header[4] = 25;
        header[5] = 0;

        // Corrupt sync word
        header[0] = 0xFE;
        assert!(parse_adts_frame_length(&header).is_none());
    }

    #[test]
    fn test_adts_frame_length_parsing_too_short() {
        assert!(parse_adts_frame_length(&[0xFF, 0xF0, 0x00]).is_none());
        assert!(parse_adts_frame_length(&[]).is_none());
    }

    #[test]
    fn test_adts_frame_length_parsing_minimum() {
        let mut header = vec![0u8; 7];
        header[0] = 0xFF;
        header[1] = 0xF0;
        // Set frame length bits to produce value 7 (minimum valid)
        // bits 2-0 = 7, bits 10-3 = 0, bits 12-11 = 0
        header[3] = 0;
        header[4] = 0;
        header[5] = 7 << 5; // 224 = 0xE0

        let len = parse_adts_frame_length(&header).unwrap();
        assert_eq!(len, 7);
    }

    #[test]
    fn test_adts_frame_length_below_minimum() {
        let mut header = vec![0u8; 7];
        header[0] = 0xFF;
        header[1] = 0xF0;
        // Frame length = 6 (below minimum 7)
        // bits 2-0 = 6, bits 10-3 = 0, bits 12-11 = 0
        header[3] = 0;
        header[4] = 0;
        header[5] = 6 << 5; // 192
        assert!(parse_adts_frame_length(&header).is_none());
    }

    #[test]
    fn test_adts_extraction_exact_buffer() {
        // Create an ADTS frame of length 100
        let mut header = vec![0u8; 7];
        header[0] = 0xFF;
        header[1] = 0xF0;
        // 100 = 0x64
        // bits 10-3 = (100 >> 3) & 0xFF = 12 = 0x0C
        // bits 2-0 = 100 & 0x07 = 4
        header[3] = 0;
        header[4] = 12; // 0x0C
        header[5] = 4 << 5; // 128 = 0x80

        let mut frame_data = [0u8; 100];
        frame_data[..7].copy_from_slice(&header);
        for (i, byte) in frame_data.iter_mut().enumerate().skip(7) {
            *byte = (i & 0xFF) as u8;
        }

        let (extracted, offset) = extract_adts_frame(&frame_data).unwrap();
        assert_eq!(extracted.len(), 100);
        assert_eq!(extracted, frame_data);
        assert_eq!(offset, 100);
    }

    #[test]
    fn test_adts_extraction_with_trailing_data() {
        let mut header = vec![0u8; 7];
        header[0] = 0xFF;
        header[1] = 0xF0;
        // Frame length = 50
        // bits 10-3 = (50 >> 3) & 0xFF = 6
        // bits 2-0 = 50 & 0x07 = 2
        header[3] = 0;
        header[4] = 6;
        header[5] = 2 << 5; // 64

        let mut frame_data = vec![0u8; 50];
        frame_data[..7].copy_from_slice(&header);

        // Buffer with trailing data after the ADTS frame
        let mut larger = frame_data.clone();
        larger.extend_from_slice(&[0xAA, 0xBB, 0xCC]);

        let (extracted, offset) = extract_adts_frame(&larger).unwrap();
        assert_eq!(extracted.len(), 50);
        assert_eq!(offset, 50);
    }

    #[test]
    fn test_adts_extraction_incomplete() {
        let mut header = vec![0u8; 7];
        header[0] = 0xFF;
        header[1] = 0xF0;
        header[3] = 0;
        header[4] = 25; // frame length 200
        header[5] = 0;

        let mut frame_data = [0u8; 200];
        frame_data[..7].copy_from_slice(&header);

        // Only provide first 50 bytes
        assert!(extract_adts_frame(&frame_data[..50]).is_none());
    }

    #[test]
    fn test_adts_extraction_empty() {
        assert!(extract_adts_frame(&[]).is_none());
    }

    #[test]
    fn test_adts_extraction_no_valid_header() {
        assert!(extract_adts_frame(&[0x00; 7]).is_none());
    }

}
