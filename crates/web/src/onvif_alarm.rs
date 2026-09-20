//! AI detections → ONVIF Pull-Point MotionAlarm events.
//!
//! The same accepted rising edge that fans out to the GB/T 28181 alarm
//! NOTIFY and the SPEC v1 §6 `alarm` SSE event also feeds the ONVIF
//! events service (onvif-device-rs 0.7): NVRs that manage this device
//! over ONVIF can `CreatePullPointSubscription` on
//! `tns1:VideoSource/MotionAlarm` instead of (or besides) the GB alarm
//! channel. Nothing is delivered until a client actually subscribes —
//! `EventsService::publish_event` is a no-op without live pull-points.

use onvif_device_rs::events::{Event, SimpleItem};

/// The MotionAlarm property event for one accepted AI rising edge.
///
/// `Source` carries the camera id (a real UUID on this multi-camera
/// device); `State=true` marks the alarm rise (the bridge is
/// rising-edge-only, mirroring the GB NOTIFY); `Targets` carries the
/// detection count that crossed the edge.
#[must_use]
pub fn motion_alarm_event(camera_id: &str, targets: usize) -> Event {
    Event {
        topic: "tns1:VideoSource/MotionAlarm".to_string(),
        source: vec![SimpleItem::new("Source", camera_id)],
        data: vec![
            SimpleItem::new("State", "true"),
            SimpleItem::new("Targets", &targets.to_string()),
        ],
        ..Event::new("tns1:VideoSource/MotionAlarm")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_alarm_event_shape() {
        let ev = motion_alarm_event("f472b01e-0000-1000-8000-aabbccddeeff", 2);
        assert_eq!(ev.topic, "tns1:VideoSource/MotionAlarm");
        assert_eq!(ev.property_operation, "", "defaults to Changed at publish");
        assert_eq!(
            (ev.source[0].name.as_str(), ev.source[0].value.as_str()),
            ("Source", "f472b01e-0000-1000-8000-aabbccddeeff")
        );
        assert_eq!(
            (ev.data[0].name.as_str(), ev.data[0].value.as_str()),
            ("State", "true")
        );
        assert_eq!(
            (ev.data[1].name.as_str(), ev.data[1].value.as_str()),
            ("Targets", "2")
        );
        assert!(ev.key.is_empty());
    }
}
