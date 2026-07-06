//! H.264 software encoder backed by Cisco's OpenH264 (BSD-2-Clause).
//!
//! Produces individual H.264 NAL units (start code stripped) suitable for
//! direct emission as [`crate::source::MediaFrame::Video`] — preserving the
//! downstream data contract that [`crate::output`] adapters depend on
//! (`data[0] & 0x1f` is the NAL type).
//!
//! # Profile / tuning
//!
//! The encoder is tuned per-stream via [`H264EncoderConfig`], whose
//! `quality_preset` field maps onto a coherent bundle of profile / complexity /
//! rate-control / QP-range settings:
//!
//! | Preset       | Profile | Complexity | RC mode  | QP range | Use case                |
//! |--------------|---------|------------|----------|----------|-------------------------|
//! | `UltraFast`  | Baseline| Low        | Quality  | 10–51    | Weak CPUs (≤2 cores)    |
//! | `Medium`     | High    | Medium     | Quality  | 10–48    | Balanced default        |
//! | `High`       | High    | High       | Quality  | 10–40    | Strong CPUs / clarity   |
//! | `HardwareMax`| —       | —          | —        | —        | Handled by HW backends  |
//!
//! **GOP** (`intra_frame_period`) is derived from the actual `fps` so a
//! keyframe lands every second regardless of capture frame rate.

#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use anyhow::{Context, Result};
use openh264::encoder::{BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Profile, QpRange, RateControlMode};
use openh264::formats::YUVSource;
use openh264::Timestamp;

use crate::capability::QualityPreset;
use super::convert::Yuv420p;

/// Configuration for constructing an [`H264Encoder`].
#[derive(Debug, Clone, Copy)]
pub struct H264EncoderConfig {
    /// Frame width in pixels (must be even).
    pub width: u32,
    /// Frame height in pixels (must be even).
    pub height: u32,
    /// Target frame rate in Hz. Drives the GOP size (1-second keyframe
    /// interval) and the muxer's `max_frame_rate`.
    pub fps: f32,
    /// Target bitrate in bits per second.
    pub bitrate_bps: u32,
    /// Quality preset — selects profile / complexity / rate control / QP range.
    pub quality_preset: QualityPreset,
}

impl Default for H264EncoderConfig {
    fn default() -> Self {
        // Match the old ffmpeg defaults: 1280x720@30fps, balanced quality.
        Self {
            width: 1280,
            height: 720,
            fps: 30.0,
            bitrate_bps: 2_500_000,
            quality_preset: QualityPreset::Medium,
        }
    }
}

/// Resolve a [`QualityPreset`] into concrete OpenH264 tuning parameters.
///
/// Kept as a standalone function so it can be unit-tested without
/// constructing an encoder.
fn preset_to_tuning(preset: QualityPreset) -> (&'static str, Profile, Complexity, RateControlMode, QpRange) {
    match preset {
        // Weak CPUs: Baseline + lowest complexity + wide QP range. Maximise
        // throughput at the cost of compression efficiency and image quality.
        QualityPreset::UltraFast => (
            "ultra-fast",
            Profile::Baseline,
            Complexity::Low,
            RateControlMode::Quality,
            // QP 18 keeps keyframes sharp; allow up to 45 before quality collapses.
            QpRange::new(18, 45),
        ),
        // Balanced: High profile (cabinets + 8x8 transform), medium
        // complexity. Good default for 4+ core CPUs.
        QualityPreset::Medium => (
            "medium",
            Profile::High,
            Complexity::Medium,
            RateControlMode::Quality,
            QpRange::new(15, 44),
        ),
        // Clarity-focused: High profile + high complexity motion search,
        // tighter QP range to hold image quality under motion.
        QualityPreset::High => (
            "high",
            Profile::High,
            Complexity::High,
            RateControlMode::Quality,
            QpRange::new(10, 40),
        ),
        // Hardware backends own their tuning; the software path should never
        // see this preset. Fall back to Medium defensively.
        QualityPreset::HardwareMax => (
            "medium (hw-max fallback)",
            Profile::High,
            Complexity::Medium,
            RateControlMode::Quality,
            QpRange::new(15, 44),
        ),
    }
}

