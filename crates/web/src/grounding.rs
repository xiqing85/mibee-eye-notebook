//! Live scene grounding for the chat engines (SPEC appendix A #29).
//!
//! The notebook device runs several perception loops independently —
//! NanoDet detection (every ~1 s), VLM alarm descriptions (tens of
//! seconds, only on alarm edges). This module is the cheap junction
//! box: it remembers the latest of each per camera and renders a
//! compact 【画面】 context block that the chat routes prepend to the
//! LLM turns, so "你能看到我吗" is answered from what the cameras
//! actually see instead of a blind language model.
//!
//! Zero new inference is ever triggered from here — recording is fed
//! by the existing loops, and stale entries expire by timestamp.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use streaming::ai::Detection;

/// Detection labels stay useful for a few seconds; the AI loop refreshes
/// them about once per second, so anything older means the loop stopped.
const DETECTION_TTL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
struct CameraGrounding {
    /// Unix-ms stamp of the detection event that produced `labels`.
    detection_ts_ms: u64,
    /// Label counts rendered at record time (e.g. `2×person, 1×chair`).
    labels: String,
    /// Unix-ms stamp + text of the last VLM alarm description.
    vlm_ts_ms: Option<u64>,
    vlm_text: Option<String>,
}

/// Shared, lock-guarded per-camera grounding state.
#[derive(Debug, Default)]
pub struct GroundingState {
    cameras: Mutex<HashMap<String, CameraGrounding>>,
}

fn render_labels(detections: &[Detection]) -> String {
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for d in detections {
        *counts.entry(d.label.as_str()).or_insert(0) += 1;
    }
    let mut parts: Vec<String> = counts
        .into_iter()
        .map(|(label, n)| format!("{n}×{label}"))
        .collect();
    parts.sort();
    parts.join(", ")
}

impl GroundingState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed from the AI detection loop (every event, cheap).
    pub fn record_detections(&self, camera_id: &str, ts_ms: u64, detections: &[Detection]) {
        let labels = render_labels(detections);
        let mut cams = self.cameras.lock().expect("grounding lock");
        let cam = cams
            .entry(camera_id.to_string())
            .or_insert(CameraGrounding {
                detection_ts_ms: 0,
                labels: String::new(),
                vlm_ts_ms: None,
                vlm_text: None,
            });
        // Only move the timestamp forward — events can arrive out of
        // order across threads.
        if ts_ms >= cam.detection_ts_ms {
            cam.detection_ts_ms = ts_ms;
            cam.labels = labels;
        }
    }

    /// Feed from the VLM alarm-description success path.
    pub fn record_vlm_description(&self, camera_id: &str, ts_ms: u64, text: &str) {
        let mut cams = self.cameras.lock().expect("grounding lock");
        let cam = cams
            .entry(camera_id.to_string())
            .or_insert(CameraGrounding {
                detection_ts_ms: 0,
                labels: String::new(),
                vlm_ts_ms: None,
                vlm_text: None,
            });
        if cam.vlm_ts_ms.is_none_or(|old| ts_ms >= old) {
            cam.vlm_ts_ms = Some(ts_ms);
            cam.vlm_text = Some(text.to_string());
        }
    }

    /// Render the 【画面】 context block for one camera. `None` when
    /// nothing fresh is known — the caller then omits the block
    /// entirely (`grounded: "none"`).
    #[must_use]
    pub fn scene_summary(&self, camera_id: &str, now_ms: u64) -> Option<String> {
        let cams = self.cameras.lock().expect("grounding lock");
        let cam = cams.get(camera_id)?;
        let mut parts: Vec<String> = Vec::new();
        if now_ms.saturating_sub(cam.detection_ts_ms) <= DETECTION_TTL.as_millis() as u64
            && !cam.labels.is_empty()
        {
            parts.push(format!("实时检测：{}", cam.labels));
        }
        if let (Some(ts), Some(text)) = (cam.vlm_ts_ms, cam.vlm_text.as_deref()) {
            let age_s = now_ms.saturating_sub(ts) / 1000;
            parts.push(format!("画面描述（{age_s} 秒前，可能滞后）：{text}"));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("；"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(label: &str) -> Detection {
        Detection {
            label: label.to_string(),
            confidence: 0.9,
            bbox: [0, 0, 10, 10],
        }
    }

    #[test]
    fn empty_state_has_no_scene() {
        let g = GroundingState::new();
        assert_eq!(g.scene_summary("0", 1000), None);
    }

    #[test]
    fn labels_are_counted_and_sorted() {
        let g = GroundingState::new();
        g.record_detections("0", 5000, &[det("person"), det("person"), det("chair")]);
        let s = g.scene_summary("0", 8000).expect("scene");
        assert!(s.contains("2×person"), "{s}");
        assert!(s.contains("1×chair"), "{s}");
        assert!(s.contains("实时检测"), "{s}");
        // Sorted: chair before person regardless of arrival order.
        let c = s.find("1×chair").expect("chair");
        let p = s.find("2×person").expect("person");
        assert!(c < p);
    }

    #[test]
    fn detections_expire_but_vlm_survives() {
        let g = GroundingState::new();
        g.record_detections("0", 1000, &[det("person")]);
        g.record_vlm_description("0", 1200, "一个人坐在桌前");
        // Fresh: both parts present.
        let s = g.scene_summary("0", 2000).expect("scene");
        assert!(
            s.contains("实时检测") && s.contains("一个人坐在桌前"),
            "{s}"
        );
        // 60 s later: detections expired, VLM description remains with age.
        let s = g.scene_summary("0", 61_200).expect("scene");
        assert!(!s.contains("实时检测"), "{s}");
        assert!(s.contains("60 秒前") && s.contains("一个人坐在桌前"), "{s}");
    }

    #[test]
    fn out_of_order_events_do_not_rewind() {
        let g = GroundingState::new();
        g.record_detections("0", 5000, &[det("person")]);
        g.record_detections("0", 3000, &[det("dog")]); // stale arrival
        let s = g.scene_summary("0", 6000).expect("scene");
        assert!(s.contains("person") && !s.contains("dog"), "{s}");
    }

    #[test]
    fn cameras_are_independent() {
        let g = GroundingState::new();
        g.record_detections("a", 1000, &[det("person")]);
        assert!(g.scene_summary("a", 1500).is_some());
        assert_eq!(g.scene_summary("b", 1500), None);
    }

    #[test]
    fn vlm_only_still_grounds() {
        let g = GroundingState::new();
        g.record_vlm_description("0", 1000, "空房间");
        assert!(g.scene_summary("0", 2000).is_some());
    }
}
