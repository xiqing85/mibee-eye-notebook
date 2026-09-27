//! Per-class sound-event state machine: window voting, hysteresis, rising
//! edge and per-class cooldown.
//!
//! Semantics deliberately mirror [`crate::web`-side `AlarmBridge`]'s
//! family contract (fire on the rising edge only, re-arm on the falling
//! edge, drop in-cooldown re-fires) — extended with a 3-patch mean vote and
//! hysteresis so single-window blips and borderline scores never alarm.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::SoundEvent;
use super::labels::WatchedClass;

/// Per-class rising-edge state with voting, hysteresis and cooldown.
#[derive(Debug)]
pub struct SoundEventState {
    watched: Vec<ClassWatcher>,
    enter_threshold: f32,
    cooldown: Duration,
}

#[derive(Debug)]
struct ClassWatcher {
    spec: WatchedClass,
    window: VecDeque<f32>,
    active: bool,
    last_fire: Option<Instant>,
}

impl SoundEventState {
    /// Build the state machine from resolved watched classes.
    #[must_use]
    pub fn new(watched: Vec<WatchedClass>, threshold: f32, cooldown: Duration) -> Self {
        Self {
            watched: watched
                .into_iter()
                .map(|spec| ClassWatcher {
                    spec,
                    window: VecDeque::with_capacity(super::VOTE_PATCHES),
                    active: false,
                    last_fire: None,
                })
                .collect(),
            enter_threshold: threshold.clamp(0.01, 1.0),
            cooldown,
        }
    }

    /// The watched class names (for logging / capability surface).
    #[must_use]
    pub fn watched(&self) -> Vec<&str> {
        self.watched.iter().map(|w| w.spec.name.as_str()).collect()
    }