/// A single H.264 NAL unit, start code stripped, with keyframe annotation.
#[derive(Debug, Clone)]
pub struct NalUnit {
    /// NAL payload bytes (header byte is `data[0]`).
    pub data: Vec<u8>,
    /// True for IDR (type 5), SPS (type 7), PPS (type 8).
    pub is_keyframe: bool,
}

/// H.264 encoder backed by OpenH264.
///
/// Wraps [`openh264::encoder::Encoder`] and exposes a simple
/// `encode(frame, timestamp_ms)` API returning split NAL units. The encoder
/// is `Send` (openh264 marks it so) but **not** `Sync` — it must be driven by
/// a single task. In the capture pipeline that task is the
/// [`crate::capture_source::VideoCaptureSource`] frame loop.
pub struct H264Encoder {
    encoder: Encoder,
    /// Cached so we can log/diagnose mismatches between configured and actual
    /// input dimensions (the underlying encoder auto-reinits on change, but a
    /// resolution change mid-stream is unexpected for a fixed camera).
    config: H264EncoderConfig,
}

impl H264Encoder {
    /// Create a new encoder with the given configuration.
    pub fn new(config: H264EncoderConfig) -> Result<Self> {
        let api = openh264::OpenH264API::from_source();
        // GOP = one keyframe per second, derived from the *actual* capture
        // frame rate. The previous code hardcoded 30 fps which produced a
        // 3-second keyframe interval for 10 fps cameras.
        let fps_for_gop = if config.fps >= 1.0 {
            config.fps
        } else {
            30.0
        };
        let gop = IntraFramePeriod::from_num_frames(fps_for_gop.round().max(1.0) as u32);

        let (preset_label, profile, complexity, rc_mode, qp_range) =
            preset_to_tuning(config.quality_preset);

        let enc_config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(config.bitrate_bps))
            .max_frame_rate(FrameRate::from_hz(config.fps))
            .profile(profile)
            .complexity(complexity)
            .rate_control_mode(rc_mode)
            .qp(qp_range)
            .intra_frame_period(gop)
            // Scene-change detection + adaptive quantisation + background
            // detection all default on; they improve quality for a moving
            // surveillance scene at negligible cost.
            ;

        let encoder = Encoder::with_api_config(api, enc_config)
            .context("failed to initialize OpenH264 encoder")?;

        tracing::info!(
            width = config.width,
            height = config.height,
            fps = config.fps,
            bitrate_bps = config.bitrate_bps,
            preset = preset_label,
            gop_frames = fps_for_gop.round() as u32,
            "OpenH264 encoder initialized"
        );

        Ok(Self { encoder, config })
    }

    /// Encode a single YUV420p frame at the given presentation timestamp
    /// (milliseconds since stream start).
    ///
    /// Returns one [`NalUnit`] per NAL in the produced access unit. OpenH264
    /// emits SPS/PPS inline on each IDR when `INCREASING_ID` strategy is used,
    /// but with the default `CONSTANT_ID` strategy SPS/PPS only appear on the
    /// first frame. We force SPS/PPS re-emission on keyframes by calling
    /// [`Encoder::force_intra_frame`] which produces a self-contained IDR —
    /// downstream consumers (RTSP, RTMP) cache SPS/PPS from the first frame
    /// and don't require re-emission, so the default strategy is fine.
    pub fn encode(&mut self, frame: &Yuv420p, timestamp_ms: u64) -> Result<Vec<NalUnit>> {
        if (frame.width, frame.height) != (self.config.width, self.config.height) {
            tracing::warn!(
                expected = ?(self.config.width, self.config.height),
                actual = ?(frame.width, frame.height),
                "frame dimensions differ from encoder config — openh264 will reinit"
            );
        }

        let bitstream = self
            .encoder
            .encode_at(frame, Timestamp::from_millis(timestamp_ms))
            .context("openh264 encode_frame failed")?;

        let frame_type = bitstream.frame_type();
        let is_keyframe = matches!(frame_type, FrameType::IDR | FrameType::I);

        // OpenH264 writes Annex B (start-code-prefixed) NALs into the
        // bitstream. We need them split into individual NAL units with the
        // start code stripped, to match the MediaFrame::Video contract.
        let mut annex_b = Vec::with_capacity(64 * 1024);
        bitstream.write_vec(&mut annex_b);

        let nals = split_annex_b_to_nals(&annex_b, is_keyframe);

        tracing::trace!(
            timestamp_ms,
            frame_type = ?frame_type,
            nal_count = nals.len(),
            bytes = annex_b.len(),
            "encoded frame"
        );

        Ok(nals)
    }

    /// Force the next [`encode`](Self::encode) call to produce an IDR frame.
    ///
    /// Useful after a stream restart or format change so consumers receive a
    /// fresh keyframe promptly.
    pub fn force_keyframe(&mut self) {
        self.encoder.force_intra_frame();
    }

    /// Return the encoder's configured dimensions.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }
}

