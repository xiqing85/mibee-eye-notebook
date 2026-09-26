//! Fragmented-MP4 (fMP4) remuxer for MSE / `MediaSource` playback.
//!
//! Takes the H.264 NAL stream produced by [`crate::capture_source`] (one NAL
//! per [`crate::source::MediaFrame::Video`], start code already stripped) and
//! repackages it into the ISO-BMFF byte-stream a browser `MediaSource`
//! expects:
//!
//! ```text
//! init segment  →  ftyp + moov (with mvex, sample-description from SPS/PPS)
//! media segment →  moof + mdat  (one per ~1s of video, keyframe-aligned)
//! ```
//!
//! Each browser connection owns its own [`Fmp4Remuxer`] so clients can join
//! mid-stream: the remuxer waits for the next keyframe, harvests the SPS/PPS
//! it carries, emits an init segment, then forwards every subsequent frame as
//! AVCC-prefixed samples inside rolling media segments.
//!
//! The underlying muxing is delegated to [`muxide::fragmented::FragmentedMuxer`]
//! so this module stays focused on the NAL → sample plumbing.

#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use anyhow::{Context, Result};
use muxide::fragmented::{FragmentConfig, FragmentedMuxer};

use crate::source::MediaFrame;

// ---------------------------------------------------------------------------
// NAL helpers
// ---------------------------------------------------------------------------

/// H.264 NAL unit type (lower 5 bits of byte 0).
fn nal_type(byte0: u8) -> u8 {
    byte0 & 0x1f
}

/// Convert a single NAL payload (start code stripped) into AVCC format:
/// 4-byte big-endian length prefix + raw NAL bytes. This is what MP4 sample
/// tables and `MediaSource` expect.
fn nal_to_avcc(nal: &[u8]) -> Vec<u8> {
    let len = nal.len() as u32;
    let mut out = Vec::with_capacity(4 + nal.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(nal);
    out
}

// ---------------------------------------------------------------------------
// Fmp4Remuxer
// ---------------------------------------------------------------------------

/// A streaming fMP4 remuxer bound to a single browser connection.
///
/// Typical use:
///
/// ```ignore
/// let mut remuxer = Fmp4Remuxer::new();
/// loop {
///     let frame = rx.recv().await?;
///     let chunks = remuxer.push(&frame)?;
///     for chunk in chunks {
///         http_response_body.write_all(&chunk).await?;
///     }
/// }
/// ```
pub struct Fmp4Remuxer {
    /// `None` until the first keyframe arrives and SPS/PPS have been harvested.
    inner: Option<Inner>,
    /// Configured fragment duration in milliseconds.
    fragment_duration_ms: u32,
    /// SPS NAL harvested from the stream, kept until PPS arrives (or vice
    /// versa). The hub delivers one NAL per `MediaFrame`, so SPS and PPS
    /// arrive as separate frames and must be accumulated across pushes.
    pending_sps: Option<Vec<u8>>,
    pending_pps: Option<Vec<u8>>,
}

struct Inner {
    muxer: FragmentedMuxer,
    /// Whether the init segment has been emitted to the caller yet.
    init_sent: bool,
    /// Absolute 90 kHz timestamp of this connection's first sample. muxide
    /// starts every muxer's baseMediaDecodeTime at 0, so the first flushed
    /// segment of a connection that joined mid-stream would carry tfdt=0 —
    /// backwards on the client's timeline. [`retime_first_segment`] fixes
    /// exactly that one segment (later segments already track absolute dts).
    first_base_ticks: u64,
    saw_sample: bool,
    first_segment_retimed: bool,
}

/// Rewrite the tfdt (v1) of a connection's FIRST segment to its absolute
/// timeline position (see [`Inner::first_base_ticks`]).
fn retime_first_segment(inner: &mut Inner, mut bytes: Vec<u8>) -> Vec<u8> {
    if inner.first_segment_retimed || !inner.saw_sample {
        return bytes;
    }
    let base = inner.first_base_ticks;
    if base == 0 {
        inner.first_segment_retimed = true;
        return bytes;
    }
    if let Some(pos) = bytes.windows(4).position(|w| w == b"tfdt")
        && let Some(value) = bytes.get_mut(pos + 8..pos + 16)
    {
        let mut b = [0u8; 8];
        b.copy_from_slice(value);
        let v = u64::from_be_bytes(b).saturating_add(base);
        value.copy_from_slice(&v.to_be_bytes());
        inner.first_segment_retimed = true;
    }
    bytes
}

/// Output chunks produced by [`Fmp4Remuxer::push`]. Drained in order and
/// written to the HTTP response body.
#[derive(Debug)]
pub enum Fmp4Chunk {
    /// The init segment — emit first, exactly once.
    Init(Vec<u8>),
    /// A completed media segment (moof + mdat).
    Segment(Vec<u8>),
}

impl Fmp4Chunk {
    pub fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Init(b) | Self::Segment(b) => b,
        }
    }
}

