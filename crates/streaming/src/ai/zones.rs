//! Geometry event engine: turns tracked objects into zone events
//! (intrusion / loitering / line crossing) with pure-Rust geometry.
//!
//! Zones come from the web layer (db-persisted, user-drawn polygons/lines);
//! this module is deliberately free of storage concerns — feed it tracks,
//! get events. All predicates are exact (ray casting for point-in-polygon,
//! proper segment intersection) and unit-tested on concave shapes.

use std::collections::HashMap;

use super::tracking::Track;

/// A user-defined zone.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Zone {
    pub name: String,
    pub kind: ZoneKind,
    /// Polygon vertices (`Intrusion`) or the two endpoints of the tripwire
    /// (`LineCross`), in video pixel coordinates.
    pub points: Vec<[f32; 2]>,
    /// Intrusion zones: seconds inside before a `Loiter` event fires
    /// (0 = fire `Intrusion` on entry only).
    #[serde(default)]
    pub dwell_secs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoneKind {
    /// Polygon: fire `Intrusion` on entry, `Loiter` after `dwell_secs`.
    Intrusion,
    /// Tripwire: fire `LineCross` on crossing, with the direction.
    LineCross,
}

/// One fired zone event.
#[derive(Debug, Clone, PartialEq)]
pub struct ZoneEvent {
    pub camera_id: String,
    pub zone: String,
    pub event: ZoneEventKind,
    pub track_id: u64,
    pub label: String,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneEventKind {
    Intrusion,
    Loiter,
    LineCross { forward: bool },
}

/// Ray-casting point-in-polygon (handles concave polygons).
#[must_use]
pub fn point_in_polygon(pt: [f32; 2], poly: &[[f32; 2]]) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (xi, yi) = (poly[i][0], poly[i][1]);
        let (xj, yj) = (poly[j][0], poly[j][1]);
        if (yi > pt[1]) != (yj > pt[1]) {
            let x_at = (xj - xi) * (pt[1] - yi) / (yj - yi) + xi;
            if pt[0] < x_at {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Whether segments ab and cd intersect (proper crossing or touching).
///
/// Classic CLRS orientation test; collinear cases fall back to bounding-box
/// containment so disjoint collinear segments stay disjoint.
#[must_use]
pub fn segments_intersect(a: [f32; 2], b: [f32; 2], c: [f32; 2], d: [f32; 2]) -> bool {
    const EPS: f32 = 1e-6;
    let orient = |p: [f32; 2], q: [f32; 2], r: [f32; 2]| -> f32 {
        (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
    };
    let sign = |v: f32| -> i8 {
        if v > EPS {
            1
        } else if v < -EPS {
            -1
        } else {
            0
        }
    };
    let on_segment = |p: [f32; 2], q: [f32; 2], r: [f32; 2]| -> bool {
        // q collinear with p–r: within the bounding box of p–r.
        let (min_x, max_x) = (p[0].min(r[0]) - EPS, p[0].max(r[0]) + EPS);
        let (min_y, max_y) = (p[1].min(r[1]) - EPS, p[1].max(r[1]) + EPS);
        q[0] >= min_x && q[0] <= max_x && q[1] >= min_y && q[1] <= max_y
    };
    let o1 = sign(orient(a, b, c));
    let o2 = sign(orient(a, b, d));
    let o3 = sign(orient(c, d, a));
    let o4 = sign(orient(c, d, b));
    if o1 != o2 && o3 != o4 && !(o1 == 0 && o2 == 0) {
        return true; // proper crossing
    }
    if o1 == 0 && on_segment(a, c, b) {
        return true;
    }
    if o2 == 0 && on_segment(a, d, b) {
        return true;
    }
    if o3 == 0 && on_segment(c, a, d) {
        return true;
    }
    if o4 == 0 && on_segment(c, b, d) {
        return true;
    }
    false
}

/// Which side of the directed line c→d the point p lies on (sign of the
/// cross product) — drives the crossing direction.
#[must_use]
fn side_of(p: [f32; 2], c: [f32; 2], d: [f32; 2]) -> f32 {
    (d[0] - c[0]) * (p[1] - c[1]) - (d[1] - c[1]) * (p[0] - c[0])
}

/// Per-camera zone-event state machine.
#[derive(Debug, Default)]
pub struct ZoneEngine {
    /// `(zone_idx, track_id) -> entered_at_ms` for intrusion dwell.
    inside_since: HashMap<(usize, u64), u64>,
    /// `track_id -> last centroid` for tripwires.
    last_pos: HashMap<u64, [f32; 2]>,
    /// Loiter events already fired for a current occupancy (reset on exit).
    loiter_fired: HashMap<(usize, u64), bool>,
}

impl ZoneEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one camera's live tracks; returns the events that fired.
    /// `now_ms` is the wall clock driving dwell timers.
    pub fn update(
        &mut self,
        camera_id: &str,
        zones: &[Zone],
        tracks: &[Track],
        now_ms: u64,
    ) -> Vec<ZoneEvent> {
        let mut events = Vec::new();
        let live: Vec<u64> = tracks.iter().map(|t| t.id).collect();

        // Track movement bookkeeping (tripwires need the previous frame).
        let mut next_pos = HashMap::with_capacity(tracks.len());
        for t in tracks {
            let center = [t.bbox[0] + t.bbox[2] / 2.0, t.bbox[1] + t.bbox[3]]; // feet point
            next_pos.insert(t.id, center);
        }

        for (zi, zone) in zones.iter().enumerate() {
            match zone.kind {
                ZoneKind::Intrusion => {
                    for t in tracks {
                        let center = next_pos[&t.id];
                        let key = (zi, t.id);
                        if point_in_polygon(center, &zone.points) {
                            let since = *self.inside_since.entry(key).or_insert(now_ms);
                            let dwell_ms = now_ms.saturating_sub(since);
                            if !self.loiter_fired.get(&key).copied().unwrap_or(false) {
                                if dwell_ms == 0 {
                                    events.push(mk(
                                        camera_id,
                                        zone,
                                        ZoneEventKind::Intrusion,
                                        t,
                                        now_ms,
                                    ));
                                }
                                let dwell_need = u64::from(zone.dwell_secs) * 1000;
                                if zone.dwell_secs > 0 && dwell_ms >= dwell_need {
                                    self.loiter_fired.insert(key, true);
                                    events.push(mk(
                                        camera_id,
                                        zone,
                                        ZoneEventKind::Loiter,
                                        t,
                                        now_ms,
                                    ));
                                }
                            }
                        } else if self.inside_since.remove(&key).is_some() {
                            self.loiter_fired.remove(&key);
                        }
                    }
                }
                ZoneKind::LineCross => {
                    if zone.points.len() < 2 {
                        continue;
                    }
                    let (c, d) = (zone.points[0], zone.points[1]);
                    for t in tracks {
                        let Some(&prev) = self.last_pos.get(&t.id) else {
                            continue;
                        };
                        let cur = next_pos[&t.id];
                        if segments_intersect(prev, cur, c, d) {
                            let forward = side_of(cur, c, d) < 0.0;
                            events.push(mk(
                                camera_id,
                                zone,
                                ZoneEventKind::LineCross { forward },
                                t,
                                now_ms,
                            ));
                        }
                    }
                }
            }
        }

        self.last_pos = next_pos;
        // Forget occupancy of tracks that disappeared (re-armed on return).
        self.inside_since.retain(|_, _| true);
        let live_set: std::collections::HashSet<u64> = live.iter().copied().collect();
        self.inside_since
            .retain(|(_, tid), _| live_set.contains(tid));
        self.loiter_fired
            .retain(|(_, tid), _| live_set.contains(tid));
        self.last_pos.retain(|tid, _| live_set.contains(tid));
        events
    }
}

fn mk(camera_id: &str, zone: &Zone, event: ZoneEventKind, t: &Track, now_ms: u64) -> ZoneEvent {
    ZoneEvent {
        camera_id: camera_id.to_string(),
        zone: zone.name.clone(),
        event,
        track_id: t.id,
        label: t.label.clone(),
        timestamp_ms: now_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::super::tracking::{Tracker, TrackerParams};
    use super::*;

    fn square() -> Vec<[f32; 2]> {
        vec![
            [100.0, 100.0],
            [300.0, 100.0],
            [300.0, 300.0],
            [100.0, 300.0],
        ]
    }

    fn track_at(id: u64, x: f32, y: f32) -> Track {
        super::super::tracking::synthetic_track(id, "person", [x, y, 10.0, 10.0], 0.9)
    }

    #[test]
    fn point_in_polygon_square() {
        let sq = square();
        assert!(point_in_polygon([200.0, 200.0], &sq));
        assert!(!point_in_polygon([50.0, 200.0], &sq));
        assert!(!point_in_polygon([400.0, 400.0], &sq));
    }

    #[test]
    fn point_in_polygon_concave() {
        // L-shape: the notch is outside.
        let l = vec![
            [0.0, 0.0],
            [100.0, 0.0],
            [100.0, 50.0],
            [50.0, 50.0],
            [50.0, 100.0],
            [0.0, 100.0],
        ];
        assert!(point_in_polygon([25.0, 25.0], &l), "in the left arm");
        assert!(point_in_polygon([75.0, 25.0], &l), "in the top arm");
        assert!(!point_in_polygon([75.0, 75.0], &l), "in the notch");
    }

    #[test]
    fn segments_intersect_basics() {
        assert!(segments_intersect(
            [0.0, 0.0],
            [10.0, 10.0],
            [0.0, 10.0],
            [10.0, 0.0]
        ));
        assert!(!segments_intersect(
            [0.0, 0.0],
            [1.0, 1.0],
            [5.0, 5.0],
            [9.0, 9.0]
        ));
    }

    #[test]
    fn intrusion_fires_on_entry_and_loiter_after_dwell() {
        let zones = vec![Zone {
            name: "yard".into(),
            kind: ZoneKind::Intrusion,
            points: square(),
            dwell_secs: 3,
        }];
        let mut e = ZoneEngine::new();
        // Person walks in at t=1000 (feet point at 105,290 inside).
        let t1 = track_at(1, 100.0, 280.0);
        let evs = e.update("0", &zones, std::slice::from_ref(&t1), 1_000);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event, ZoneEventKind::Intrusion);
        assert_eq!(evs[0].zone, "yard");
        // Still inside at t=2500: nothing new.
        assert!(
            e.update("0", &zones, std::slice::from_ref(&t1), 2_500)
                .is_empty()
        );
        // At t=4000 dwell 3s reached: loiter fires once.
        let evs = e.update("0", &zones, std::slice::from_ref(&t1), 4_000);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event, ZoneEventKind::Loiter);
        // Stays inside: no re-fire.
        assert!(
            e.update("0", &zones, std::slice::from_ref(&t1), 10_000)
                .is_empty()
        );
    }

    #[test]
    fn exit_rearms_intrusion() {
        let zones = vec![Zone {
            name: "yard".into(),
            kind: ZoneKind::Intrusion,
            points: square(),
            dwell_secs: 0,
        }];
        let mut e = ZoneEngine::new();
        let inside = track_at(1, 100.0, 280.0);
        let outside = track_at(1, 500.0, 500.0);
        assert_eq!(
            e.update("0", &zones, std::slice::from_ref(&inside), 1_000)
                .len(),
            1
        );
        assert!(
            e.update("0", &zones, std::slice::from_ref(&outside), 2_000)
                .is_empty()
        );
        // Re-entry fires again.
        assert_eq!(e.update("0", &zones, &[inside], 3_000).len(), 1);
    }

    #[test]
    fn line_cross_fires_with_direction() {
        let zones = vec![Zone {
            name: "gate".into(),
            kind: ZoneKind::LineCross,
            points: vec![[200.0, 0.0], [200.0, 400.0]],
            dwell_secs: 0,
        }];
        let mut e = ZoneEngine::new();
        // Seed previous position left of the line.
        e.update("0", &zones, &[track_at(7, 100.0, 200.0)], 1_000);
        // Cross to the right.
        let evs = e.update("0", &zones, &[track_at(7, 250.0, 200.0)], 2_000);
        assert_eq!(evs.len(), 1);
        assert!(matches!(
            evs[0].event,
            ZoneEventKind::LineCross { forward: true }
        ));
        // Walking parallel/away never fires.
        assert!(
            e.update("0", &zones, &[track_at(7, 260.0, 200.0)], 3_000)
                .is_empty()
        );
        // Cross back.
        let evs = e.update("0", &zones, &[track_at(7, 100.0, 200.0)], 4_000);
        assert_eq!(evs.len(), 1);
        assert!(matches!(
            evs[0].event,
            ZoneEventKind::LineCross { forward: false }
        ));
    }

    /// End-to-end: detections → tracker → zone events (IDs flow through).
    #[test]
    fn detections_through_tracker_drive_zone_events() {
        let zones = vec![Zone {
            name: "door".into(),
            kind: ZoneKind::Intrusion,
            points: square(),
            dwell_secs: 0,
        }];
        let mut tracker = Tracker::new(TrackerParams::default());
        let mut engine = ZoneEngine::new();
        let det = super::super::Detection {
            label: "person".into(),
            confidence: 0.9,
            bbox: [100, 280, 10, 10],
        };
        // Two frames confirm the track, then the zone sees it.
        tracker.update(std::slice::from_ref(&det));
        let tracks = tracker.update(std::slice::from_ref(&det));
        assert_eq!(tracks.len(), 1);
        let evs = engine.update("0", &zones, &tracks, 1_000);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event, ZoneEventKind::Intrusion);
        assert_eq!(evs[0].track_id, tracks[0].id);
        assert_eq!(evs[0].label, "person");
    }
}
