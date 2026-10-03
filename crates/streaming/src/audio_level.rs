//! Microphone level meter for the real-time voice waveform (SPEC §6
//! `audio_level` event, notebook dialect #36).
//!
//! A lightweight tap on the 16 kHz mono monitor broadcast: RMS per window,
//! attack/release smoothing, and a minimum emit interval so the SSE bus
//! carries at most ~10 updates/second regardless of the chunk cadence.
//! Pure state machine — the main.rs wiring just feeds chunks in and sends
//! whatever comes out.

/// Smoothed mic level 0..=1 plus the emit policy.
pub struct LevelMeter {
    /// Samples accumulated since the last emit window.
    pending: usize,
    pending_sq: f64,
    /// Last emitted smoothed level (attack fast, release slow).
    level: f32,
    /// Monotonic ms at last emit; `None` until the first emit (the very
    /// first full window is never throttled). Caller-supplied clock keeps
    /// the throttle deterministic in tests.
    last_emit_ms: Option<u64>,
}

const WINDOW_SAMPLES: usize = 1600; // 100 ms @ 16 kHz
const MIN_EMIT_INTERVAL_MS: u64 = 100;
const ATTACK: f32 = 0.6;
const RELEASE: f32 = 0.3;
const FLOOR: f32 = 0.0015; // ≈ −56 dBFS: room noise does not flicker the bar

impl Default for LevelMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl LevelMeter {
    pub fn new() -> Self {
        Self {
            pending: 0,
            pending_sq: 0.0,
            level: 0.0,
            last_emit_ms: None,
        }
    }

    /// Feed one monitor chunk; returns the smoothed level when a new value
    /// should be published (window full AND the emit interval elapsed).
    pub fn update(&mut self, samples: &[i16], now_ms: u64) -> Option<f32> {
        for s in samples {
            let f = f64::from(*s) / 32768.0;
            self.pending_sq += f * f;
        }
        self.pending += samples.len();
        if self.pending < WINDOW_SAMPLES {
            return None;
        }
        if self
            .last_emit_ms
            .is_some_and(|t| now_ms.saturating_sub(t) < MIN_EMIT_INTERVAL_MS)
        {
            // Interval not elapsed: keep accumulating (the window average
            // just spans slightly more than 100 ms — harmless).
            return None;
        }
        let rms = (self.pending_sq / self.pending as f64).sqrt() as f32;
        self.pending = 0;
        self.pending_sq = 0.0;
        self.last_emit_ms = Some(now_ms);
        let target = rms.clamp(0.0, 1.0);
        // Speech bursts must light up instantly; decay can lag — a
        // symmetric filter makes the wave look sluggish on stop.
        let alpha = if target > self.level { ATTACK } else { RELEASE };
        self.level = self.level + alpha * (target - self.level);
        if self.level < FLOOR {
            self.level = 0.0;
        }
        Some(self.level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn silence(n: usize) -> Vec<i16> {
        vec![0; n]
    }

    fn loud(n: usize) -> Vec<i16> {
        // Near-full-scale triangle wave: RMS ≈ 0.4 — like close speech.
        (0..n)
            .map(|i| ((i % 7) as f32 / 7.0 * 60000.0 - 30000.0) as i16)
            .collect()
    }

    #[test]
    fn emits_at_most_every_100ms_and_needs_a_full_window() {
        let mut m = LevelMeter::new();
        // Half a window → nothing.
        assert!(m.update(&silence(800), 0).is_none());
        // The FIRST full window emits immediately (no throttle before the
        // first publish — the UI must not wait 100 ms of dead air).
        assert!(m.update(&silence(800), 0).is_some());
        // Window full again but only +50ms since the last emit → throttled.
        assert!(m.update(&loud(1600), 50).is_none());
        // At +120ms → emits again.
        assert!(m.update(&loud(1600), 130).is_some());
    }

    #[test]
    fn attack_is_fast_release_is_slow() {
        let mut m = LevelMeter::new();
        let l1 = m.update(&loud(1600), 0).unwrap();
        let l2 = m.update(&loud(1600), 150).unwrap();
        let l3 = m.update(&loud(1600), 300).unwrap();
        assert!(l1 > 0.1, "first loud window lights up: {l1}");
        assert!(
            l2 >= l1 && l3 >= l2,
            "sustained input rises: {l1} {l2} {l3}"
        );
        // Input stops: decays but not instantly to zero.
        let d1 = m.update(&silence(1600), 450).unwrap();
        assert!(d1 < l3 && d1 > 0.0, "release is gradual: {l3} -> {d1}");
        // Long silence eventually floors to exactly zero (idle suppression).
        let mut m2 = LevelMeter::new();
        m2.update(&loud(1600), 0);
        let mut last = 1.0;
        for t in (0..25).map(|i| 150 + i * 150) {
            if let Some(v) = m2.update(&silence(1600), t) {
                last = v;
            }
        }
        assert_eq!(last, 0.0, "long silence floors the level");
    }

    #[test]
    fn silent_start_emits_zero_not_noise() {
        let mut m = LevelMeter::new();
        assert_eq!(m.update(&silence(3200), 0), Some(0.0));
    }
}