impl Fmp4Remuxer {
    /// Create a new remuxer with a ~1-second fragment duration.
    pub fn new() -> Self {
        Self::with_fragment_duration_ms(1000)
    }

    /// Create a new remuxer with a custom target fragment duration.
    pub fn with_fragment_duration_ms(fragment_duration_ms: u32) -> Self {
        Self {
            inner: None,
            fragment_duration_ms,
            pending_sps: None,
            pending_pps: None,
        }
    }

    /// Pre-seed the remuxer with cached SPS/PPS so it can build the init
    /// segment immediately, without waiting for the next IDR to carry fresh
    /// parameter sets. Safe to call before the first [`push`](Self::push);
    /// any subsequently harvested SPS/PPS from the stream overrides the seed.
    pub fn seed_sps_pps(&mut self, sps: Vec<u8>, pps: Vec<u8>) {
        if self.inner.is_none() {
            self.pending_sps = Some(sps);
            self.pending_pps = Some(pps);
        }
    }

    /// Push an encoded frame and return any init/segment chunks ready to emit.
    ///
    /// Non-keyframe frames received before the first keyframe are dropped
    /// (the browser cannot decode without SPS/PPS + IDR).
    pub fn push(&mut self, frame: &MediaFrame) -> Result<Vec<Fmp4Chunk>> {
        let MediaFrame::Video {
            data,
            keyframe,
            timestamp,
        } = frame
        else {
            return Ok(Vec::new());
        };

        // Lazily initialise the muxer once we have both SPS and PPS. The hub
        // delivers one NAL per `MediaFrame`, so an IDR access unit (SPS, PPS,
        // IDR slice) arrives as three separate frames; we must accumulate the
        // parameter sets across pushes.
        if self.inner.is_none() {
            // Harvest SPS/PPS from this NAL if it carries one.
            match nal_type(data[0]) {
                7 if self.pending_sps.is_none() => self.pending_sps = Some(data.clone()),
                8 if self.pending_pps.is_none() => self.pending_pps = Some(data.clone()),
                _ => {}
            }
            // Need both to build the init segment. Check first, then take —
            // `take()` in a tuple-binding would steal the SPS even when the
            // PPS side is still None, losing the buffered parameter set.
            if self.pending_sps.is_some() && self.pending_pps.is_some() {
                let sps = self.pending_sps.take().expect("checked above");
                let pps = self.pending_pps.take().expect("checked above");
                self.init_with_sps_pps(sps, pps)?;
            } else {
                // Still waiting for the other parameter set. We can't emit
                // anything yet — the browser can't decode without the init
                // segment. Drop this frame; non-keyframe frames are also
                // useless until init completes.
                return Ok(Vec::new());
            }
        }

        let inner = self.inner.as_mut().expect("initialised above");
        let mut out = Vec::new();

        // Emit the init segment once, right after construction.
        if !inner.init_sent {
            out.push(Fmp4Chunk::Init(inner.muxer.init_segment()));
            inner.init_sent = true;
        }

        // The hub delivers one NAL per MediaFrame, so the SPS/PPS/SEI that
        // precede each IDR arrive as standalone frames. Writing them as
        // samples produces keyframe-flagged, picture-less access units that
        // Chrome's decode pipeline rejects (PIPELINE_ERROR_DECODE) — the
        // parameter sets already live in the init segment. Skip them.
        if matches!(nal_type(data[0]), 6..=8) {
            return Ok(out);
        }

        // Convert this NAL to AVCC and queue it as a sample. Frame PTS is
        // ms since camera-stream start and is passed through un-rebased:
        // every (re)connection of a client lands on the SAME timeline, so
        // a transparent reconnect can keep appending to its existing
        // SourceBuffer — the seamless-reconnect contract (SPEC §4.1).
        let avcc = nal_to_avcc(data);
        let pts_ticks = ms_to_ticks(*timestamp);
        if !inner.saw_sample {
            inner.saw_sample = true;
            inner.first_base_ticks = pts_ticks;
        }
        inner
            .muxer
            .write_video(pts_ticks, pts_ticks, &avcc, *keyframe)
            .context("fMP4 write_video failed")?;

        // Flush a segment whenever the muxer has buffered enough samples.
        if inner.muxer.ready_to_flush()
            && let Some(segment) = inner.muxer.flush_segment()
        {
            out.push(Fmp4Chunk::Segment(retime_first_segment(inner, segment)));
        }

        Ok(out)
    }

