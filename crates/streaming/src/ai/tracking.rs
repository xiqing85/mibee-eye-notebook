//! ByteTrack-style multi-object tracker (tracking-by-detection, pure
//! post-processing on the existing NanoDet output).
//!
//! Deliberately small (a faithful subset of the ByteTrack paper: constant-
//! velocity Kalman on the box center/size, IoU cost matrix, two-stage
//! matching — high-confidence detections first, then low-confidence ones —
//! and confirmed-track lifecycle rules). Stable track ids are what the
//! geometry event engine needs: without them, "zone intrusion", "line
//! crossing" and "loitering" cannot be told apart from "detection noise".
//!
//! No external crates: ~500 lines of ndarray-free linear algebra. MIT-
//! compatible by construction (independent implementation of the published
//! algorithm).

use std::collections::HashMap;

use super::Detection;

/// Tuning defaults (ByteTrack paper-ish values scaled for 1 Hz inference).
#[derive(Debug, Clone, PartialEq)]
pub struct TrackerParams {
    /// Detection confidence split between the two matching stages.
    pub high_confidence: f32,
    /// Frames a track survives without a match before deletion.
    pub max_age: u32,
    /// Consecutive matches before a track is confirmed (emits events).
    pub min_hits: u32,
    /// IoU below which a candidate pair is never matched.
    pub min_iou: f32,
}

impl Default for TrackerParams {
    fn default() -> Self {
        Self {
            high_confidence: 0.5,
            max_age: 10,
            min_hits: 2,
            min_iou: 0.3,
        }
    }
}

/// One tracked object.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: u64,
    pub label: String,
    /// `[x, y, w, h]` smoothed by the Kalman state.
    pub bbox: [f32; 4],
    pub confidence: f32,
    pub state: TrackState,
    /// Consecutive frames matched so far.
    hit_streak: u32,
    /// Frames since the track was created.
    age: u32,
    /// Frames since last match (time_to_live countdown).
    time_since_update: u32,
    /// Kalman state: [cx, cy, w, h, vcx, vcy].
    kf: KalmanState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackState {
    Tentative,
    Confirmed,
    Deleted,
}

/// Minimal constant-velocity filter on `[cx, cy, w, h]` (w/h follow as
/// smoothed scalars — enough for IoU association at ~1 Hz).
#[derive(Debug, Clone, Copy, PartialEq)]
struct KalmanState {
    /// [cx, cy, w, h]
    pos: [f32; 4],
    /// [vcx, vcy]
    vel: [f32; 2],
}

impl KalmanState {
    fn from_bbox(bbox: [f32; 4]) -> Self {
        Self {
            pos: bbox_center(bbox),
            vel: [0.0; 2],
        }
    }

    fn predict(&mut self) {
        self.pos[0] += self.vel[0];
        self.pos[1] += self.vel[1];
    }

    fn update(&mut self, observed: [f32; 4]) {
        // Exponential blend keeps the filter stable without covariance
        // bookkeeping; velocities derive from consecutive corrections.
        let obs_center = bbox_center(observed);
        let alpha = 0.6;
        let prev = self.pos;
        for (p, o) in self.pos.iter_mut().zip(obs_center.iter()) {
            *p = alpha * o + (1.0 - alpha) * *p;
        }
        for (i, v) in self.vel.iter_mut().enumerate().take(2) {
            // Per-frame velocity estimate, itself smoothed.
            let inst = self.pos[i] - prev[i];
            *v = 0.5 * inst + 0.5 * *v;
        }
    }

    fn predicted_bbox(&self) -> [f32; 4] {
        center_bbox(self.pos)
    }
}

fn bbox_center(b: [f32; 4]) -> [f32; 4] {
    // [x,y,w,h] -> center form [cx, cy, w, h]
    [b[0] + b[2] / 2.0, b[1] + b[3] / 2.0, b[2], b[3]]
}

fn center_bbox(c: [f32; 4]) -> [f32; 4] {
    // center form -> [x,y,w,h]
    [c[0] - c[2] / 2.0, c[1] - c[3] / 2.0, c[2], c[3]]
}

/// IoU of two `[x, y, w, h]` boxes.
#[must_use]
pub fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let (ax1, ay1, ax2, ay2) = (a[0], a[1], a[0] + a[2], a[1] + a[3]);
    let (bx1, by1, bx2, by2) = (b[0], b[1], b[0] + b[2], b[1] + b[3]);
    let ix1 = ax1.max(bx1);
    let iy1 = ay1.max(by1);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    let inter = iw * ih;
    let area_a = a[2] * a[3];
    let area_b = b[2] * b[3];
    let union = area_a + area_b - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

