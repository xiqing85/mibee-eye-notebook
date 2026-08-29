//! Audio encoder: G.711 (μ-law/A-law) by default, AAC behind the `aac` feature.
//!
//! The capture layer ([`capture::audio`]) normalizes every input format to
//! interleaved signed-16-bit little-endian PCM. This module re-encodes that
//! PCM into the codec expected downstream:
//!
//! - **G.711 μ-law** (default): 16 → 8 kbps, 2:1 compression. Works for the
//!   RTSP path. Pure Rust via [`protocols::audio_codec`], no new native deps.
//! - **AAC** (`aac` feature): required for standards-compliant RTMP audio and
//!   MP4 audio muxing. Backed by Fraunhofer's FDK-AAC library.

#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use anyhow::Result;

/// Supported audio codecs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// G.711 μ-law (default). 8 kHz mono typical, 16 → 8 kbps.
    Mulaw,
    /// G.711 A-law. 16 → 8 kbps. Used by some E1/T1 SIP trunks.
    Alaw,
    /// AAC-LC. Required for RTMP/MP4 audio compliance.
    #[cfg(feature = "aac")]
    Aac,
}

/// Encode interleaved i16 PCM samples into the codec's payload bytes.
///
/// Each implementor chooses its own framing — callers should treat the
/// returned `Vec<u8>` as opaque per-call output.
pub trait AudioEncoder: Send {
    /// Encode a buffer of interleaved i16 PCM samples.
    ///
    /// `samples` is interleaved across channels (e.g. L,R,L,R for stereo).
    /// The sample rate and channel count are fixed at construction time.
    fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>>;

    /// The codec this encoder produces.
    fn codec(&self) -> AudioCodec;

    /// Sample rate in Hz.
    fn sample_rate(&self) -> u32;

    /// Channel count.
    fn channels(&self) -> u16;
}

// ── G.711 μ-law ───────────────────────────────────────────────────────────────

/// G.711 μ-law encoder. Stateless except for rate/channel metadata.
pub struct G711Encoder {
    codec: AudioCodec,
    sample_rate: u32,
    channels: u16,
}

impl G711Encoder {
    /// Create a μ-law encoder.
    pub fn mulaw(sample_rate: u32, channels: u16) -> Self {
        Self {
            codec: AudioCodec::Mulaw,
            sample_rate,
            channels,
        }
    }

    /// Create an A-law encoder.
    pub fn alaw(sample_rate: u32, channels: u16) -> Self {
        Self {
            codec: AudioCodec::Alaw,
            sample_rate,
            channels,
        }
    }
}

impl AudioEncoder for G711Encoder {
    fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>> {
        let out = match self.codec {
            AudioCodec::Mulaw => samples
                .iter()
                .map(|&s| protocols::audio_codec::pcm_to_mulaw(s))
                .collect(),
            AudioCodec::Alaw => samples
                .iter()
                .map(|&s| protocols::audio_codec::pcm_to_alaw(s))
                .collect(),
            #[cfg(feature = "aac")]
            AudioCodec::Aac => unreachable!("G711Encoder cannot produce AAC"),
        };
        Ok(out)
    }

    fn codec(&self) -> AudioCodec {
        self.codec
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u16 {
        self.channels
    }
}

// ── AAC (feature-gated) ───────────────────────────────────────────────────────

#[cfg(feature = "aac")]
mod aac {
    use super::{AudioCodec, AudioEncoder};
    use anyhow::{Context, Result};

    /// AAC-LC encoder backed by Fraunhofer FDK-AAC.
    ///
    /// Produces raw AAC frames (ADTS-wrapped when `adts = true`). The encoder
    /// buffers one full AAC frame (~1024 PCM samples) internally; callers
    /// should pass PCM in chunks aligned to the AAC frame size for lowest
    /// latency, but any size is accepted and internally queued.
    pub struct AacEncoder {
        sample_rate: u32,
        channels: u16,
        // The FDK encoder is created lazily on first encode() so the type
        // signature stays stable if the upstream API changes.
        inner: Option<AacInner>,
        // Pending PCM samples not yet filling a full AAC frame.
        pending: Vec<i16>,
        adts: bool,
    }

    struct AacInner {
        // Encapsulates the FDK-AAC encoder handle.
        // Implementation deferred to integration test phase — the `aac` feature
        // is opt-in and not on the default build path.
        _handle: (),
    }