    /// Force-flush whatever samples are buffered, returning a segment if any.
    ///
    /// Useful when the caller wants to keep latency low (flush on every
    /// keyframe) regardless of the target fragment duration.
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        let inner = self.inner.as_mut()?;
        let segment = inner.muxer.flush_segment()?;
        Some(retime_first_segment(inner, segment))
    }

    fn init_with_sps_pps(&mut self, sps: Vec<u8>, pps: Vec<u8>) -> Result<()> {
        // Parse width/height out of the SPS so the muxer's track metadata is
        // correct. Fall back to 1280x720 if parsing fails — MSE decoders
        // re-derive geometry from the SPS itself, so an incorrect value in the
        // moov is tolerated.
        let (width, height) = protocols::h264::parse_sps(&sps)
            .map(|p| (p.width(), p.height()))
            .unwrap_or((1280, 720));

        let config = FragmentConfig {
            width,
            height,
            timescale: 90_000,
            fragment_duration_ms: self.fragment_duration_ms,
            sps,
            pps,
            vps: None,
            av1_sequence_header: None,
            vp9_config: None,
        };
        self.inner = Some(Inner {
            muxer: FragmentedMuxer::new(config),
            init_sent: false,
            first_base_ticks: 0,
            saw_sample: false,
            first_segment_retimed: false,
        });
        Ok(())
    }
}

