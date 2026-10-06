//! Desktop integration (SPEC appendix A #42): tray icon + desktop
//! notifications when the service runs on a Linux host with a desktop
//! session. Everything here is fail-open — a headless server (no
//! session bus) skips the whole feature with one INFO line and behaves
//! exactly as before.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use notify_rust::Notification;

use crate::config::DesktopConfig;

/// Notification bodies are previews, not transcripts — keep them short.
const NOTIFY_BODY_MAX_CHARS: usize = 120;

/// Session-bus presence probe: a desktop session is assumed when
/// `DBUS_SESSION_BUS_ADDRESS` is set or the user runtime dir carries a
/// session-bus socket. Pure over its inputs so tests stay hermetic.
pub fn session_bus_available_from(dbus_addr: Option<&str>, xdg_bus: Option<&Path>) -> bool {
    dbus_addr.is_some_and(|v| !v.trim().is_empty()) || xdg_bus.is_some_and(|p| p.exists())
}

/// Environment-bound wrapper around [`session_bus_available_from`].
pub fn session_bus_available() -> bool {
    let addr = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
    let xdg_bus = std::env::var_os("XDG_RUNTIME_DIR").map(|dir| PathBuf::from(dir).join("bus"));
    session_bus_available_from(addr.as_deref(), xdg_bus.as_deref())
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

/// Title/body for an alarm notification (visual detections, sound
/// events, zone events — whatever the alarm fanout accepted).
#[must_use]
pub fn alarm_notification(source: &str, targets: usize, class: Option<&str>) -> (String, String) {
    match source {
        "audio" => (
            "MiBee Eye · 听觉告警".to_string(),
            format!("听到：{}", class.unwrap_or("未知声音")),
        ),
        "zone" => (
            "MiBee Eye · 区域告警".to_string(),
            class.map_or_else(
                || "区域事件触发".to_string(),
                |zone| format!("区域事件：{zone}"),
            ),
        ),
        _ => (
            "MiBee Eye · 视觉告警".to_string(),
            format!("检测到 {targets} 个目标"),
        ),
    }
}

/// Title/body for a voice-reply notification.
#[must_use]
pub fn conversation_notification(reply: &str) -> (String, String) {
    (
        "MiBee Eye · 语音回复".to_string(),
        truncate_chars(reply, NOTIFY_BODY_MAX_CHARS),
    )
}

/// The local management URL the tray opens: prefer the plain-HTTP
/// listener when configured (no TLS ceremony on localhost), else the
/// TLS port.
#[must_use]
pub fn web_ui_url(http_port: u16, tls_port: u16) -> String {
    if http_port != 0 {
        format!("http://127.0.0.1:{http_port}")
    } else {
        format!("https://127.0.0.1:{tls_port}")
    }
}

/// Notification fanout. The first failed send (no notification daemon /
/// bus gone) latches the feature off — a headless host must not spam
/// the log on every alarm.
pub struct Desktop {
    notifications_on: bool,
    notify_conversations: bool,
    available: Arc<AtomicBool>,
}

impl Desktop {
    pub fn new(config: &DesktopConfig) -> Arc<Self> {
        let bus = session_bus_available();
        let notifications_on = config.notifications && bus;
        if config.notifications && !bus {
            tracing::info!("desktop: no session bus — notifications disabled (headless host)");
        }
        Arc::new(Self {
            notifications_on,
            notify_conversations: config.notify_conversations,
            available: Arc::new(AtomicBool::new(notifications_on)),
        })
    }

    /// Fire one desktop notification (spawned; never blocks the caller).
    fn notify(&self, summary: String, body: String) {
        if !self.available.load(Ordering::Relaxed) {
            return;
        }
        let available = Arc::clone(&self.available);
        tokio::spawn(async move {
            let sent = Notification::new()
                .summary(&summary)
                .body(&body)
                .timeout(notify_rust::Timeout::Milliseconds(8000))
                .show_async()
                .await;
            if sent.is_err() {
                // No notifications daemon (or the bus died): latch off
                // instead of erroring on every subsequent event.
                available.store(false, Ordering::Relaxed);
                tracing::info!("desktop: notification send failed — disabled for this run");
            }
        });
    }

    /// Alarm rising-edge notification (visual / audio / zone sources).
    pub fn notify_alarm(&self, source: &str, targets: usize, class: Option<&str>) {
        if !self.notifications_on {
            return;
        }
        let (summary, body) = alarm_notification(source, targets, class);
        self.notify(summary, body);
    }

    /// Voice-reply notification (opt-in via `notify_conversations`).
    pub fn notify_conversation(&self, reply: &str) {
        if !self.notifications_on || !self.notify_conversations {
            return;
        }
        let (summary, body) = conversation_notification(reply);
        self.notify(summary, body);
    }
}

// ---------------------------------------------------------------------------
// Tray icon (StatusNotifierItem via ksni)
// ---------------------------------------------------------------------------

struct MiBeeTray {
    url: String,
}

fn open_url(url: &str) {
    if let Err(e) = std::process::Command::new("xdg-open").arg(url).spawn() {
        tracing::warn!(error = %e, url, "desktop: xdg-open failed");
    }
}

impl ksni::Tray for MiBeeTray {
    fn id(&self) -> String {
        "mibee-eye".into()
    }

    fn title(&self) -> String {
        "MiBee Eye".into()
    }

    fn icon_name(&self) -> String {
        // Freedesktop icon-theme name present in every mainstream theme.
        "camera-video".into()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "MiBee Eye".into(),
            description: "running — click to open the web UI".into(),
            ..ksni::ToolTip::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        open_url(&self.url);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::MenuItem;
        use ksni::menu::StandardItem;
        vec![
            MenuItem::Standard(StandardItem {
                label: "打开 Web 界面 / Open Web UI".into(),
                enabled: true,
                visible: true,
                activate: Box::new(|tray: &mut MiBeeTray| open_url(&tray.url)),
                ..StandardItem::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: "MiBee Eye — running".into(),
                enabled: false,
                visible: true,
                ..StandardItem::default()
            }),
        ]
    }
}

/// Spawn the tray icon when configured and a desktop session exists.
/// ksni keeps retrying in the background if the watcher appears later;
/// a hard connect error just logs and gives up (fail-open).
pub fn spawn_tray(config: &DesktopConfig, url: &str) {
    if !config.tray {
        tracing::info!("desktop: tray disabled in config");
        return;
    }
    if !session_bus_available() {
        tracing::info!("desktop: no session bus — tray skipped (headless host)");
        return;
    }
    let tray = MiBeeTray {
        url: url.to_string(),
    };
    tokio::spawn(async move {
        use ksni::TrayMethods;
        match tray.spawn().await {
            Ok(_handle) => tracing::info!("desktop: tray icon started"),
            Err(e) => {
                tracing::info!(error = %e, "desktop: tray unavailable (fail-open)")
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ksni::Tray as _;

    #[test]
    fn session_bus_probe_truth_table() {
        assert!(session_bus_available_from(
            Some("unix:path=/run/user/1000/bus"),
            None
        ));
        let existing = std::env::temp_dir();
        assert!(session_bus_available_from(None, Some(&existing)));
        assert!(!session_bus_available_from(
            None,
            Some(&existing.join("no-such-bus-socket"))
        ));
        assert!(!session_bus_available_from(Some("  "), None));
        assert!(!session_bus_available_from(None, None));
    }

    #[test]
    fn alarm_notification_shapes() {
        let (s, b) = alarm_notification("ai", 3, None);
        assert_eq!(s, "MiBee Eye · 视觉告警");
        assert_eq!(b, "检测到 3 个目标");
        let (_s, b) = alarm_notification("audio", 1, Some("Dog"));
        assert_eq!(b, "听到：Dog");
        let (_s, b) = alarm_notification("zone", 2, Some("door"));
        assert_eq!(b, "区域事件：door");
    }

    #[test]
    fn conversation_notification_truncates_body() {
        let (_, b) = conversation_notification(&"很".repeat(300));
        assert_eq!(b.chars().count(), NOTIFY_BODY_MAX_CHARS);
    }

    #[test]
    fn web_ui_url_prefers_plain_http_listener() {
        assert_eq!(web_ui_url(0, 8443), "https://127.0.0.1:8443");
        assert_eq!(web_ui_url(8080, 8443), "http://127.0.0.1:8080");
    }

    #[test]
    fn tray_menu_carries_open_action_and_status() {
        let tray = MiBeeTray {
            url: "http://127.0.0.1:8080".into(),
        };
        let menu = tray.menu();
        assert_eq!(menu.len(), 3);
        match &menu[0] {
            ksni::MenuItem::Standard(item) => {
                assert!(item.label.contains("Web"));
                assert!(item.enabled);
            }
            ksni::MenuItem::Separator => panic!("first menu item should be the open action"),
            _ => panic!("first menu item should be the open action"),
        }
        match &menu[2] {
            ksni::MenuItem::Standard(item) => assert!(!item.enabled, "status row is informational"),
            _ => panic!("third menu item should be the status row"),
        }
        assert!(matches!(menu[1], ksni::MenuItem::Separator));
        assert_eq!(tray.id(), "mibee-eye");
        assert_eq!(tray.icon_name(), "camera-video");
    }
}
