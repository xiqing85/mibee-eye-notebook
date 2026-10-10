//! Away mode (SPEC v1 §3.6, notebook dialect appendix A #44): the
//! armed watch loop over the existing AI detection stream.
//!
//! The engine owns the pure decision state — arm/disarm flag, per-camera
//! person presence latches (arrival gap + greeting cooldown), activity
//! cooldowns, the away analysis interval throttle, and the single
//! device-wide listening slot (one microphone). The orchestration task
//! in the binary (`mibee_eye::away_monitor`) feeds it detection events,
//! runs the voice/VLM/desktop legs its decisions trigger, and persists
//! records through [`crate::db`].
//!
//! Semantics mirror the family alarm bridge (`crate::alarm`): rising
//! edges only, edges inside a cooldown are dropped (not deferred), the
//! falling edge re-arms.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use serde::Deserialize;
use serde::Serialize;
use streaming::ai::AiEngine;
use streaming::ai::Detection;

/// `[away]` config (SPEC appendix A #44). Boot-time TOML, restart to
/// change — like `[agent]`/`[conversations]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AwayConfig {
    /// Minimum spacing between away-mode analyses per camera, stacked on
    /// top of the `[ai] interval_ms` detection cadence (the away loop
    /// never samples faster than the AI worker).
    #[serde(default = "default_interval_ms")]
    pub interval_ms: u64,
    /// Person absence grace — a re-detection inside this window is the
    /// same visit, not a new arrival (detection flicker).
    #[serde(default = "default_person_gap_secs")]
    pub person_gap_secs: u64,
    /// Minimum spacing between visitor interactions per camera — a
    /// lingering person is greeted at most this often.
    #[serde(default = "default_greeting_cooldown_secs")]
    pub greeting_cooldown_secs: u64,
    /// Minimum spacing between activity records per camera.
    #[serde(default = "default_activity_cooldown_secs")]
    pub activity_cooldown_secs: u64,
    /// Non-person labels that count as activity while armed. Defaults to
    /// pets — empty the list if the animal lives here.
    #[serde(default = "default_activity_labels")]
    pub activity_labels: Vec<String>,
    /// One-shot no-wake-word listening window after the greeting
    /// (seconds). Does not touch `follow_up_window_secs`.
    #[serde(default = "default_listen_secs")]
    pub listen_secs: u64,
    /// Directory for evidence snapshots (cwd-relative, like `models/`).
    #[serde(default = "default_snapshot_dir")]
    pub snapshot_dir: String,
    /// Greeting for an enrolled face; `{name}` is replaced.
    #[serde(default = "default_greeting_known")]
    pub greeting_known: String,
    /// Greeting + identity question for an unknown visitor.
    #[serde(default = "default_greeting_unknown")]
    pub greeting_unknown: String,
}

fn default_interval_ms() -> u64 {
    1000
}
fn default_person_gap_secs() -> u64 {
    10
}
fn default_greeting_cooldown_secs() -> u64 {
    120
}
fn default_activity_cooldown_secs() -> u64 {
    60
}
fn default_activity_labels() -> Vec<String> {
    ["cat", "dog", "bird"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}
fn default_listen_secs() -> u64 {
    10
}
fn default_snapshot_dir() -> String {
    "away-snapshots".to_string()
}
fn default_greeting_known() -> String {
    "欢迎回家，{name}。".to_string()
}
fn default_greeting_unknown() -> String {
    "你好，这里是主人的智能看家助手。主人现在不在家，请问你是谁？".to_string()
}

impl Default for AwayConfig {
    fn default() -> Self {
        Self {
            interval_ms: default_interval_ms(),
            person_gap_secs: default_person_gap_secs(),
            greeting_cooldown_secs: default_greeting_cooldown_secs(),
            activity_cooldown_secs: default_activity_cooldown_secs(),
            activity_labels: default_activity_labels(),
            listen_secs: default_listen_secs(),
            snapshot_dir: default_snapshot_dir(),
            greeting_known: default_greeting_known(),
            greeting_unknown: default_greeting_unknown(),
        }
    }
}

/// One persisted away event (SPEC §3.6 event object — the API list item
/// and the SSE `away_event` payload are the same shape; the SSE update
/// carries the full record so the browser upserts by `id`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AwayEventRecord {
    pub id: i64,
    pub camera_id: String,
    /// `"person"` (visitor pipeline) | `"activity"`.
    pub kind: String,
    pub started_ms: i64,
    /// Detection summary at fire time, e.g. `person×1`.
    pub labels: String,
    /// Enrolled-face match (person events; `None` = unknown/absent).
    pub face_name: Option<String>,
    /// VLM description — patched in asynchronously when it lands.
    pub description: Option<String>,
    /// The visitor's spoken answer inside the listen window.
    pub visitor_reply: Option<String>,
    /// Server-generated snapshot file name (never a client path).
    pub snapshot: Option<String>,
    /// `greeting` → `listening` → `answered` | `silent`; terminal:
    /// `known` / `no_voice` / `recorded`.
    pub state: String,
}

