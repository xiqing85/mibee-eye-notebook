//! Audio capture module using cpal.
//!
//! Provides device enumeration, format negotiation, and async audio capture
//! via tokio mpsc channels. The cpal data callback uses `try_send` to forward
//! samples without blocking the real-time audio thread.
//!
//! Audio frames are normalized to `i16` samples regardless of the device's
//! native sample format (`f32`, `i16`, `u16`, etc.).

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, StreamConfig, SupportedStreamConfig};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Information about an audio input device.
#[derive(Debug, Clone)]
pub struct AudioDeviceInfo {
    /// Device name string.
    pub name: String,
    /// Supported input configurations (best-effort).
    pub supported_configs: Vec<AudioConfigInfo>,
}

/// A description of a supported audio configuration range.
#[derive(Debug, Clone)]
pub struct AudioConfigInfo {
    /// Number of channels (1 = mono, 2 = stereo).
    pub channels: u16,
    /// Minimum sample rate in Hz.
    pub min_sample_rate: f64,
    /// Maximum sample rate in Hz.
    pub max_sample_rate: f64,
    /// Debug description of the sample format (e.g. "F32", "I16").
    pub sample_format: String,
}

/// A captured audio frame with PCM `i16` samples.
///
/// All samples are converted to signed 16-bit linear PCM regardless of the
/// device's native format (f32 samples are scaled, u16 samples are centered).
#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// Monotonic timestamp from when the samples were received.
    pub timestamp: std::time::Instant,
    /// Sample rate in Hz (e.g. 44100.0, 48000.0).
    pub sample_rate: f64,
    /// Number of channels (1 = mono, 2 = stereo).
    pub channels: u16,
    /// Interleaved PCM `i16` samples.
    pub samples: Vec<i16>,
}

/// Audio capture controller.
///
/// Opens an audio input stream via cpal and forwards captured samples
/// through a tokio mpsc channel. The callback uses `try_send` to avoid
/// blocking the audio thread.
///
/// # Example
///
/// ```no_run
/// use capture::audio::AudioCapture;
/// use cpal::traits::{DeviceTrait, HostTrait};
///
/// # fn example() -> anyhow::Result<()> {
/// let host = cpal::default_host();
/// let device = host.default_input_device().unwrap();
/// let config = device.default_input_config().unwrap();
///
/// let mut capture = AudioCapture::new(&device, &config)?;
/// let mut rx = capture.start(&device, &config)?;
///
/// while let Some(frame) = rx.blocking_recv() {
///     println!("Audio: {} Hz, {} ch, {} samples",
///         frame.sample_rate, frame.channels, frame.samples.len());
/// }
/// # Ok(())
/// # }
/// ```
pub struct AudioCapture {
    /// The active cpal stream, kept alive while this struct exists.
    stream: Option<cpal::Stream>,
    /// Human-readable device name for logging.
    device_name: String,
}

impl AudioCapture {
    /// Prepare an audio capture session.
    ///
    /// Validates the device and config but does **not** start streaming yet.
    /// Call [`start`](Self::start) to begin capturing.
    pub fn new(device: &Device, config: &SupportedStreamConfig) -> Result<Self> {
        let device_name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "<unknown>".into());
        info!(
            device = %device_name,
            channels = config.channels(),
            sample_rate = %config.sample_rate(),
            format = ?config.sample_format(),
            "Audio capture configured"
        );
        Ok(Self {
            stream: None,
            device_name,
        })
    }

    /// Begin capturing audio from the given device using the provided config.
    ///
    /// Returns a `mpsc::Receiver` that yields [`AudioFrame`]s as they arrive
    /// from the audio callback. Capture runs until `AudioCapture` is dropped
    /// or the receiver is dropped.
    ///
    /// The callback uses `try_send` to avoid blocking the real-time audio thread.
    /// If the channel is full, the frame is dropped and a warning is logged.
    pub fn start(
        &mut self,
        device: &Device,
        config: &SupportedStreamConfig,
    ) -> Result<mpsc::Receiver<AudioFrame>> {
        let (tx, rx) = mpsc::channel::<AudioFrame>(256);
        let stream_config: StreamConfig = config.config();
        let channels = config.channels();
        let sample_rate = config.sample_rate() as f64;

        let sample_format = config.sample_format();
        let stream = match sample_format {
            SampleFormat::F32 => {
                self::build_f32_stream(device, stream_config, channels, sample_rate, tx)?
            }
            SampleFormat::I16 => {
                self::build_i16_stream(device, stream_config, channels, sample_rate, tx)?
            }
            SampleFormat::U16 => {
                self::build_u16_stream(device, stream_config, channels, sample_rate, tx)?
            }
            other => bail!("unsupported audio sample format: {other:?}"),
        };

        stream.play().context("failed to start audio stream")?;

        info!(
            device = %self.device_name,
            channels,
            sample_rate,
            format = ?sample_format,
            "Audio capture started"
        );

        self.stream = Some(stream);
        Ok(rx)
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            drop(stream);
            debug!(device = %self.device_name, "Audio stream stopped");
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers: one builder per sample format
// ---------------------------------------------------------------------------

/// Build an f32 input stream, convert to i16 samples.
fn build_f32_stream(
    device: &Device,
    config: StreamConfig,
    channels: u16,
    sample_rate: f64,
    tx: mpsc::Sender<AudioFrame>,
) -> Result<cpal::Stream> {
    let tx_clone = tx.clone();
    let stream = device
        .build_input_stream(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let samples: Vec<i16> = data
                    .iter()
                    .map(|&s| {
                        // Clamp to [-1.0, 1.0] then scale to i16 range
                        let clamped = s.clamp(-1.0, 1.0);
                        (clamped * i16::MAX as f32) as i16
                    })
                    .collect();
                let db_level = compute_rms_db(&samples);
                let frame = AudioFrame {
                    timestamp: std::time::Instant::now(),
                    sample_rate,
                    channels,
                    samples,
                };
                observability::metrics::set_audio_level("default", db_level);
                if let Err(e) = tx_clone.try_send(frame)
                    && matches!(e, tokio::sync::mpsc::error::TrySendError::Full(_))
                {
                    warn!("Audio channel full, dropping f32 frame");
                }
            },
            move |err| {
                error!("Audio capture error (f32): {err}");
            },
            None,
        )
        .context("failed to build f32 input stream")?;
    Ok(stream)
}