    /// Feed one window's class scores; returns the events that fired.
    ///
    /// A class fires when the mean of the last [`VOTE_PATCHES`] windows
    /// crosses the enter threshold from below (rising edge) and its
    /// per-class cooldown has expired. It re-arms once the voted mean drops
    /// to half the enter threshold (hysteresis).
    pub fn update(&mut self, scores: &[f32], now: Instant, timestamp_ms: u64) -> Vec<SoundEvent> {
        let mut fired = Vec::new();
        for w in &mut self.watched {
            let score = scores.get(w.spec.index).copied().unwrap_or(0.0);
            w.window.push_back(score);
            while w.window.len() > super::VOTE_PATCHES {
                w.window.pop_front();
            }
            // Warm-up: a half-filled window must not vote (a single loud
            // patch is exactly the blip this machine exists to reject).
            if w.window.len() < super::VOTE_PATCHES {
                continue;
            }
            let mean = w.window.iter().sum::<f32>() / w.window.len() as f32;
            let exit_threshold = 0.5 * self.enter_threshold;

            if w.active {
                if mean <= exit_threshold {
                    w.active = false; // falling edge: silent re-arm
                }
            } else if mean >= self.enter_threshold {
                w.active = true;
                let cooled = w
                    .last_fire
                    .is_none_or(|t| now.duration_since(t) >= self.cooldown);
                if cooled {
                    w.last_fire = Some(now);
                    fired.push(SoundEvent {
                        class: w.spec.name.clone(),
                        label_zh: w.spec.label_zh.clone(),
                        score: mean,
                        timestamp_ms,
                    });
                }
            }
        }
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::super::labels::WatchedClass;
    use super::*;

    fn spec(name: &str, index: usize) -> WatchedClass {
        WatchedClass {
            name: name.into(),
            label_zh: "测试".into(),
            index,
        }
    }

    fn t(secs: u64) -> Instant {
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *START.get_or_init(Instant::now) + Duration::from_secs(secs)
    }

    /// Sparse score vector: 0.0 everywhere except `index`.
    fn scores(index: usize, value: f32) -> Vec<f32> {
        let mut v = vec![0.0_f32; 521];
        v[index] = value;
        v
    }

    #[test]
    fn single_patch_blip_never_fires() {
        let mut s = SoundEventState::new(vec![spec("Dog", 75)], 0.3, Duration::from_secs(30));
        assert!(s.update(&scores(75, 0.9), t(0), 1).is_empty());
        assert!(s.update(&scores(75, 0.0), t(1), 2).is_empty());
        assert!(s.update(&scores(75, 0.0), t(2), 3).is_empty());
    }

    #[test]
    fn sustained_class_fires_on_third_patch() {
        let mut s = SoundEventState::new(vec![spec("Dog", 75)], 0.3, Duration::from_secs(30));
        assert!(s.update(&scores(75, 0.6), t(0), 1).is_empty());
        assert!(s.update(&scores(75, 0.6), t(1), 2).is_empty());
        let events = s.update(&scores(75, 0.6), t(2), 3);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].class, "Dog");
        assert!((events[0].score - 0.6).abs() < 1e-6);
        assert_eq!(events[0].timestamp_ms, 3);
    }

    #[test]
    fn steady_active_does_not_refire() {
        let mut s = SoundEventState::new(vec![spec("Dog", 75)], 0.3, Duration::from_secs(30));
        for i in 0..5 {
            let ev = s.update(&scores(75, 0.8), t(i), 1_000 + i);
            if i < 2 {
                assert!(ev.is_empty());
            } else if i == 2 {
                assert_eq!(ev.len(), 1);
            } else {
                assert!(ev.is_empty(), "steady active must not re-fire");
            }
        }
    }

    #[test]
    fn cooldown_drops_refire_until_expired() {
        let mut s = SoundEventState::new(vec![spec("Dog", 75)], 0.3, Duration::from_secs(30));
        for i in 0..3 {
            let ev = s.update(&scores(75, 0.8), t(i), 1_000 + i);
            assert_eq!(ev.len(), usize::from(i == 2), "fires exactly once at t=2");
        }
        // Hysteresis falling edge needs a fully-voted-down window: three
        // zero patches take the class inactive at t=6.
        for i in 4..7 {
            s.update(&scores(75, 0.0), t(i), 2_000);
        }
        // Rise again inside the cooldown: the vote clearly crosses
        // (mean 0.33) but the fire is dropped (last fire t=2, now t=7).
        for i in 7..10 {
            assert!(
                s.update(&scores(75, 0.99), t(i), 3_000).is_empty(),
                "in-cooldown re-fire must be dropped (t={i})"
            );
        }
        // Fall again (three zeros), then rise after the cooldown expiry.
        for i in 10..13 {
            s.update(&scores(75, 0.0), t(i), 4_000);
        }
        let events = s.update(&scores(75, 0.99), t(40), 8_000);
        assert_eq!(events.len(), 1, "fires again after cooldown expiry");
        assert!((events[0].score - 0.33).abs() < 1e-3);
    }

    #[test]
    fn hysteresis_prevents_borderline_flapping() {
        // Scores oscillating between the enter and exit thresholds hold
        // the active state without re-firing (no falling edge happens).
        let mut s = SoundEventState::new(vec![spec("Siren", 396)], 0.4, Duration::from_secs(30));
        for i in 0..3 {
            s.update(&scores(396, 0.6), t(i), i);
        }
        let mut fires = 0;
        for i in 4..12 {
            let v = if i % 2 == 0 { 0.3 } else { 0.2 }; // mean stays in (0.2, 0.4)
            fires += s.update(&scores(396, v), t(i), i).len();
        }
        assert_eq!(fires, 0, "scores above exit but below enter must not flap");
    }

    #[test]
    fn classes_are_independent() {
        let mut s = SoundEventState::new(
            vec![spec("Dog", 75), spec("Siren", 396)],
            0.3,
            Duration::from_secs(30),
        );
        // Dog fires; Siren never scored.
        for i in 0..3 {
            s.update(&scores(75, 0.8), t(i), i);
        }
        // Now Siren rises while Dog is in cooldown: Siren's vote fills at
        // the second combined patch (t=4) and fires independently.
        for i in 3..5 {
            let mut v = vec![0.0_f32; 521];
            v[75] = 0.8;
            v[396] = 0.8;
            let ev = s.update(&v, t(i), 1_000);
            if i == 4 {
                assert_eq!(ev.len(), 1);
                assert_eq!(ev[0].class, "Siren");
            } else {
                assert!(ev.is_empty());
            }
        }
    }
}