/// What the engine wants the orchestrator to do for one detection
/// sample — the decision is pure, the legs (snapshot, face, TTS, VLM,
/// records) belong to the async side.
#[derive(Debug, Clone, PartialEq)]
pub enum AwayAction {
    /// New person arrival outside both gap and cooldown — run the full
    /// visitor pipeline.
    Visitor {
        person_count: usize,
    },
    /// Configured activity label rising edge outside its cooldown —
    /// record (with snapshot), no voice.
    Activity {
        labels: String,
    },
    None,
}

#[derive(Debug, Default)]
struct CameraLatch {
    /// Last unix-ms a person was detected on this camera. `None` until
    /// the first sight — a 0-stamp would read as "seen a moment ago"
    /// for timestamps inside the gap window and swallow the first
    /// arrival.
    person_seen_ms: Option<u64>,
    /// Last unix-ms a visitor event fired on this camera.
    person_greeted_ms: Option<u64>,
    /// Last unix-ms an away analysis ran (interval throttle).
    analysis_ms: Option<u64>,
    /// Last unix-ms an activity record landed.
    activity_ms: Option<u64>,
}

#[derive(Debug)]
struct OpenListen {
    event_id: i64,
    deadline_ms: u64,
}

#[derive(Debug, Default)]
struct AwayInner {
    cameras: HashMap<String, CameraLatch>,
    /// The single no-wake-word listening window (one microphone — a
    /// second visitor while the slot is open records without voice).
    listen: Option<OpenListen>,
}

/// Process-wide away-mode engine: armed flag + decision state machine.
/// Clone-free shared state (`Arc<AwayEngine>` everywhere); the inner
/// map sits behind a `std::sync::Mutex` — decisions are synchronous and
/// short (no await while held).
pub struct AwayEngine {
    config: AwayConfig,
    /// Live AI-detection availability — the away surface cannot arm
    /// without it (capability `away.available`).
    ai: Arc<AiEngine>,
    /// Whether the voice legs can run at all (TTS + voice engines
    /// active at boot; capability `away.voice`).
    voice_capable: bool,
    armed: AtomicBool,
    armed_since_ms: AtomicU64,
    inner: Mutex<AwayInner>,
}

impl AwayEngine {
    #[must_use]
    pub fn new(config: AwayConfig, ai: Arc<AiEngine>, voice_capable: bool) -> Self {
        Self {
            config,
            ai,
            voice_capable,
            armed: AtomicBool::new(false),
            armed_since_ms: AtomicU64::new(0),
            inner: Mutex::new(AwayInner::default()),
        }
    }

    #[must_use]
    pub fn config(&self) -> &AwayConfig {
        &self.config
    }