impl Default for Fmp4Remuxer {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert milliseconds since stream start to 90 kHz timescale ticks.
fn ms_to_ticks(ms: u64) -> u64 {
    ms.saturating_mul(90_000) / 1_000
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nal_to_avcc_prepends_be_length() {
        let nal = [0x67, 0x42, 0x00, 0x1e]; // SPS-ish
        let avcc = nal_to_avcc(&nal);
        assert_eq!(avcc.len(), 4 + nal.len());
        // 4-byte big-endian length prefix.
        assert_eq!(&avcc[0..4], &[0, 0, 0, 4]);
        assert_eq!(&avcc[4..], &nal);
    }

    #[test]
    fn nal_type_masks_low_five_bits() {
        assert_eq!(nal_type(0x67), 7); // SPS
        assert_eq!(nal_type(0x68), 8); // PPS
        assert_eq!(nal_type(0x65), 5); // IDR
    }

    #[test]
    fn ms_to_ticks_converts_at_90khz() {
        assert_eq!(ms_to_ticks(0), 0);
        assert_eq!(ms_to_ticks(1000), 90_000); // 1s
        assert_eq!(ms_to_ticks(33), 2970); // ~1 frame @30fps
    }

    #[test]
    fn remuxer_drops_non_keyframes_before_init() {
        let mut r = Fmp4Remuxer::new();
        let frame = MediaFrame::Video {
            data: vec![0x61, 0x00], // P-slice, not a keyframe
            keyframe: false,
            timestamp: 10,
        };
        let chunks = r.push(&frame).expect("push ok");
        assert!(chunks.is_empty(), "no chunks before first keyframe");
    }

    #[test]
    fn remuxer_accumulates_sps_and_pps_across_separate_frames() {
        let mut r = Fmp4Remuxer::new();
        let sps = vec![0x67, 0x42, 0x00, 0x0a, 0xf8, 0x41, 0xa2];
        let pps = vec![0x68, 0xce, 0x38, 0x80];

        // SPS arrives alone — no output yet, but must be buffered.
        let f1 = MediaFrame::Video {
            data: sps,
            keyframe: true,
            timestamp: 0,
        };
        assert!(r.push(&f1).unwrap().is_empty(), "no init until PPS arrives");

        // PPS arrives — now both are present, init segment should emit.
        let f2 = MediaFrame::Video {
            data: pps,
            keyframe: true,
            timestamp: 10,
        };
        let chunks = r.push(&f2).unwrap();
        assert!(
            chunks.iter().any(|c| matches!(c, Fmp4Chunk::Init(_))),
            "init segment should emit once SPS+PPS are both buffered"
        );
    }

    // ── seamless-reconnect contract (SPEC §4.1) ──────────────────────────────

    /// Extract the 64-bit baseMediaDecodeTime from the first tfdt (v1) box.
    fn parse_tfdt_v1(segment: &[u8]) -> Option<u64> {
        let pos = segment.windows(4).position(|w| w == b"tfdt")?;
        let bytes = segment.get(pos + 8..pos + 16)?;
        let mut b = [0u8; 8];
        b.copy_from_slice(bytes);
        Some(u64::from_be_bytes(b))
    }

    #[test]
    fn remuxer_timeline_continues_across_connections() {
        let sps = vec![0x67, 0x42, 0x00, 0x0a, 0xf8, 0x41, 0xa2];
        let pps = vec![0x68, 0xce, 0x38, 0x80];
        let idr = vec![0x65, 0x88, 0x84, 0x00];

        // Drive a remuxer over the given frames and return the tfdt of the
        // LAST media segment it produced (segments may flush inside push()).
        fn last_segment_tfdt(r: &mut Fmp4Remuxer, frames: &[(u64, Vec<u8>)]) -> u64 {
            let mut last: Option<u64> = None;
            for (ms, nal) in frames {
                for chunk in r
                    .push(&MediaFrame::Video {
                        keyframe: true,
                        data: nal.clone(),
                        timestamp: *ms,
                    })
                    .expect("push ok")
                {
                    if let Fmp4Chunk::Segment(bytes) = chunk {
                        last = parse_tfdt_v1(&bytes);
                    }
                }
            }
            if let Some(bytes) = r.flush() {
                last = parse_tfdt_v1(&bytes);
            }
            last.expect("at least one media segment")
        }

        // Connection 1 joins the camera stream at t=0, sees frames to t=1s.
        let t1 = last_segment_tfdt(
            &mut Fmp4Remuxer::new(),
            &[
                (0, sps.clone()),
                (0, pps.clone()),
                (0, idr.clone()),
                (1000, idr.clone()),
            ],
        );

        // Connection 2 (transparent client reconnect) joins the SAME camera
        // stream 4s later — its timeline must continue past connection 1's,
        // not restart at zero, so the client can keep appending to its
        // existing SourceBuffer.
        let t2 = last_segment_tfdt(
            &mut Fmp4Remuxer::new(),
            &[(5000, sps), (5000, pps), (5000, idr)],
        );

        assert!(
            t2 > t1,
            "second connection must continue the timeline: {t2} <= {t1}"
        );
    }
}
