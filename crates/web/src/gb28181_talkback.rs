//! GB28181 voice-talkback receive pipeline (device side, GB/T 28181-2022
//! §9.2): the gb28181-rs [`AudioTalkbackSink`] decodes G.711 RTP payloads
//! to 8 kHz mono PCM and feeds a cpal output stream.
//!
//! Fail-open posture: when talkback playback is disabled or the host has
//! no usable audio output, no sink is registered and the library refuses
//! talkback INVITEs with 488 — never a 200 OK with silently dropped
//! audio.

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::{Context, Result};
use gb28181_rs::AudioCodec;
use gb28181_rs::server::AudioTalkbackSink;
use parking_lot::Mutex;
use protocols::audio_codec::{alaw_to_pcm, mulaw_to_pcm};

/// Decoded-PCM destination: a cpal ring buffer in production, a collector
/// in tests.
pub trait PcmSink: Send + Sync {
    /// Append 8 kHz mono samples.
    fn push(&self, samples: &[i16]);
}

/// Decodes G.711 talkback packets into 8 kHz mono PCM. One instance
/// serves the whole GB28181 runtime; the per-packet codec comes from the
/// offer-negotiated payload type (A-law vs μ-law).
pub struct TalkbackDecoder {
    out: Arc<dyn PcmSink>,
}

impl TalkbackDecoder {
    pub fn new(out: Arc<dyn PcmSink>) -> Self {
        Self { out }
    }

    fn decode(payload: &[u8], codec: AudioCodec) -> Vec<i16> {
        match codec {
            AudioCodec::Pcma => payload.iter().map(|&b| alaw_to_pcm(b)).collect(),
            AudioCodec::Pcmu => payload.iter().map(|&b| mulaw_to_pcm(b)).collect(),
        }
    }
}

impl AudioTalkbackSink for TalkbackDecoder {
    fn on_audio(&self, payload: &[u8], _ssrc: u32) {
        // Only reached for sinks that do not override on_audio_codec;
        // this one does. A-law is the dominant GB28181 variant if the
        // codec-aware path ever regresses.
        self.out.push(&Self::decode(payload, AudioCodec::Pcma));
    }

    fn on_audio_codec(&self, payload: &[u8], _ssrc: u32, codec: AudioCodec) {
        self.out.push(&Self::decode(payload, codec));
    }
}

/// Bounded mono PCM ring (≈2 s at 8 kHz). Live audio: drop-oldest keeps
/// playback latency bounded instead of accumulating a backlog when the
/// consumer stalls.
struct PcmRing {
    buf: Mutex<VecDeque<i16>>,
    cap: usize,
}

impl PcmRing {
    fn new(cap: usize) -> Self {
        Self {
            buf: Mutex::new(VecDeque::with_capacity(cap)),
            cap,
        }
    }

    fn write(&self, samples: &[i16]) {
        let mut buf = self.buf.lock();
        buf.extend(samples.iter().copied());
        while buf.len() > self.cap {
            buf.pop_front();
        }
    }

    /// Drain up to `out.len()` samples (FIFO) into `out`; returns the
    /// number filled. The remainder is zero-filled (output silence).
    fn drain_into(&self, out: &mut [i16]) -> usize {
        let mut buf = self.buf.lock();
        let n = out.len().min(buf.len());
        for slot in out.iter_mut().take(n) {
            *slot = buf.pop_front().unwrap_or(0);
        }
        for slot in out.iter_mut().skip(n) {
            *slot = 0;
        }
        n
    }
}

/// Expand mono i16 samples to interleaved N-channel i16.
fn expand_i16(mono: &[i16], channels: usize, out: &mut Vec<i16>) {
    out.clear();
    out.reserve(mono.len() * channels);
    for &s in mono {
        for _ in 0..channels {
            out.push(s);
        }
    }
}

/// Expand mono i16 samples to interleaved N-channel f32.
fn expand_f32(mono: &[i16], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    out.reserve(mono.len() * channels);
    for &s in mono {
        let v = f32::from(s) / 32768.0;
        for _ in 0..channels {
            out.push(v);
        }
    }
}

/// cpal-backed PCM sink: writes into the ring drained by the output
/// stream callback. The returned [`cpal::Stream`] must be kept alive for
/// playback to continue.
pub struct CpalTalkbackOutput {
    ring: Arc<PcmRing>,
}

impl CpalTalkbackOutput {
    /// Open the default output device at G.711's native layout
    /// (8 kHz mono); when the host refuses it, retry with the device's
    /// default config, adapting channels and sample format (still at
    /// 8 kHz — no resampling). Any other rate is an error so the caller
    /// can fail open (no sink → 488).
    pub fn open() -> Result<(Arc<Self>, cpal::Stream)> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        use cpal::{SampleFormat, StreamConfig};

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("no audio output device")?;