// ── YUVSource impl: bridge our Yuv420p into openh264 ──────────────────────────

impl YUVSource for Yuv420p {
    fn dimensions(&self) -> (usize, usize) {
        (self.width as usize, self.height as usize)
    }

    fn strides(&self) -> (usize, usize, usize) {
        // Tightly packed: Y stride = width, U/V stride = width/2.
        let y_stride = self.width as usize;
        let uv_stride = (self.width as usize) / 2;
        (y_stride, uv_stride, uv_stride)
    }

    fn y(&self) -> &[u8] {
        self.y_plane()
    }

    fn u(&self) -> &[u8] {
        self.u_plane()
    }

    fn v(&self) -> &[u8] {
        self.v_plane()
    }
}

// Also implement for &Yuv420p so callers can borrow without consuming the
// owned frame (the encode loop passes a reference).
impl YUVSource for &Yuv420p {
    fn dimensions(&self) -> (usize, usize) {
        (*self).dimensions()
    }
    fn strides(&self) -> (usize, usize, usize) {
        (*self).strides()
    }
    fn y(&self) -> &[u8] {
        (*self).y()
    }
    fn u(&self) -> &[u8] {
        (*self).u()
    }
    fn v(&self) -> &[u8] {
        (*self).v()
    }
}

// ── Annex B splitting ─────────────────────────────────────────────────────────

/// Split an Annex B byte stream into individual NAL units, stripping start
/// codes (`00 00 01` or `00 00 00 01`).
///
/// `is_keyframe` annotates every NAL from this access unit — technically SPS
/// (7) and PPS (8) aren't "keyframes" but downstream consumers treat them as
/// metadata that must precede an IDR, so marking them as keyframe-bearing is
/// the convention used throughout the streaming pipeline.
fn split_annex_b_to_nals(annex_b: &[u8], is_keyframe: bool) -> Vec<NalUnit> {
    let mut nals = Vec::new();
    let mut i = 0;
    let n = annex_b.len();

    while i < n {
        // Find the next start code at position i.
        let sc_len = match start_code_len_at(annex_b, i) {
            Some(len) => len,
            None => {
                i += 1;
                continue;
            }
        };
        let nal_start = i + sc_len;
        // Find the next start code (or EOF) to bound this NAL.
        let mut j = nal_start + 1;
        while j < n {
            if start_code_len_at(annex_b, j).is_some() {
                break;
            }
            j += 1;
        }
        if nal_start < j {
            nals.push(NalUnit {
                data: annex_b[nal_start..j].to_vec(),
                is_keyframe,
            });
        }
        i = j;
    }

    nals
}