/// Build an i16 input stream, forward samples as-is.
fn build_i16_stream(
    device: &Device,
    config: StreamConfig,
    channels: u16,
    sample_rate: f64,
    tx: mpsc::Sender<AudioFrame>,
) -> Result<cpal::Stream> {
    let tx_clone = tx.clone();
    let stream = device
        .build_input_stream(
            config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let db_level = compute_rms_db(data);
                let frame = AudioFrame {
                    timestamp: std::time::Instant::now(),
                    sample_rate,
                    channels,
                    samples: data.to_vec(),
                };
                observability::metrics::set_audio_level("default", db_level);
                if let Err(e) = tx_clone.try_send(frame)
                    && matches!(e, tokio::sync::mpsc::error::TrySendError::Full(_))
                {
                    warn!("Audio channel full, dropping i16 frame");
                }
            },
            move |err| {
                error!("Audio capture error (i16): {err}");
            },
            None,
        )
        .context("failed to build i16 input stream")?;
    Ok(stream)
}

/// Build a u16 input stream, center-convert to i16 samples.
fn build_u16_stream(
    device: &Device,
    config: StreamConfig,
    channels: u16,
    sample_rate: f64,
    tx: mpsc::Sender<AudioFrame>,
) -> Result<cpal::Stream> {
    let tx_clone = tx.clone();
    let stream = device
        .build_input_stream(
            config,
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                let samples: Vec<i16> = data
                    .iter()
                    .map(|&s| {
                        // Convert u16 [0, 65535] to i16 [-32768, 32767]
                        // by wrapping subtraction of 32768 (middle value)
                        s.wrapping_sub(32768) as i16
                    })
                    .collect();
                let db_level = compute_rms_db(&samples);
                let frame = AudioFrame {
                    timestamp: std::time::Instant::now(),
                    sample_rate,
                    channels,
                    samples,
                };
                observability::metrics::set_audio_level("default", db_level);
                if let Err(e) = tx_clone.try_send(frame)
                    && matches!(e, tokio::sync::mpsc::error::TrySendError::Full(_))
                {
                    warn!("Audio channel full, dropping u16 frame");
                }
            },
            move |err| {
                error!("Audio capture error (u16): {err}");
            },
            None,
        )
        .context("failed to build u16 input stream")?;
    Ok(stream)
}

// ---------------------------------------------------------------------------
// Audio level metering
// ---------------------------------------------------------------------------

/// Compute the RMS level in dBFS from a slice of i16 PCM samples.
///
/// Returns a value in the range [-96.0, 0.0] where 0.0 dBFS is maximum
/// amplitude. Values below -96.0 dBFS are clamped to -96.0 (effective
/// silence).
pub fn compute_rms_db(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return -96.0;
    }

    let sum_sq: f64 = samples
        .iter()
        .map(|&s| {
            let f = s as f64;
            f * f
        })
        .sum();

    let rms = (sum_sq / samples.len() as f64).sqrt();
    let max_amplitude = i16::MAX as f64;

    if rms <= 0.0 {
        return -96.0;
    }

    let db = 20.0 * (rms / max_amplitude).log10();
    db.max(-96.0)
}

// ---------------------------------------------------------------------------
// Public API: device enumeration
// ---------------------------------------------------------------------------

