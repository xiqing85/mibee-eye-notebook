//! USB camera hot-plug detection via kernel netlink uevents.
//!
//! Monitors the `video4linux` subsystem for device add/remove events.
//! When a camera is plugged in, a [`HotplugEvent::Added`] is emitted.
//! When unplugged, [`HotplugEvent::Removed`] is emitted.
//!
//! # Platform
//!
//! Only available on Linux. On other platforms, [`HotplugMonitor`] is a
//! no-op stub that never produces events.
//!
//! # Implementation
//!
//! Uses a raw `NETLINK_KOBJECT_UEVENT` socket (via `libc`) to receive
//! kernel uevents directly — no libudev dependency required. The socket
//! file descriptor is wrapped in [`tokio::io::unix::AsyncFd`] for
//! non-blocking, sub-second-latency event delivery.
//!
//! Message format: uevents arrive as null-terminated text segments.
//! The first segment is `ACTION@DEVPATH`, followed by `KEY=VALUE` pairs
//! like `SUBSYSTEM=video4linux`, `DEVNAME=video0`, etc.

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A hot-plug event from the netlink uevent monitor.
#[derive(Debug, Clone)]
pub enum HotplugEvent {
    /// A video device was plugged in.
    Added {
        /// `/dev/videoN` device index (the `N` in the path).
        device_index: u32,
        /// Human-readable device name.
        name: String,
    },
    /// A video device was unplugged.
    Removed {
        /// `/dev/videoN` device index.
        device_index: u32,
    },
}