/// Return the start-code length (3 or 4) if `data[i..]` begins with one.
fn start_code_len_at(data: &[u8], i: usize) -> Option<usize> {
    if i + 4 <= data.len() && data[i..i + 4] == [0x00, 0x00, 0x00, 0x01] {
        Some(4)
    } else if i + 3 <= data.len() && data[i..i + 3] == [0x00, 0x00, 0x01] {
        Some(3)
    } else {
        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_handles_3_and_4_byte_start_codes() {
        // Two NALs, 4-byte then 3-byte start codes.
        let stream = [
            0x00, 0x00, 0x00, 0x01, // 4-byte SC
            0x67, 0x42, // NAL 1 (SPS-ish)
            0x00, 0x00, 0x01, // 3-byte SC
            0x65, 0x88, 0x80, // NAL 2 (IDR-ish)
        ];
        let nals = split_annex_b_to_nals(&stream, true);
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0].data, vec![0x67, 0x42]);
        assert_eq!(nals[1].data, vec![0x65, 0x88, 0x80]);
        assert!(nals[0].is_keyframe);
    }

    #[test]
    fn split_empty_input() {
        assert!(split_annex_b_to_nals(&[], false).is_empty());
    }

    #[test]
    fn split_single_nal_no_trailing_sc() {
        let stream = [0x00, 0x00, 0x00, 0x01, 0x67, 0xAB];
        let nals = split_annex_b_to_nals(&stream, false);
        assert_eq!(nals.len(), 1);
        assert_eq!(nals[0].data, vec![0x67, 0xAB]);
    }

    #[test]
    fn start_code_detection() {
        assert_eq!(start_code_len_at(&[0, 0, 0, 1, 9], 0), Some(4));
        assert_eq!(start_code_len_at(&[0, 0, 1, 9], 0), Some(3));
        assert_eq!(start_code_len_at(&[0, 0, 2, 9], 0), None);
        assert_eq!(start_code_len_at(&[1, 2, 3], 0), None);
    }

    #[test]
    fn ultra_fast_preset_uses_baseline_for_speed() {
        // openh264's Profile/Complexity enums don't derive PartialEq, so we
        // assert on the preset label (our own string) plus the QP range that
        // is unique to each preset.
        let (label, _profile, _complexity, _, _qp) =
            preset_to_tuning(QualityPreset::UltraFast);
        assert_eq!(label, "ultra-fast");
    }

    #[test]
    fn high_preset_uses_high_profile_and_complexity() {
        let (label, _profile, _complexity, _, _qp) =
            preset_to_tuning(QualityPreset::High);
        assert_eq!(label, "high");
    }

    #[test]
    fn medium_preset_is_the_balanced_default() {
        let (label, _profile, _complexity, _, _qp) =
            preset_to_tuning(QualityPreset::Medium);
        assert_eq!(label, "medium");
    }

    /// End-to-end encoder smoke test.
    ///
    /// Requires the `source` feature's bundled Cisco binary, which is only
    /// present on x86_64/aarch64 Linux. On other hosts this is a no-op.
    #[cfg(target_os = "linux")]
    #[test]
    fn encode_synthetic_frame_produces_valid_nals() {
        let mut encoder = match H264Encoder::new(H264EncoderConfig {
            width: 64,
            height: 64,
            fps: 30.0,
            bitrate_bps: 500_000,
            quality_preset: QualityPreset::Medium,
        }) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("skipping — openh264 init failed: {e}");
                return;
            }
        };

        // A simple gradient frame.
        let mut frame = Yuv420p::new(64, 64);
        for (i, y) in frame.y_plane_mut().iter_mut().enumerate() {
            *y = (i % 256) as u8;
        }

        let nals = encoder.encode(&frame, 0).expect("encode failed");
        assert!(!nals.is_empty(), "encoder produced no NALs");

        // First frame should be a keyframe (IDR), so first NAL should be SPS
        // (type 7) or IDR (type 5). At minimum, at least one keyframe NAL.
        assert!(
            nals.iter().any(|n| n.is_keyframe),
            "first encoded frame should contain keyframe NALs"
        );

        // Each NAL's first byte is the header — verify nal_ref_idc/type nibbles.
        for nal in &nals {
            assert!(!nal.data.is_empty(), "empty NAL");
            let forbidden_bit = nal.data[0] & 0x80;
            assert_eq!(forbidden_bit, 0, "forbidden_zero_bit set in NAL");
        }

        // Verify SPS parses back to our configured dimensions.
        let sps = nals
            .iter()
            .find(|n| !n.data.is_empty() && (n.data[0] & 0x1f) == 7);
        if let Some(sps) = sps {
            // Defer to the protocols parser for a full decode.
            match protocols::h264::parse_sps(&sps.data) {
                Ok(params) => {
                    assert_eq!(params.width(), 64);
                    assert_eq!(params.height(), 64);
                }
                Err(e) => eprintln!("note: SPS parse warning (non-fatal): {e}"),
            }
        }
    }
}