    /// Whether the device can arm at all (AI detection active).
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.ai.is_active()
    }

    /// Why arming is refused (empty when available) — surfaced by the
    /// arm endpoint so the UI can show the honest reason.
    #[must_use]
    pub fn unavailable_reason(&self) -> String {
        self.ai.inactive_reason().to_string()
    }

    #[must_use]
    pub fn voice_capable(&self) -> bool {
        self.voice_capable
    }

    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn armed_since_ms(&self) -> Option<u64> {
        if self.is_armed() {
            Some(self.armed_since_ms.load(Ordering::SeqCst))
        } else {
            None
        }
    }

    /// Arm the watch (no-op when already armed).
    pub fn arm(&self, now_ms: u64) {
        if !self.armed.swap(true, Ordering::SeqCst) {
            self.armed_since_ms.store(now_ms, Ordering::SeqCst);
        }
    }

    /// Disarm (no-op when already disarmed; the since stamp resets so a
    /// later arm starts fresh). Any open listening window drops too — a
    /// disarmed device stops asking questions.
    pub fn disarm(&self) {
        self.armed.store(false, Ordering::SeqCst);
        self.armed_since_ms.store(0, Ordering::SeqCst);
        self.inner.lock().expect("away inner lock").listen = None;
    }

    /// Feed one detection sample while armed. Presence latches always
    /// update; record decisions are throttled to `interval_ms` and gated
    /// by the arrival gap / cooldowns. Detection labels come from the
    /// engine's own COCO vocabulary — `person` is the visitor trigger.
    pub fn observe(&self, camera_id: &str, detections: &[Detection], now_ms: u64) -> AwayAction {
        let gap_ms = self.config.person_gap_secs * 1000;
        let cooldown_ms = self.config.greeting_cooldown_secs * 1000;
        let activity_ms = self.config.activity_cooldown_secs * 1000;
        let interval_ms = self.config.interval_ms.max(1);
        let mut inner = self.inner.lock().expect("away inner lock");
        let latch = inner.cameras.entry(camera_id.to_string()).or_default();
        // Interval throttle — a camera's first sample is never throttled.
        let throttled = latch
            .analysis_ms
            .is_some_and(|t| now_ms.saturating_sub(t) < interval_ms);

        let person_count = detections.iter().filter(|d| d.label == "person").count();
        if person_count > 0 {
            let last_seen = latch.person_seen_ms;
            latch.person_seen_ms = Some(now_ms);
            // Arrival = no person seen within the gap window. The very
            // first sight (None) is an arrival: someone IS here.
            let is_arrival = last_seen.is_none_or(|t| now_ms.saturating_sub(t) > gap_ms);
            let past_cooldown = latch
                .person_greeted_ms
                .is_none_or(|t| now_ms.saturating_sub(t) > cooldown_ms);
            if is_arrival && past_cooldown && !throttled {
                latch.person_greeted_ms = Some(now_ms);
                latch.analysis_ms = Some(now_ms);
                return AwayAction::Visitor { person_count };
            }
            return AwayAction::None;
        }

        // No person: configured activity labels (person presence
        // suppresses activity records — the visitor pipeline owns the
        // moment).
        let summary = summarize_labels(detections, &self.config.activity_labels);
        if !summary.is_empty()
            && latch
                .activity_ms
                .is_none_or(|t| now_ms.saturating_sub(t) > activity_ms)
            && !throttled
        {
            latch.activity_ms = Some(now_ms);
            latch.analysis_ms = Some(now_ms);
            return AwayAction::Activity { labels: summary };
        }
        AwayAction::None
    }

    /// Open the single listening slot for a visitor event. `false` when
    /// the slot is busy (single microphone — the caller records the
    /// event without voice).
    pub fn open_listen(&self, event_id: i64, deadline_ms: u64) -> bool {
        let mut inner = self.inner.lock().expect("away inner lock");
        if inner.listen.is_some() {
            return false;
        }
        inner.listen = Some(OpenListen {
            event_id,
            deadline_ms,
        });
        true
    }

    /// Attribute a follow-up transcript to the open listening window.
    /// Returns the event id exactly once (the slot closes on first
    /// attribution); `None` when no window is open or it already
    /// answered.
    pub fn attribute_listen(&self, now_ms: u64) -> Option<i64> {
        let mut inner = self.inner.lock().expect("away inner lock");
        match inner.listen.take() {
            Some(l) if now_ms <= l.deadline_ms => Some(l.event_id),
            // Expired but not yet reaped — treat as closed.
            Some(_) => None,
            None => None,
        }
    }

    /// Reap an expired unanswered window (the orchestrator's timer).
    /// Returns the event id when the window was still open and past its
    /// deadline — the caller flips it to `silent`.
    pub fn expire_listen(&self, now_ms: u64) -> Option<i64> {
        let mut inner = self.inner.lock().expect("away inner lock");
        // Only an expired window is reaped — a live one stays armed.
        if inner
            .listen
            .as_ref()
            .is_some_and(|l| now_ms > l.deadline_ms)
        {
            inner.listen.take().map(|l| l.event_id)
        } else {
            None
        }
    }

    /// Test seam: force a listening slot (integration tests).
    #[cfg(test)]
    pub fn force_listen(&self, event_id: i64, deadline_ms: u64) {
        self.inner.lock().expect("away inner lock").listen = Some(OpenListen {
            event_id,
            deadline_ms,
        });
    }
}