/// Enumerate available audio input devices with their supported configurations.
pub fn enumerate_devices() -> Result<Vec<AudioDeviceInfo>> {
    let host = cpal::default_host();
    let devices = host
        .input_devices()
        .context("failed to enumerate audio input devices")?;

    let mut result = Vec::new();
    for device in devices {
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "<unknown>".into());
        let supported = match device.supported_input_configs() {
            Ok(ranges) => ranges
                .map(|r| AudioConfigInfo {
                    channels: r.channels(),
                    min_sample_rate: r.min_sample_rate() as f64,
                    max_sample_rate: r.max_sample_rate() as f64,
                    sample_format: format!("{:?}", r.sample_format()),
                })
                .collect(),
            Err(e) => {
                warn!(device = %name, error = %e, "Failed to query supported input configs");
                Vec::new()
            }
        };

        result.push(AudioDeviceInfo {
            name,
            supported_configs: supported,
        });
    }

    if result.is_empty() {
        info!("No audio input devices found");
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::traits::DeviceTrait;

    /// Smoke test: enumerate audio devices (safe on CI/headless).
    #[test]
    fn test_enumerate_audio_devices() {
        let devices = enumerate_devices().unwrap_or_default();
        if devices.is_empty() {
            println!("no audio input devices found (expected on CI/headless)");
        } else {
            for dev in &devices {
                println!("  {} ({} configs)", dev.name, dev.supported_configs.len());
            }
        }
    }

    /// Integration test: open default audio input, start capture, receive frame.
    ///
    /// Skipped when no audio input hardware is available.
    #[tokio::test]
    async fn test_audio_capture_channel_contract() {
        let host = cpal::default_host();
        let device = match host.default_input_device() {
            Some(d) => d,
            None => {
                println!("skipping test — no default audio input device");
                return;
            }
        };

        let config = match device.default_input_config() {
            Ok(c) => c,
            Err(e) => {
                println!("skipping test — no default config: {e}");
                return;
            }
        };
        let mut capture = match AudioCapture::new(&device, &config) {
            Ok(c) => c,
            Err(e) => {
                println!("skipping test — AudioCapture::new failed: {e}");
                return;
            }
        };
        let mut rx = match capture.start(&device, &config) {
            Ok(rx) => rx,
            Err(e) => {
                println!("skipping test — AudioCapture::start failed: {e}");
                return;
            }
        };

        // Wait up to 500 ms for an audio frame.
        let timeout = tokio::time::sleep(std::time::Duration::from_millis(500));
        tokio::pin!(timeout);

        let got_frame = tokio::select! {
            frame = rx.recv() => {
                let f = frame.expect("expected an audio frame from live device");
                assert!(f.sample_rate > 0.0, "sample rate must be positive");
                assert!(f.channels > 0, "channel count must be positive");
                assert!(!f.samples.is_empty(), "frame must carry samples");
                println!(
                    "received audio frame: {} Hz, {} ch, {} samples",
                    f.sample_rate,
                    f.channels,
                    f.samples.len()
                );
                true
            }
            _ = &mut timeout => {
                println!("timeout waiting for audio frame (device may be idle or no mic input)");
                false
            }
        };

        if got_frame {
            println!("successfully received an audio frame");
        }

        // Dropping the capture stops the audio stream.
        drop(capture);
        let remaining = rx.recv().await;
        assert!(
            remaining.is_none(),
            "channel should be closed after AudioCapture is dropped"
        );
    }

    #[test]
    fn test_compute_rms_db_silence() {
        // All-zero samples should give -96.0 dBFS (silence floor).
        let samples = vec![0i16; 480];
        let db = compute_rms_db(&samples);
        assert!(
            (db - (-96.0)).abs() < 0.01,
            "silence should be -96.0 dBFS, got {}",
            db
        );
    }

    #[test]
    fn test_compute_rms_db_full_scale_sine() {
        // A full-scale sine wave at amplitude 32767 has RMS = 32767/sqrt(2),
        // giving approximately -3.01 dBFS.
        let n = 480;
        let amplitude: i16 = 32767;
        let samples: Vec<i16> = (0..n)
            .map(|i| {
                let phase = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
                (amplitude as f64 * phase.sin()) as i16
            })
            .collect();
        let db = compute_rms_db(&samples);
        // Full-scale sine ≈ -3.01 dBFS, allow small rounding tolerance.
        let expected = -3.01;
        assert!(
            (db - expected).abs() < 0.1,
            "full-scale sine should be approx {expected} dBFS, got {db}",
        );
    }

    #[test]
    fn test_compute_rms_db_half_scale_sine() {
        // A half-scale sine wave at amplitude 16384 has RMS ≈ -9.03 dBFS.
        let n = 480;
        let amplitude: i16 = 16384;
        let samples: Vec<i16> = (0..n)
            .map(|i| {
                let phase = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
                (amplitude as f64 * phase.sin()) as i16
            })
            .collect();
        let db = compute_rms_db(&samples);
        // Half-scale sine ≈ -9.03 dBFS, allow small rounding tolerance.
        let expected = -9.03;
        assert!(
            (db - expected).abs() < 0.1,
            "half-scale sine should be approx {expected} dBFS, got {db}",
        );
    }

    #[test]
    fn test_compute_rms_db_empty() {
        let db = compute_rms_db(&[]);
        assert!(
            (db - (-96.0)).abs() < 0.01,
            "empty slice should return -96.0, got {}",
            db
        );
    }
}