        // Preferred: 8 kHz mono i16, the G.711 native layout.
        let native = StreamConfig {
            channels: 1,
            sample_rate: 8000,
            buffer_size: cpal::BufferSize::Default,
        };
        let ring = Arc::new(PcmRing::new(16_000));
        let ring_i16 = Arc::clone(&ring);
        if let Ok(stream) = device.build_output_stream(
            native,
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                ring_i16.drain_into(data);
            },
            |err| tracing::warn!(error = %err, "talkback output stream error"),
            None,
        ) {
            stream.play().context("start talkback output stream")?;
            return Ok((Self::spawn(ring), stream));
        }

        // Fallback: the device's default config, 8 kHz only.
        let supported = device
            .default_output_config()
            .context("query default output config")?;
        if supported.sample_rate() != 8000 {
            anyhow::bail!(
                "output device cannot run at 8 kHz (default {} Hz, channels {})",
                supported.sample_rate(),
                supported.channels()
            );
        }
        let channels = supported.channels() as usize;
        let config = supported.config();
        let stream = match supported.sample_format() {
            SampleFormat::I16 => {
                let ring_cb = Arc::clone(&ring);
                let mut mono = Vec::new();
                let mut interleaved = Vec::new();
                device.build_output_stream(
                    config,
                    move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                        mono.clear();
                        mono.resize(data.len() / channels, 0);
                        ring_cb.drain_into(&mut mono);
                        expand_i16(&mono, channels, &mut interleaved);
                        data.copy_from_slice(&interleaved);
                    },
                    |err| tracing::warn!(error = %err, "talkback output stream error"),
                    None,
                )
            }
            SampleFormat::F32 => {
                let ring_cb = Arc::clone(&ring);
                let mut mono = Vec::new();
                let mut interleaved = Vec::new();
                device.build_output_stream(
                    config,
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        mono.clear();
                        mono.resize(data.len() / channels, 0);
                        ring_cb.drain_into(&mut mono);
                        expand_f32(&mono, channels, &mut interleaved);
                        data.copy_from_slice(&interleaved);
                    },
                    |err| tracing::warn!(error = %err, "talkback output stream error"),
                    None,
                )
            }
            other => anyhow::bail!("unsupported output sample format: {other:?}"),
        }
        .context("build talkback output stream (default config)")?;
        stream.play().context("start talkback output stream")?;
        Ok((Self::spawn(ring), stream))
    }

    fn spawn(ring: Arc<PcmRing>) -> Arc<Self> {
        Arc::new(Self { ring })
    }
}

impl PcmSink for CpalTalkbackOutput {
    fn push(&self, samples: &[i16]) {
        self.ring.write(samples);
    }
}

/// Resolve the talkback sink per config: `None` when disabled; when
/// enabled, a decoder feeding the local output device. Errors bubble up
/// so the caller can fail open (no sink → the library answers 488).
pub fn open_sink(enabled: bool) -> Result<Option<(Arc<dyn AudioTalkbackSink>, cpal::Stream)>> {
    if !enabled {
        return Ok(None);
    }
    let (out, stream) = CpalTalkbackOutput::open()?;
    Ok(Some((Arc::new(TalkbackDecoder::new(out)), stream)))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct Collector(Mutex<Vec<i16>>);

    impl PcmSink for Collector {
        fn push(&self, samples: &[i16]) {
            self.0.lock().unwrap().extend_from_slice(samples);
        }
    }

    #[test]
    fn decoder_decodes_pcma_payload() {
        let out = Arc::new(Collector(Mutex::new(Vec::new())));
        let decoder = TalkbackDecoder::new(Arc::clone(&out) as Arc<dyn PcmSink>);
        let sink: Arc<dyn AudioTalkbackSink> = Arc::new(decoder);
        sink.on_audio_codec(&[0xD5, 0x5A, 0xA5], 200006001, AudioCodec::Pcma);
        assert_eq!(
            out.0.lock().unwrap().as_slice(),
            [alaw_to_pcm(0xD5), alaw_to_pcm(0x5A), alaw_to_pcm(0xA5)]
        );
    }

    #[test]
    fn decoder_decodes_pcmu_payload() {
        let out = Arc::new(Collector(Mutex::new(Vec::new())));
        let decoder = TalkbackDecoder::new(Arc::clone(&out) as Arc<dyn PcmSink>);
        let sink: Arc<dyn AudioTalkbackSink> = Arc::new(decoder);
        sink.on_audio_codec(&[0xFF, 0x7F], 777, AudioCodec::Pcmu);
        assert_eq!(
            out.0.lock().unwrap().as_slice(),
            [mulaw_to_pcm(0xFF), mulaw_to_pcm(0x7F)]
        );
    }

    #[test]
    fn ring_drains_fifo_and_zero_fills() {
        let ring = PcmRing::new(8);
        ring.write(&[1, 2, 3]);
        let mut out = [0i16; 5];
        assert_eq!(ring.drain_into(&mut out), 3);
        assert_eq!(out, [1, 2, 3, 0, 0]);
        // Drained empty → pure silence.
        let mut out2 = [9i16; 2];
        assert_eq!(ring.drain_into(&mut out2), 0);
        assert_eq!(out2, [0, 0]);
    }

    #[test]
    fn ring_drops_oldest_beyond_cap() {
        let ring = PcmRing::new(4);
        ring.write(&[1, 2, 3, 4, 5, 6]);
        let mut out = [0i16; 8];
        assert_eq!(ring.drain_into(&mut out), 4);
        assert_eq!(out, [3, 4, 5, 6, 0, 0, 0, 0]);
    }

    #[test]
    fn expand_i16_interleaves_channels() {
        let mut out = Vec::new();
        expand_i16(&[10, -10], 2, &mut out);
        assert_eq!(out, [10, 10, -10, -10]);
    }

    #[test]
    fn expand_f32_interleaves_and_scales() {
        let mut out = Vec::new();
        expand_f32(&[16384], 1, &mut out);
        assert!((out[0] - 0.5).abs() < 1e-6, "got {}", out[0]);
    }

    #[test]
    fn open_sink_disabled_returns_none_without_touching_audio() {
        let opened = open_sink(false).expect("disabled must not error");
        assert!(opened.is_none());
    }
}