/// `label×count` summary over the configured activity set, in
/// detection order, deduplicated — `cat×2、dog×1`. Empty when nothing
/// matches (person is deliberately not an activity label).
#[must_use]
pub fn summarize_labels(detections: &[Detection], activity_labels: &[String]) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for d in detections {
        if activity_labels.iter().any(|l| l == &d.label) {
            let entry = counts.entry(d.label.as_str()).or_insert(0);
            if *entry == 0 {
                order.push(d.label.as_str());
            }
            *entry += 1;
        }
    }
    order
        .into_iter()
        .map(|label| format!("{label}×{}", counts[label]))
        .collect::<Vec<_>>()
        .join("、")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use streaming::ai::AiConfig;

    fn engine() -> AwayEngine {
        AwayEngine::new(
            AwayConfig::default(),
            Arc::new(AiEngine::from_parts(AiConfig::default(), None)),
            true,
        )
    }

    fn det(label: &str) -> Detection {
        Detection {
            label: label.to_string(),
            confidence: 0.9,
            bbox: [1, 2, 3, 4],
        }
    }

    #[test]
    fn config_defaults_roundtrip_toml() {
        let cfg = AwayConfig::default();
        assert_eq!(cfg.interval_ms, 1000);
        assert_eq!(cfg.person_gap_secs, 10);
        assert_eq!(cfg.greeting_cooldown_secs, 120);
        assert_eq!(cfg.activity_labels, vec!["cat", "dog", "bird"]);
        assert_eq!(cfg.listen_secs, 10);
        assert_eq!(cfg.snapshot_dir, "away-snapshots");
        assert!(cfg.greeting_known.contains("{name}"));
        // Empty document → all defaults (the boot TOML parse path; the
        // root crate owns the actual toml dep, serde defaults are the
        // shared contract).
        let again: AwayConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(again, cfg);
    }

    #[test]
    fn arm_disarm_tracks_since() {
        let e = engine();
        assert!(!e.is_armed());
        e.arm(1_000);
        assert!(e.is_armed());
        assert_eq!(e.armed_since_ms(), Some(1_000));
        e.arm(2_000); // re-arm keeps the original stamp
        assert_eq!(e.armed_since_ms(), Some(1_000));
        e.disarm();
        assert_eq!(e.armed_since_ms(), None);
    }

    #[test]
    fn person_arrival_fires_once_per_visit() {
        let e = engine();
        let d = vec![det("person")];
        // First sight = arrival.
        assert_eq!(
            e.observe("0", &d, 1_000),
            AwayAction::Visitor { person_count: 1 }
        );
        // Flicker within the gap = same visit.
        assert_eq!(e.observe("0", &d, 5_000), AwayAction::None);
        assert_eq!(e.observe("0", &d, 9_000), AwayAction::None);
        // Away past the gap but inside the greeting cooldown → dropped.
        assert_eq!(e.observe("0", &d, 30_000), AwayAction::None);
        // Left and came back past the cooldown → new visitor event.
        assert_eq!(
            e.observe("0", &d, 300_000),
            AwayAction::Visitor { person_count: 1 }
        );
    }

    #[test]
    fn steady_presence_then_departure_rearms() {
        let e = engine();
        let d = vec![det("person")];
        assert!(matches!(e.observe("0", &d, 0), AwayAction::Visitor { .. }));
        // Present continuously for minutes — one event only.
        for t in (1..200).map(|i| i * 1000) {
            assert_eq!(e.observe("0", &d, t), AwayAction::None);
        }
        // Gone well past the gap + cooldown, then returns → fires again.
        assert!(matches!(
            e.observe("0", &d, 600_000),
            AwayAction::Visitor { .. }
        ));
    }

    #[test]
    fn activity_labels_and_cooldown() {
        let e = engine();
        let cats = vec![det("cat"), det("cat"), det("dog")];
        assert_eq!(
            e.observe("0", &cats, 1_000),
            AwayAction::Activity {
                labels: "cat×2、dog×1".to_string()
            }
        );
        // Inside the activity cooldown → nothing.
        assert_eq!(e.observe("0", &cats, 30_000), AwayAction::None);
        assert_eq!(e.observe("0", &cats, 50_000), AwayAction::None);
        assert!(matches!(
            e.observe("0", &cats, 120_000),
            AwayAction::Activity { .. }
        ));
        // Unconfigured labels never count as activity.
        assert_eq!(e.observe("1", &[det("chair")], 1_000), AwayAction::None);
    }

    #[test]
    fn person_suppresses_activity() {
        let e = engine();
        let mixed = vec![det("person"), det("cat")];
        assert!(matches!(
            e.observe("0", &mixed, 1_000),
            AwayAction::Visitor { .. }
        ));
        assert_eq!(e.observe("0", &mixed, 2_000), AwayAction::None);
    }

    #[test]
    fn interval_throttle_skips_analysis() {
        // interval longer than the activity cooldown: the throttle —
        // not the cooldown — is what drops the middle sample.
        let cfg = AwayConfig {
            interval_ms: 100_000,
            ..AwayConfig::default()
        };
        let e = AwayEngine::new(
            cfg,
            Arc::new(AiEngine::from_parts(AiConfig::default(), None)),
            true,
        );
        let cats = vec![det("cat")];
        assert!(matches!(
            e.observe("0", &cats, 0),
            AwayAction::Activity { .. }
        ));
        assert_eq!(
            e.observe("0", &cats, 70_000),
            AwayAction::None,
            "activity cooldown passed but interval throttle drops it"
        );
        assert!(matches!(
            e.observe("0", &cats, 150_000),
            AwayAction::Activity { .. }
        ));
    }

    #[test]
    fn cameras_are_independent() {
        let e = engine();
        let d = vec![det("person")];
        assert!(matches!(e.observe("0", &d, 0), AwayAction::Visitor { .. }));
        assert!(matches!(
            e.observe("1", &d, 1_000),
            AwayAction::Visitor { .. }
        ));
    }

    #[test]
    fn listen_slot_is_single_shot_and_expires() {
        let e = engine();
        assert!(e.open_listen(7, 10_000));
        assert!(!e.open_listen(8, 20_000), "one microphone — slot busy");
        assert_eq!(e.attribute_listen(9_999), Some(7), "inside deadline");
        assert_eq!(e.attribute_listen(9_999), None, "answered exactly once");
        // Slot free again after attribution.
        assert!(e.open_listen(9, 30_000));
        assert_eq!(e.expire_listen(29_999), None, "still inside deadline");
        assert_eq!(e.expire_listen(30_001), Some(9));
        assert!(e.open_listen(10, 40_000));
        // Attribution after the deadline is closed, not late-answered.
        assert_eq!(e.attribute_listen(45_000), None);
    }

    #[test]
    fn disarm_closes_open_listen() {
        let e = engine();
        e.open_listen(3, 100_000);
        e.disarm();
        assert_eq!(e.attribute_listen(1_000), None);
    }

    #[test]
    fn availability_follows_ai_engine() {
        // The fixture engine has no model → inactive → not armable.
        assert!(!engine().is_available());
    }

    #[test]
    fn summarize_labels_ignores_person_and_unknown() {
        let labels: Vec<String> = ["cat", "dog"].iter().map(|s| s.to_string()).collect();
        let dets = vec![det("person"), det("bird"), det("cat"), det("tv")];
        assert_eq!(summarize_labels(&dets, &labels), "cat×1");
    }
}
