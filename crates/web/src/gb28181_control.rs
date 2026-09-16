//! DeviceControl / DeviceConfig host handlers (gb28181-rs seams).
//!
//! Installing a control handler accepts the whole DeviceControl family:
//! the library acks recognized sub-commands and routes them here.
//! notebook has no pan-tilt hardware — PTZ / HomePosition / DragZoom and
//! the guard / teleboot family stay ack-only via the trait defaults.
//! RecordCmd gates local recording through a shared pause flag; IFrameCmd
//! is honestly unsupported (H.264 comes from the camera's native stream —
//! there is no encoder to force a keyframe on), logged once.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gb28181_rs::server::{DeviceConfigHandler, DeviceControlHandler};

/// DeviceControl family handler (A.2.3.1).
pub struct Gb28181ControlHandler {
    /// Shared with the recording `FileOutput`s: `true` pauses local
    /// recording (platform RecordCmd StopRecord), `false` resumes.
    recording_paused: Arc<AtomicBool>,
    force_idr_unsupported_logged: AtomicBool,
}

impl Gb28181ControlHandler {
    pub fn new(recording_paused: Arc<AtomicBool>) -> Self {
        Self {
            recording_paused,
            force_idr_unsupported_logged: AtomicBool::new(false),
        }
    }

    /// Snapshot of the pause gate (mirrors the shared flag).
    pub fn recording_paused(&self) -> bool {
        self.recording_paused.load(Ordering::SeqCst)
    }
}

impl DeviceControlHandler for Gb28181ControlHandler {
    fn on_force_iframe(&self) {
        if !self
            .force_idr_unsupported_logged
            .swap(true, Ordering::SeqCst)
        {
            tracing::warn!(
                "DeviceControl IFrameCmd unsupported: H.264 comes from the camera's \
                 native stream — no encoder to force a keyframe on"
            );
        }
    }

    fn on_record(&self, start: bool) {
        let prev = self.recording_paused.swap(!start, Ordering::SeqCst);
        if prev != !start {
            tracing::info!(
                record = start,
                "DeviceControl RecordCmd: local recording gate updated"
            );
        }
    }

    // on_guard / on_reset_alarm / on_teleboot / on_ptz / on_home_position /
    // on_drag_zoom: no actuators on a notebook — the trait defaults ack
    // these as documented no-ops.
}

/// DeviceConfig AlarmReport gate (A.2.3.2.10): the platform's dual
/// switches toggle whether AI alarms go out as GB28181 Alarm NOTIFY.
/// SSE `alarm` events are unaffected. The initial value comes from the
/// `alarm_notify_enabled` config key; the platform flips it at runtime
/// via DeviceConfig.
#[derive(Debug)]
pub struct AlarmReportGate(pub Arc<AtomicBool>);

impl AlarmReportGate {
    pub fn new(initial: Arc<AtomicBool>) -> Self {
        Self(initial)
    }

    pub fn enabled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl DeviceConfigHandler for AlarmReportGate {
    fn on_alarm_report(&self, motion_detection: u32, field_detection: u32) {
        let enabled = motion_detection > 0 || field_detection > 0;
        let prev = self.0.swap(enabled, Ordering::SeqCst);
        if prev != enabled {
            tracing::info!(
                enabled,
                "DeviceConfig AlarmReport: alarm notify gate updated"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_cmd_flips_pause_gate() {
        let flag = Arc::new(AtomicBool::new(false));
        let handler = Gb28181ControlHandler::new(Arc::clone(&flag));
        assert!(!handler.recording_paused());

        handler.on_record(false); // StopRecord
        assert!(handler.recording_paused());
        assert!(flag.load(Ordering::SeqCst));

        handler.on_record(true); // Record
        assert!(!handler.recording_paused());
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn alarm_report_gate_follows_dual_switches() {
        let gate = AlarmReportGate::new(Arc::new(AtomicBool::new(true)));
        assert!(gate.enabled());

        gate.on_alarm_report(0, 0);
        assert!(!gate.enabled());

        gate.on_alarm_report(1, 0);
        assert!(gate.enabled());

        gate.on_alarm_report(0, 1);
        assert!(gate.enabled());
    }

    #[test]
    fn force_idr_marks_logged_once() {
        let handler = Gb28181ControlHandler::new(Arc::new(AtomicBool::new(false)));
        // The handler logs once per process; the latch is observable.
        assert!(!handler.force_idr_unsupported_logged.load(Ordering::SeqCst));
        handler.on_force_iframe();
        assert!(handler.force_idr_unsupported_logged.load(Ordering::SeqCst));
    }
}