/// Greedy assignment on an IoU cost matrix (Hungarian is overkill at this
/// scale; greedy IoU behaves identically on sparse scenes).
fn greedy_assign(costs: &[Vec<f32>], threshold: f32) -> Vec<(usize, usize)> {
    let mut pairs: Vec<(f32, usize, usize)> = Vec::new();
    for (t, row) in costs.iter().enumerate() {
        for (d, c) in row.iter().enumerate() {
            if *c >= threshold {
                pairs.push((*c, t, d));
            }
        }
    }
    pairs.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut used_t = vec![false; costs.len()];
    let mut used_d = vec![false; costs.first().map_or(0, |r| r.len())];
    let mut out = Vec::new();
    for (_, t, d) in pairs {
        if !used_t[t] && !used_d[d] {
            used_t[t] = true;
            used_d[d] = true;
            out.push((t, d));
        }
    }
    out
}

/// Per-camera tracker.
#[derive(Debug)]
pub struct Tracker {
    params: TrackerParams,
    tracks: Vec<Track>,
    next_id: u64,
}

impl Tracker {
    #[must_use]
    pub fn new(params: TrackerParams) -> Self {
        Self {
            params,
            tracks: Vec::new(),
            next_id: 1,
        }
    }

    /// Feed one frame's detections; returns the live (matched this frame,
    /// non-deleted) tracks after the update.
    pub fn update(&mut self, detections: &[Detection]) -> Vec<Track> {
        for t in &mut self.tracks {
            t.kf.predict();
            t.age += 1;
            t.time_since_update += 1;
        }

        // Two-stage ByteTrack association: strong detections match first,
        // then weak ones recover tracks the strong stage missed.
        let mut matched_dets = vec![false; detections.len()];
        for stage in 0..2 {
            let det_idx: Vec<usize> = (0..detections.len())
                .filter(|&i| {
                    !matched_dets[i]
                        && ((stage == 0)
                            == (detections[i].confidence >= self.params.high_confidence))
                })
                .collect();
            if det_idx.is_empty() {
                continue;
            }
            let live: Vec<usize> = self
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| t.state != TrackState::Deleted)
                .map(|(i, _)| i)
                .collect();
            let costs: Vec<Vec<f32>> = live
                .iter()
                .map(|&ti| {
                    det_idx
                        .iter()
                        .map(|&di| {
                            iou(
                                self.tracks[ti].kf.predicted_bbox(),
                                f32_bbox(detections[di].bbox),
                            )
                        })
                        .collect()
                })
                .collect();
            for (ti, di) in greedy_assign(&costs, self.params.min_iou) {
                let track = &mut self.tracks[live[ti]];
                let det = &detections[det_idx[di]];
                track.kf.update(f32_bbox(det.bbox));
                track.bbox = center_bbox(track.kf.pos);
                track.confidence = det.confidence;
                track.label = det.label.clone();
                track.hit_streak += 1;
                track.time_since_update = 0;
                if track.state == TrackState::Tentative && track.hit_streak >= self.params.min_hits
                {
                    track.state = TrackState::Confirmed;
                }
                matched_dets[det_idx[di]] = true;
            }
        }

        // Lifecycle: tentative tracks that missed are deleted; confirmed
        // tracks coast for `max_age` frames before deletion.
        for t in &mut self.tracks {
            if t.state == TrackState::Tentative && t.time_since_update > 1 {
                t.state = TrackState::Deleted;
            }
            if t.state == TrackState::Confirmed && t.time_since_update > self.params.max_age {
                t.state = TrackState::Deleted;
            }
        }

        // Spawn tentative tracks for detections that matched nothing.
        for (i, det) in detections.iter().enumerate() {
            if matched_dets[i] {
                continue;
            }
            let bbox = f32_bbox(det.bbox);
            let kf = KalmanState::from_bbox(bbox);
            self.tracks.push(Track {
                id: self.next_id,
                label: det.label.clone(),
                bbox,
                confidence: det.confidence,
                state: TrackState::Tentative,
                hit_streak: 1,
                age: 1,
                time_since_update: 0,
                kf,
            });
            self.next_id += 1;
        }

        // Garbage-collect deleted tracks after a grace period.
        self.tracks.retain(|t| {
            t.state != TrackState::Deleted || t.time_since_update <= self.params.max_age + 5
        });

        self.tracks
            .iter()
            .filter(|t| t.state != TrackState::Deleted && t.time_since_update == 0)
            .cloned()
            .collect()
    }

    /// All confirmed tracks regardless of the current frame's match.
    #[must_use]
    pub fn confirmed(&self) -> Vec<Track> {
        self.tracks
            .iter()
            .filter(|t| t.state == TrackState::Confirmed)
            .cloned()
            .collect()
    }
}

/// Test seam: build a confirmed track without running the matcher.
#[cfg(test)]
pub(crate) fn synthetic_track(id: u64, label: &str, bbox: [f32; 4], confidence: f32) -> Track {
    Track {
        id,
        label: label.to_string(),
        bbox,
        confidence,
        state: TrackState::Confirmed,
        hit_streak: 3,
        age: 5,
        time_since_update: 0,
        kf: KalmanState::from_bbox(bbox),
    }
}

fn f32_bbox(b: [u32; 4]) -> [f32; 4] {
    [b[0] as f32, b[1] as f32, b[2] as f32, b[3] as f32]
}