// ---------------------------------------------------------------------------
// Linux implementation (raw NETLINK_KOBJECT_UEVENT)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use tokio::io::unix::AsyncFd;

    /// Netlink protocol number for kobject_uevent (kernel broadcasts here).
    // Defined in include/uapi/linux/netlink.h as NETLINK_KOBJECT_UEVENT = 15.
    // Not always available via libc constants, so we hardcode it.
    const NETLINK_KOBJECT_UEVENT: i32 = 15;

    /// The multicast group to join (bit 0 = group 1 = uevent).
    const UEVENT_GROUP: u32 = 1;

    /// Buffer size for a single uevent message.
    const RECV_BUF_SIZE: usize = 16 * 1024;

    /// Monitor for USB camera hot-plug events.
    pub struct HotplugMonitor {
        fd: AsyncFd<OwnedFd>,
    }

    impl HotplugMonitor {
        /// Create a new hot-plug monitor for the `video4linux` subsystem.
        pub fn new() -> Result<Self> {
            // 1. Create the netlink socket (non-blocking, close-on-exec).
            let raw_fd = unsafe {
                libc::socket(
                    libc::AF_NETLINK,
                    libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                    NETLINK_KOBJECT_UEVENT,
                )
            };
            if raw_fd < 0 {
                return Err(std::io::Error::last_os_error())
                    .context("Failed to create NETLINK_KOBJECT_UEVENT socket");
            }
            // SAFETY: raw_fd is valid and newly allocated.
            let fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(raw_fd) };

            // 2. Bind to the uevent multicast group.
            let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
            addr.nl_family = libc::AF_NETLINK as u16;
            addr.nl_groups = UEVENT_GROUP;
            let ret = unsafe {
                libc::bind(
                    fd.as_raw_fd(),
                    &addr as *const _ as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
                )
            };
            if ret < 0 {
                return Err(std::io::Error::last_os_error())
                    .context("Failed to bind netlink uevent socket");
            }

            let fd = AsyncFd::new(fd).context("Failed to wrap netlink fd in AsyncFd")?;

            info!("netlink uevent hot-plug monitor started for video4linux subsystem");
            Ok(Self { fd })
        }

        /// Run the monitor loop, sending events to `tx`.
        ///
        /// Blocks until the receiver is dropped or an unrecoverable error occurs.
        pub async fn run(self, tx: mpsc::Sender<HotplugEvent>) {
            let fd = std::sync::Arc::new(self.fd);

            loop {
                // Wait for the socket to become readable (uevent available).
                let mut guard = match fd.readable().await {
                    Ok(g) => g,
                    Err(e) => {
                        error!(error = %e, "uevent monitor readable() error, retrying");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    }
                };

                // Drain all available events without blocking.
                let events = match guard.try_io(|inner| drain_events(inner.get_ref())) {
                    Ok(Ok(evts)) => evts,
                    Ok(Err(e)) => {
                        warn!(error = %e, "uevent recv error");
                        vec![]
                    }
                    Err(_would_block) => vec![],
                };

                for event in events {
                    debug!(?event, "dispatching hot-plug event");
                    if tx.send(event).await.is_err() {
                        info!("uevent monitor receiver dropped, stopping");
                        return;
                    }
                }
            }
        }
    }

    /// Read all pending uevents from the socket and parse the ones that
    /// belong to the `video4linux` subsystem.
    fn drain_events(fd: &OwnedFd) -> std::io::Result<Vec<HotplugEvent>> {
        let mut events = Vec::new();
        let mut buf = [0u8; RECV_BUF_SIZE];

        loop {
            // Non-blocking recv: returns WouldBlock when no more data.
            let n = unsafe {
                libc::recv(
                    fd.as_raw_fd(),
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if matches!(err.kind(), std::io::ErrorKind::WouldBlock) {
                    break; // No more events.
                }
                return Err(err);
            }
            if n == 0 {
                // A zero-length datagram is a delivered message on netlink
                // (and socketpair) sockets, not end-of-data. Consume it and
                // keep draining — breaking here leaves tokio's readiness
                // flag set without a WouldBlock to clear it, so a stream of
                // empty datagrams busy-loops the monitor task at 100% CPU.
                continue;
            }

            let data = &buf[..n as usize];
            if let Some(event) = parse_uevent(data) {
                events.push(event);
            }
        }

        Ok(events)
    }

    /// Parse a raw uevent message buffer into a [`HotplugEvent`].
    ///
    /// Returns `None` for non-video4linux events or unparseable messages.
    fn parse_uevent(data: &[u8]) -> Option<HotplugEvent> {
        // Uevents are null-separated ASCII strings.
        let segments: Vec<&str> = data
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .filter_map(|s| std::str::from_utf8(s).ok())
            .collect();

        if segments.is_empty() {
            return None;
        }

        // Parse KEY=VALUE segments.
        let mut action: Option<&str> = None;
        let mut subsystem: Option<&str> = None;
        let mut devname: Option<&str> = None;

        for seg in &segments {
            if let Some((key, val)) = seg.split_once('=') {
                match key {
                    "ACTION" => action = Some(val),
                    "SUBSYSTEM" => subsystem = Some(val),
                    "DEVNAME" => devname = Some(val),
                    _ => {}
                }
            }
        }

        // Only care about video4linux subsystem.
        if subsystem != Some("video4linux") {
            return None;
        }

        let action = action?;
        let devname = devname?;

        // Extract device index from devname: "video0" -> 0.
        let device_index: u32 = devname.strip_prefix("video")?.parse().ok()?;

        let name = segments
            .iter()
            .find_map(|s| {
                s.strip_prefix("ID_MODEL_FROM_DATABASE=")
                    .or_else(|| s.strip_prefix("ID_MODEL="))
            })
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("Video Device {}", device_index));

        match action {
            "add" => Some(HotplugEvent::Added { device_index, name }),
            "remove" => Some(HotplugEvent::Removed { device_index }),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_parse_uevent_add() {
            let msg = [
                "add\0",
                "ACTION=add\0",
                "SUBSYSTEM=video4linux\0",
                "DEVNAME=video0\0",
                "ID_MODEL=USB_Camera\0",
            ]
            .concat();

            let event = parse_uevent(msg.as_bytes());
            assert!(event.is_some());
            match event.unwrap() {
                HotplugEvent::Added { device_index, name } => {
                    assert_eq!(device_index, 0);
                    assert!(name.contains("USB_Camera"));
                }
                _ => panic!("expected Added"),
            }
        }

        #[test]
        fn test_parse_uevent_remove() {
            let msg = [
                "remove\0",
                "ACTION=remove\0",
                "SUBSYSTEM=video4linux\0",
                "DEVNAME=video2\0",
            ]
            .concat();

            let event = parse_uevent(msg.as_bytes());
            match event.unwrap() {
                HotplugEvent::Removed { device_index } => {
                    assert_eq!(device_index, 2);
                }
                _ => panic!("expected Removed"),
            }
        }

        #[test]
        fn test_parse_uevent_ignores_non_video4linux() {
            let msg = [
                "add\0",
                "ACTION=add\0",
                "SUBSYSTEM=block\0",
                "DEVNAME=sda1\0",
            ]
            .concat();

            let event = parse_uevent(msg.as_bytes());
            assert!(event.is_none(), "should ignore non-video4linux events");
        }

        #[test]
        fn test_parse_uevent_ignores_unknown_action() {
            let msg = [
                "change\0",
                "ACTION=change\0",
                "SUBSYSTEM=video4linux\0",
                "DEVNAME=video0\0",
            ]
            .concat();

            let event = parse_uevent(msg.as_bytes());
            assert!(event.is_none(), "should ignore non-add/remove actions");
        }

        #[tokio::test]
        async fn test_hotplug_monitor_creation() {
            match HotplugMonitor::new() {
                Ok(_) => {}
                Err(e) => {
                    println!("skipping test — monitor creation failed: {e}");
                }
            }
        }

        /// Regression: a zero-length datagram must be consumed and discarded,
        /// not treated as end-of-drain.
        ///
        /// On netlink uevent sockets a zero-length datagram is a delivered
        /// message (recv returns 0); the old code `break`ed on it, leaving
        /// tokio's readiness flag set without ever reaching WouldBlock — with
        /// a steady stream of empty datagrams the monitor loop spun at 100%
        /// CPU and starved the whole runtime (web-server wedge ~25s after
        /// start).
        ///
        /// A Unix socketpair has the same recv semantics, so `drain_events`
        /// can be driven deterministically: an empty datagram followed by a
        /// real uevent must drain BOTH (previously it drained neither).
        #[test]
        fn test_drain_events_consumes_zero_length_datagram() {
            use std::os::unix::net::UnixDatagram;

            let (a, b) = UnixDatagram::pair().expect("socketpair");
            // Zero-length datagram, then a real video4linux "add" uevent.
            a.send(&[]).expect("send empty datagram");
            let uevent = [
                "add\0",
                "ACTION=add\0",
                "SUBSYSTEM=video4linux\0",
                "DEVNAME=video7\0",
            ]
            .concat();
            a.send(uevent.as_bytes()).expect("send uevent");

            let owned = {
                use std::os::fd::FromRawFd;
                let raw = b.as_raw_fd();
                std::mem::forget(b); // ownership moves into OwnedFd
                // SAFETY: raw is the live descriptor of b, taken above.
                unsafe { OwnedFd::from_raw_fd(raw) }
            };

            let events = drain_events(&owned).expect("drain should succeed");
            assert_eq!(
                events.len(),
                1,
                "the real uevent behind the empty datagram must be drained"
            );
            match &events[0] {
                HotplugEvent::Added { device_index, .. } => assert_eq!(*device_index, 7),
                other => panic!("expected Added, got {other:?}"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Non-Linux stub
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::*;

    pub struct HotplugMonitor;

    impl HotplugMonitor {
        pub fn new() -> Result<Self> {
            warn!("hot-plug monitor is not available on this platform");
            Ok(Self)
        }

        pub async fn run(self, _tx: mpsc::Sender<HotplugEvent>) {
            std::future::pending::<()>().await;
        }
    }
}

pub use platform::HotplugMonitor;

// ---------------------------------------------------------------------------
// Tests (cross-platform)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hotplug_event_clone() {
        let added = HotplugEvent::Added {
            device_index: 0,
            name: "Test Camera".to_string(),
        };
        let _clone = added.clone();

        let removed = HotplugEvent::Removed { device_index: 1 };
        let _clone2 = removed.clone();
    }
}
