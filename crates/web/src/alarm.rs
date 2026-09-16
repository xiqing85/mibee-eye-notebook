//! AI detection → alarm rising-edge bridge (SPEC v1 §6 `alarm` event,
//! GB/T 28181-2022 §9.5.2 Alarm NOTIFY).
//!
//! Family semantics mirrored from the mibee-eye-rs/go products: fire on
//! the rising edge only (idle → some detections), re-arm on the falling
//! edge, and drop re-fires within a cooldown window so a busy scene
//! cannot spam the platform and the browser. A rising edge that lands
//! inside the cooldown is dropped entirely (not deferred) — the next
//! idle→active transition fires again.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// One fired alarm: everything the SSE event and the GB28181 NOTIFY need.
#[derive(Debug, Clone, PartialEq)]
pub struct AlarmSignal {
    pub camera_id: String,
    /// Detection count at fire time (SPEC §6 `targets`).
    pub targets: usize,
    /// Unix epoch milliseconds captured at fire time.
    pub timestamp_ms: u64,
}

/// Per-camera rising-edge state machine with a fire cooldown.
#[derive(Debug)]
pub struct AlarmBridge {
    was_active: HashMap<String, bool>,
    last_fire: HashMap<String, Instant>,
    cooldown: Duration,
}

impl AlarmBridge {
    pub fn new(cooldown: Duration) -> Self {
        Self {
            was_active: HashMap::new(),
            last_fire: HashMap::new(),
            cooldown,
        }
    }

    /// Feed one detection sample. Returns a signal on a qualifying rising
    /// edge; `None` for a falling edge, steady idle, or a rising edge
    /// inside the cooldown window.
    pub fn observe(
        &mut self,
        camera_id: &str,
        targets: usize,
        now: Instant,
        timestamp_ms: u64,
    ) -> Option<AlarmSignal> {
        let active = targets > 0;
        let was = *self.was_active.get(camera_id).unwrap_or(&false);
        self.was_active.insert(camera_id.to_string(), active);

        // Falling edge and steady idle never fire; they re-arm the camera.
        if !active || was {
            return None;
        }

        // Rising edge — drop if the camera fired inside the cooldown.
        if let Some(t) = self.last_fire.get(camera_id)
            && now.duration_since(*t) < self.cooldown
        {
            return None;
        }
        self.last_fire.insert(camera_id.to_string(), now);
        Some(AlarmSignal {
            camera_id: camera_id.to_string(),
            targets,
            timestamp_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: u64) -> Instant {
        // A fixed anchor; only elapsed deltas matter to the bridge.
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *START.get_or_init(Instant::now) + Duration::from_secs(secs)
    }

    #[test]
    fn rising_edge_fires() {
        let mut b = AlarmBridge::new(Duration::from_secs(30));
        let sig = b.observe("0", 2, t(0), 1_000);
        assert_eq!(
            sig,
            Some(AlarmSignal {
                camera_id: "0".into(),
                targets: 2,
                timestamp_ms: 1_000,
            })
        );
    }

    #[test]
    fn steady_active_does_not_refire() {
        let mut b = AlarmBridge::new(Duration::from_secs(30));
        assert!(b.observe("0", 2, t(0), 1_000).is_some());
        assert!(b.observe("0", 3, t(1), 2_000).is_none());
        assert!(b.observe("0", 1, t(100), 3_000).is_none());
    }

    #[test]
    fn falling_edge_rearms_and_next_rising_fires() {
        let mut b = AlarmBridge::new(Duration::from_secs(30));
        assert!(b.observe("0", 2, t(0), 1_000).is_some());
        assert!(b.observe("0", 0, t(1), 2_000).is_none()); // falling: silent
        assert!(b.observe("0", 1, t(2), 3_000).is_none()); // within cooldown: dropped
        assert!(b.observe("0", 0, t(3), 4_000).is_none());
        assert!(b.observe("0", 4, t(100), 5_000).is_some()); // re-armed past cooldown
    }

    #[test]
    fn rising_edge_inside_cooldown_is_dropped() {
        let mut b = AlarmBridge::new(Duration::from_secs(30));
        assert!(b.observe("0", 1, t(0), 1_000).is_some());
        assert!(b.observe("0", 0, t(1), 2_000).is_none());
        assert!(b.observe("0", 5, t(2), 3_000).is_none()); // 1s later < 30s cooldown
    }

    #[test]
    fn steady_idle_never_fires() {
        let mut b = AlarmBridge::new(Duration::from_secs(30));
        assert!(b.observe("0", 0, t(0), 1_000).is_none());
        assert!(b.observe("0", 0, t(1000), 2_000).is_none());
    }

    #[test]
    fn cameras_are_independent() {
        let mut b = AlarmBridge::new(Duration::from_secs(30));
        assert!(b.observe("0", 1, t(0), 1_000).is_some());
        assert!(b.observe("1", 1, t(0), 1_000).is_some());
        assert!(b.observe("0", 0, t(1), 2_000).is_none());
        assert!(b.observe("0", 2, t(2), 3_000).is_none()); // cam 0 in cooldown
        assert!(b.observe("1", 0, t(1), 2_000).is_none());
        assert!(b.observe("1", 2, t(200), 9_000).is_some()); // cam 1 independent
    }
}