/// Per-camera tracker set keyed by camera id.
#[derive(Debug, Default)]
pub struct Trackers {
    inner: HashMap<String, Tracker>,
}

impl Trackers {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Update one camera's tracker; returns its live tracks.
    pub fn update(&mut self, camera_id: &str, detections: &[Detection]) -> Vec<Track> {
        self.inner
            .entry(camera_id.to_string())
            .or_insert_with(|| Tracker::new(TrackerParams::default()))
            .update(detections)
    }

    /// Drop one camera's tracker (stream stopped).
    pub fn remove(&mut self, camera_id: &str) {
        self.inner.remove(camera_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(label: &str, conf: f32, bbox: [u32; 4]) -> Detection {
        Detection {
            label: label.into(),
            confidence: conf,
            bbox,
        }
    }

    #[test]
    fn iou_basics() {
        assert!((iou([0.0, 0.0, 10.0, 10.0], [0.0, 0.0, 10.0, 10.0]) - 1.0).abs() < 1e-6);
        assert!(iou([0.0, 0.0, 10.0, 10.0], [20.0, 20.0, 5.0, 5.0]) < 1e-6);
        assert!(
            (iou([0.0, 0.0, 10.0, 10.0], [5.0, 0.0, 10.0, 10.0]) - (50.0 / 150.0)).abs() < 1e-5
        );
    }

    #[test]
    fn stable_id_for_moving_object() {
        let mut t = Tracker::new(TrackerParams::default());
        // Object moving right 8 px/frame, confident detections.
        let mut last_id = None;
        for frame in 0..8 {
            let x = frame * 8;
            let tracks = t.update(&[det("person", 0.9, [x, 100, 40, 80])]);
            assert_eq!(tracks.len(), 1);
            let id = tracks[0].id;
            if let Some(prev) = last_id {
                assert_eq!(prev, id, "id must be stable while tracked");
            }
            last_id = Some(id);
            if frame + 1 >= 2 {
                assert_eq!(tracks[0].state, TrackState::Confirmed);
            }
        }
    }

    #[test]
    fn occlusion_survives_within_max_age() {
        let mut t = Tracker::new(TrackerParams {
            max_age: 5,
            ..TrackerParams::default()
        });
        for _ in 0..3 {
            t.update(&[det("person", 0.9, [100, 100, 40, 80])]);
        }
        let id_before = t.confirmed()[0].id;
        // 3 frames of occlusion (no detections).
        for _ in 0..3 {
            t.update(&[]);
        }
        let id_after = t.update(&[det("person", 0.9, [104, 100, 40, 80])]);
        assert_eq!(id_after.len(), 1);
        assert_eq!(id_after[0].id, id_before, "same id re-associates");
    }

    #[test]
    fn occlusion_beyond_max_age_gets_new_id() {
        let mut t = Tracker::new(TrackerParams {
            max_age: 3,
            ..TrackerParams::default()
        });
        for _ in 0..3 {
            t.update(&[det("person", 0.9, [100, 100, 40, 80])]);
        }
        let id_before = t.confirmed()[0].id;
        for _ in 0..10 {
            t.update(&[]);
        }
        let after = t.update(&[det("person", 0.9, [100, 100, 40, 80])]);
        assert_eq!(after.len(), 1);
        assert_ne!(after[0].id, id_before, "expired track must not revive");
    }

    #[test]
    fn two_objects_keep_distinct_ids() {
        let mut t = Tracker::new(TrackerParams::default());
        for frame in 0..5 {
            let x = frame * 4;
            let tracks = t.update(&[
                det("person", 0.9, [x, 100, 40, 80]),
                det("person", 0.9, [500 - x, 100, 40, 80]),
            ]);
            assert_eq!(tracks.len(), 2, "both tracked");
            assert_ne!(tracks[0].id, tracks[1].id);
        }
    }

    #[test]
    fn weak_detections_recover_strong_tracks() {
        // A confirmed track dips below the confidence split for a couple of
        // frames — ByteTrack's second stage must keep it alive.
        let mut t = Tracker::new(TrackerParams::default());
        for _ in 0..4 {
            t.update(&[det("person", 0.9, [100, 100, 40, 80])]);
        }
        let id = t.confirmed()[0].id;
        let tracks = t.update(&[det("person", 0.3, [102, 100, 40, 80])]);
        assert_eq!(tracks.len(), 1, "weak detection still matches the track");
        assert_eq!(tracks[0].id, id);
    }

    #[test]
    fn trackers_map_per_camera() {
        let mut ts = Trackers::new();
        let a = ts.update("cam-a", &[det("person", 0.9, [0, 0, 30, 30])]);
        let b = ts.update("cam-b", &[det("car", 0.9, [0, 0, 30, 30])]);
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        ts.remove("cam-a");
        // cam-a restarts with a fresh tracker (new id space per camera).
        let a2 = ts.update("cam-a", &[det("person", 0.9, [0, 0, 30, 30])]);
        assert_eq!(a2.len(), 1);
    }
}