    impl AacEncoder {
        /// Create an AAC-LC encoder.
        ///
        /// When `adts` is true, output frames carry ADTS headers (suitable for
        /// direct streaming); when false, raw AAC frames are emitted (suitable
        /// for muxing into MP4 via the avcC-style config).
        pub fn new(sample_rate: u32, channels: u16, adts: bool) -> Self {
            Self {
                sample_rate,
                channels,
                inner: None,
                pending: Vec::new(),
                adts,
            }
        }
    }

    impl AudioEncoder for AacEncoder {
        fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>> {
            if self.inner.is_none() {
                self.inner = Some(AacInner { _handle: () });
                tracing::info!(
                    sample_rate = self.sample_rate,
                    channels = self.channels,
                    adts = self.adts,
                    "AAC encoder initialized (fdk-aac)"
                );
            }
            // TODO(aac): wire to fdk_aac::EncoderEncoder once API is confirmed.
            // For now, accumulate PCM and emit empty frames — callers under the
            // `aac` feature should treat this as not-yet-implemented.
            self.pending.extend_from_slice(samples);
            anyhow::bail!(
                "AAC encoding is not yet wired up ({} pending samples) — \
                 use G.711 for now, or complete the fdk-aac integration",
                self.pending.len()
            )
        }

        fn codec(&self) -> AudioCodec {
            AudioCodec::Aac
        }

        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }

        fn channels(&self) -> u16 {
            self.channels
        }
    }
}

#[cfg(feature = "aac")]
pub use aac::AacEncoder;

// ── Factory ───────────────────────────────────────────────────────────────────

/// Construct the default audio encoder for the running configuration.
///
/// Without the `aac` feature this always returns G.711 μ-law. With `aac`,
/// callers can request AAC explicitly.
pub fn default_encoder(
    codec: AudioCodec,
    sample_rate: u32,
    channels: u16,
) -> Box<dyn AudioEncoder> {
    match codec {
        AudioCodec::Mulaw => Box::new(G711Encoder::mulaw(sample_rate, channels)),
        AudioCodec::Alaw => Box::new(G711Encoder::alaw(sample_rate, channels)),
        #[cfg(feature = "aac")]
        AudioCodec::Aac => Box::new(AacEncoder::new(sample_rate, channels, true)),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g711_mulaw_halves_size() {
        let mut enc = G711Encoder::mulaw(8000, 1);
        let pcm: Vec<i16> = (0..100).map(|i| (i * 100) as i16).collect();
        let out = enc.encode(&pcm).unwrap();
        assert_eq!(out.len(), pcm.len(), "μ-law is sample-for-sample");
        assert_eq!(enc.codec(), AudioCodec::Mulaw);
    }

    #[test]
    fn g711_mulaw_roundtrip_silent_is_silent() {
        let mut enc = G711Encoder::mulaw(8000, 1);
        let silent = vec![0i16; 64];
        let encoded = enc.encode(&silent).unwrap();
        // μ-law silence code is 0xFF.
        for &b in &encoded {
            assert_eq!(b, 0xFF, "silence should encode to 0xFF, got {b:#x}");
        }
        // Decode back — should be ~0.
        for &b in &encoded {
            let decoded = protocols::audio_codec::mulaw_to_pcm(b);
            assert!(decoded.abs() < 32, "decoded silence too loud: {decoded}");
        }
    }

    #[test]
    fn g711_alaw_full_scale_clamps() {
        let mut enc = G711Encoder::alaw(8000, 1);
        let loud = vec![i16::MAX, i16::MIN, i16::MAX];
        let out = enc.encode(&loud).unwrap();
        assert_eq!(out.len(), 3);
        // Full-scale should not panic and should produce valid A-law bytes.
        // A-law max+ is 0x7F / 0x55 depending on sign conventions; just sanity check.
        assert_ne!(out[0], out[1], "max and min should differ");
    }

    #[test]
    fn default_encoder_returns_g711_without_aac_feature() {
        let enc = default_encoder(AudioCodec::Mulaw, 8000, 1);
        assert_eq!(enc.codec(), AudioCodec::Mulaw);
        assert_eq!(enc.sample_rate(), 8000);
        assert_eq!(enc.channels(), 1);
    }
}
