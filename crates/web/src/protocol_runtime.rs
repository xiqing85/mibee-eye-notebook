//! Hot-toggle runtime for ONVIF, GB28181, and RTMP protocols.
//!
//! Each protocol can be started/stopped independently without restarting
//! the server. Shutdown is graceful (up to 5s) with force abort on timeout.
//! A failure in one protocol does NOT affect the others.
//!
//! ## Protocol specifics
//!
//! - **ONVIF**: `WsDiscoveryServer` background task, UDP 3702.
//! - **GB28181**: SIP device registration loop, attaches RTP outputs on INVITE.
//! - **RTMP**: Per-stream output (no global task). Toggling controls whether
//!   new streams auto-attach an RTMP push output.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Duration;

use crate::stream_manager::StreamManager;

// ── Constants ────────────────────────────────────────────────────────────────

/// Maximum time to wait for graceful protocol shutdown before force-aborting.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// ONVIF device HTTP service port (SOAP endpoint advertised in XAddrs).
const ONVIF_HTTP_PORT: u16 = 8080;

/// Default RTSP server port used in stream URLs.
const RTSP_PORT: u16 = 8554;

// ── Status types ─────────────────────────────────────────────────────────────

/// Runtime status of all three protocols.
#[derive(Debug, Clone, Serialize)]
pub struct ProtocolStatus {
    pub onvif: ProtocolState,
    pub gb28181: ProtocolState,
    pub rtmp: ProtocolState,
}

/// State of a single protocol.
#[derive(Debug, Clone, Serialize)]
pub struct ProtocolState {
    pub running: bool,
}

// ── Config extraction helpers ────────────────────────────────────────────────

/// Extract an ONVIF device config from the DB JSON value, filling in
/// runtime-dependent fields (rtsp_url, xaddrs) from known parameters.
pub fn build_onvif_config_from_json(
    db_config: &serde_json::Value,
    advertised_host: &str,
) -> OnvifRuntimeConfig {
    let get_str = |key: &str, default: &str| {
        db_config
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    };
    let get_u32 = |key: &str, default: u32| {
        db_config
            .get(key)
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(default)
    };

    let model = get_str("model", "Rec-01");

    OnvifRuntimeConfig {
        device: onvif_device_rs::DeviceConfig {
            manufacturer: get_str("manufacturer", "MiBee"),
            firmware: get_str("firmware_version", "1.0.0"),
            serial_number: get_str("serial", "NC00000001"),
            hardware_id: model.clone(),
            name: model.clone(),
            model,
        },
        onvif_port: ONVIF_HTTP_PORT,
        username: get_str("username", ""),
        password: get_str("password", ""),
        host: advertised_host.to_string(),
        rtsp_port: RTSP_PORT,
        camera_width: get_u32("profile_width", 1280),
        camera_height: get_u32("profile_height", 720),
        camera_fps: get_u32("profile_fps", 25),
        camera_bitrate: get_u32("profile_bitrate", 2_500_000),
    }
}

/// Extract GB28181 config fields from the DB JSON value.
pub fn extract_gb28181_config(db_config: &serde_json::Value) -> Gb28181RuntimeConfig {
    let get_str = |key: &str, default: &str| {
        db_config
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    };
    let get_u16 = |key: &str, default: u16| {
        db_config
            .get(key)
            .and_then(|v| v.as_u64())
            .map(|v| v as u16)
            .unwrap_or(default)
    };
    let get_u64 = |key: &str, default: u64| {
        db_config
            .get(key)
            .and_then(|v| v.as_u64())
            .unwrap_or(default)
    };

    Gb28181RuntimeConfig {
        enabled: db_config
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        device_id: get_str("device_id", "34020000002000000001"),
        sip_addr: get_str("platform_sip_address", "127.0.0.1"),
        sip_port: get_u16("platform_sip_port", 5060),
        password: get_str("password", ""),
        sip_domain: get_str("sip_domain", "3402000000"),
        register_interval: get_u64("register_interval_secs", 60),
        heartbeat_interval_secs: get_u64("heartbeat_interval_secs", 60),
        heartbeat_timeout_count: db_config
            .get("heartbeat_timeout_count")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(3),
        channel_id: get_str("channel_id", "34020000001320000001"),
        local_sip_port: get_u16("local_sip_port", 5060),
        device_name: get_str("device_name", "mibee-rec"),
        manufacturer: get_str("manufacturer", "MiBee"),
        model: get_str("model", "Rec-01"),
        firmware: get_str("firmware", env!("CARGO_PKG_VERSION")),
    }
}

/// ONVIF runtime configuration resolved from the persisted DB JSON. The
/// SOAP/discovery role itself lives in the `onvif-rs` library; media profile
/// dimensions default to 720p and are overridable via DB keys
/// (`profile_width` / `profile_height` / `profile_fps` / `profile_bitrate`).
#[derive(Debug, Clone)]
pub struct OnvifRuntimeConfig {
    pub device: onvif_device_rs::DeviceConfig,
    pub onvif_port: u16,
    pub username: String,
    pub password: String,
    /// Advertised host for XAddrs / stream URIs.
    pub host: String,
    pub rtsp_port: u16,
    pub camera_width: u32,
    pub camera_height: u32,
    pub camera_fps: u32,
    pub camera_bitrate: u32,
}

#[derive(Debug)]
/// Parsed GB28181 configuration extracted from DB JSON.
pub struct Gb28181RuntimeConfig {
    pub enabled: bool,
    pub device_id: String,
    pub sip_addr: String,
    pub sip_port: u16,
    pub password: String,
    pub sip_domain: String,
    pub register_interval: u64,
    pub heartbeat_interval_secs: u64,
    pub heartbeat_timeout_count: u32,
    pub channel_id: String,
    pub local_sip_port: u16,
    /// Catalog/DeviceInfo identity. gb28181-rs 0.6.0 defaults to neutral
    /// placeholders unless the host stamps its product identity here.
    pub device_name: String,
    pub manufacturer: String,
    pub model: String,
    pub firmware: String,
}

// ── ProtocolRuntime ──────────────────────────────────────────────────────────

/// Manages hot-toggle lifecycle for ONVIF, GB28181, and RTMP protocols.
///
/// Each protocol runs as an independent background task. Stopping sends
/// a shutdown signal and waits up to 5s for graceful exit; on timeout the
/// task is force-aborted. A failure in one protocol does NOT affect others.
pub struct ProtocolRuntime {
    onvif_handle: Option<JoinHandle<()>>,
    /// WS-Discovery task (stopped together with the SOAP server).
    discovery_handle: Option<JoinHandle<()>>,
    gb28181_handle: Option<JoinHandle<()>>,
    /// Reserved for future use (RTMP currently per-stream, no global task).
    #[allow(dead_code)]
    rtmp_handle: Option<JoinHandle<()>>,
    /// Shutdown signal senders keyed by protocol name.
    shutdown_txs: HashMap<String, watch::Sender<bool>>,
    /// Whether RTMP push is enabled (per-stream; no global background task).
    rtmp_enabled: bool,
}

impl Default for ProtocolRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolRuntime {
    /// Create a new empty runtime with all protocols stopped.
    pub fn new() -> Self {
        Self {
            onvif_handle: None,
            discovery_handle: None,
            gb28181_handle: None,
            rtmp_handle: None,
            shutdown_txs: HashMap::new(),
            rtmp_enabled: false,
        }
    }

    // ── ONVIF ────────────────────────────────────────────────────────────

    /// Start the ONVIF device role (onvif-rs): SOAP server + WS-Discovery.
    /// Stops the existing instance first.
    ///
    /// The Media profile advertises the FIRST active stream's RTSP path
    /// (`live/{camera_id}`), resolved at start time; toggle the protocol
    /// again after stream topology changes to refresh it. With no active
    /// stream the Device service + discovery still run (identity-only).
    #[tracing::instrument(skip(self, config, stream_manager))]
    pub async fn start_onvif(
        &mut self,
        config: OnvifRuntimeConfig,
        stream_manager: Arc<StreamManager>,
    ) -> anyhow::Result<()> {
        self.stop_onvif().await;

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        self.shutdown_txs.insert("onvif".into(), shutdown_tx);

        // Resolve the advertised stream path from the first active stream.
        let stream_path = stream_manager
            .list_active_streams()
            .await
            .first()
            .map(|info| format!("/live/{}", info.camera_id));
        match &stream_path {
            Some(p) => tracing::info!(path = %p, "ONVIF media profile targets first active stream"),
            None => tracing::warn!(
                "ONVIF starting with no active stream — media actions unavailable until re-toggled"
            ),
        }

        let host = config.host.clone();
        let soap_port = config.onvif_port;
        let device_ip = if host.is_empty() {
            onvif_device_rs::discovery::detect_local_ip()
        } else {
            host
        };

        // SOAP server: Device service (identity) + Media service (when a
        // stream is advertised).
        // An empty password is this product's "auth off" setting (the ONVIF
        // toggle itself lives behind the authenticated settings page), so
        // opt into the library's fail-closed no-auth escape hatch.
        let mut soap = onvif_device_rs::OnvifServer::new(&onvif_device_rs::OnvifConfig {
            port: config.onvif_port,
            username: config.username.clone(),
            password: config.password.clone(),
            allow_no_auth: config.password.is_empty(),
            ..Default::default()
        });

        let device_svc = Arc::new(onvif_device_rs::device::DeviceServiceHandlers::new(
            config.device.clone(),
            config.onvif_port,
            device_ip.clone(),
        ));
        for action in [
            "GetSystemDateAndTime",
            "GetDeviceInformation",
            "GetCapabilities",
            "GetServices",
            "GetScopes",
        ] {
            soap.register_handler(
                action,
                Box::new(onvif_device_rs::device::DeviceHandler(Arc::clone(
                    &device_svc,
                ))),
            );
        }
        // Pre-auth actions per ONVIF Core spec (discovery + clock sync
        // happen before clients can compute WS-Security digests).
        for action in ["GetSystemDateAndTime", "GetCapabilities", "GetServices"] {
            soap.register_anonymous_action(action);
        }

        if let Some(stream_path) = stream_path.clone() {
            let mut media = onvif_device_rs::media::OnvifMediaConfig::new(
                config.camera_width,
                config.camera_height,
                config.camera_fps,
                config.camera_bitrate,
                config.rtsp_port,
                device_ip.clone(),
            );
            media.stream_path = stream_path;
            let media_cfg = Arc::new(media);
            soap.register_handler(
                "GetProfiles",
                Box::new(onvif_device_rs::media::GetProfilesHandler::new(Arc::clone(
                    &media_cfg,
                ))),
            );
            soap.register_handler(
                "GetStreamUri",
                Box::new(onvif_device_rs::media::GetStreamUriHandler::new(
                    Arc::clone(&media_cfg),
                )),
            );
            soap.register_handler(
                "GetVideoSources",
                Box::new(onvif_device_rs::media::GetVideoSourcesHandler::new(
                    Arc::clone(&media_cfg),
                )),
            );
        }

        let discovery = onvif_device_rs::discovery::DiscoveryServer::with_identity(
            &device_ip,
            soap_port,
            &config.device.name,
            &config.device.hardware_id,
        );

        let mut shutdown_rx = shutdown_rx;
        let handle = tokio::spawn(async move {
            tracing::info!(
                port = soap_port,
                "ONVIF SOAP + WS-Discovery starting (onvif-rs)"
            );
            tokio::select! {
                // start() resolves as soon as the accept loop is spawned and
                // its handle's Drop stops the server — awaiting the handle
                // here is what keeps the task (and the server) alive.
                res = soap.start() => match res {
                    Ok(server) => {
                        let _ = server.await;
                        tracing::info!("ONVIF SOAP server exited");
                    }
                    Err(e) => tracing::error!(error = %e, "ONVIF SOAP server error"),
                },
                _ = shutdown_rx.changed() => {
                    tracing::info!("ONVIF graceful shutdown signal received");
                }
            }
        });
        let discovery_handle = tokio::spawn(async move {
            match discovery.start().await {
                Ok(server) => {
                    let _ = server.await;
                }
                Err(e) => tracing::error!(error = %e, "ONVIF WS-Discovery server error"),
            }
        });

        // SOAP task is the tracked lifecycle handle; discovery is aborted
        // alongside it by the same stop path.
        self.onvif_handle = Some(handle);
        self.discovery_handle = Some(discovery_handle);
        tracing::info!("ONVIF protocol started");
        Ok(())
    }

    /// Stop ONVIF WS-Discovery server with graceful 5s timeout.
    #[tracing::instrument(skip(self))]
    pub async fn stop_onvif(&mut self) {
        let tx = self.shutdown_txs.remove("onvif");
        let handle = self.onvif_handle.take();
        if let Some(dh) = self.discovery_handle.take() {
            dh.abort();
        }
        if tx.is_none() && handle.is_none() {
            return;
        }
        graceful_shutdown("onvif", tx, handle).await;
        tracing::info!("ONVIF protocol stopped");
    }

    // ── GB28181 ──────────────────────────────────────────────────────────

    /// Start the GB28181 device server (gb28181-rs). Stops existing first.
    ///
    /// The library server owns the full device role — REGISTER lifecycle
    /// (digest auth), keepalive, catalog, INVITE/ACK/BYE with its own SDP
    /// answers, and PS-over-RTP media push sourced from the first active
    /// camera via [`StreamManagerFrameSource`].
    #[tracing::instrument(skip(self, stream_manager))]
    pub async fn start_gb28181(
        &mut self,
        config: &Gb28181RuntimeConfig,
        stream_manager: Arc<StreamManager>,
    ) -> anyhow::Result<()> {
        // Graceful stop any existing GB28181 task before starting a new one.
        self.stop_gb28181().await;

        let lib_config = gb28181_rs::config::Gb28181Config {
            enabled: true,
            platform_sip_address: config.sip_addr.clone(),
            platform_sip_port: config.sip_port,
            device_id: config.device_id.clone(),
            channel_id: config.channel_id.clone(),
            sip_domain: config.sip_domain.clone(),
            password: config.password.clone(),
            local_sip_port: config.local_sip_port,
            register_interval_secs: config.register_interval,
            heartbeat_interval_secs: config.heartbeat_interval_secs,
            heartbeat_timeout_count: config.heartbeat_timeout_count,
            transport: gb28181_rs::config::Transport::Udp,
            user_agent: Some(format!("mibee-rec/{}", env!("CARGO_PKG_VERSION"))),
            device_name: Some(config.device_name.clone()),
            manufacturer: Some(config.manufacturer.clone()),
            model: Some(config.model.clone()),
            firmware: Some(config.firmware.clone()),
        };

        // The watch channel stays for stop-gb28181 symmetry; the library
        // server exits via task abort in graceful_shutdown.
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        self.shutdown_txs.insert("gb28181".into(), shutdown_tx);

        let source = Arc::new(StreamManagerFrameSource::new(stream_manager));
        tracing::info!(
            device_id = %config.device_id,
            platform = %format!("{}:{}", config.sip_addr, config.sip_port),
            channel = %config.channel_id,
            "GB28181 device server starting (gb28181-rs)"
        );

        let handle = tokio::spawn(async move {
            match gb28181_rs::server::Gb28181Server::start(lib_config, source, None).await {
                Ok(server) => {
                    let _ = server.await;
                    tracing::info!("GB28181 device server task exited");
                }
                Err(e) => {
                    tracing::error!(error = %e, "GB28181 device server failed to start");
                }
            }
        });

        self.gb28181_handle = Some(handle);
        tracing::info!("GB28181 protocol started");
        Ok(())
    }

    /// Stop GB28181 SIP registration with graceful 5s timeout.
    #[tracing::instrument(skip(self))]
    pub async fn stop_gb28181(&mut self) {
        let tx = self.shutdown_txs.remove("gb28181");
        let handle = self.gb28181_handle.take();
        if tx.is_none() && handle.is_none() {
            return;
        }
        graceful_shutdown("gb28181", tx, handle).await;
        tracing::info!("GB28181 protocol stopped");
    }

    // ── RTMP ─────────────────────────────────────────────────────────────

    /// Enable RTMP push. RTMP is per-stream; no global background task is
    /// spawned. New streams will auto-attach an RTMP output based on the
    /// DB config that was just updated.
    #[tracing::instrument(skip(self))]
    pub async fn start_rtmp(&mut self) -> anyhow::Result<()> {
        if self.rtmp_enabled {
            tracing::debug!("RTMP already enabled, nothing to do");
            return Ok(());
        }
        self.rtmp_enabled = true;
        tracing::info!("RTMP push enabled (per-stream; new streams will auto-attach)");
        Ok(())
    }

    /// Disable RTMP push. Existing streams keep their RTMP output until stopped.
    #[tracing::instrument(skip(self))]
    pub async fn stop_rtmp(&mut self) {
        if !self.rtmp_enabled {
            return;
        }
        self.rtmp_enabled = false;
        tracing::info!("RTMP push disabled (new streams will not attach RTMP output)");
    }

    // ── Status ───────────────────────────────────────────────────────────

    /// Return the current running/stopped status for all protocols.
    pub fn status(&self) -> ProtocolStatus {
        ProtocolStatus {
            onvif: ProtocolState {
                running: self.onvif_handle.is_some(),
            },
            gb28181: ProtocolState {
                running: self.gb28181_handle.is_some(),
            },
            rtmp: ProtocolState {
                running: self.rtmp_enabled,
            },
        }
    }

    /// Stop all protocols (for server shutdown).
    #[tracing::instrument(skip(self))]
    pub async fn shutdown_all(&mut self) {
        self.stop_onvif().await;
        self.stop_gb28181().await;
        self.stop_rtmp().await;
    }
}

// ── GB28181 frame source (gb28181-rs seam) ───────────────────────────────────

/// [`gb28181_rs::frame::FrameSource`] bridging the StreamManager's first
/// active camera into the device server.
///
/// The GB28181 runtime exposes the notebook's streams as ONE logical channel
/// (the same "first active stream" routing the previous hand-rolled runtime
/// used): each subscription attaches a bridge task that resolves the first
/// active camera, parses its Annex-B frames into access units, and forwards
/// them into the library's bounded channel (full channel = frame dropped,
/// mirroring the hub's slow-consumer semantics). When no stream is active the
/// bridge retries until one appears, so an INVITE racing stream startup still
/// gets media.
struct StreamManagerFrameSource {
    manager: Arc<StreamManager>,
    next_id: std::sync::atomic::AtomicU64,
    subscribers:
        std::sync::Mutex<HashMap<u64, std::sync::mpsc::SyncSender<gb28181_rs::frame::AccessUnit>>>,
}

impl StreamManagerFrameSource {
    fn new(manager: Arc<StreamManager>) -> Self {
        Self {
            manager,
            next_id: std::sync::atomic::AtomicU64::new(1),
            subscribers: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl gb28181_rs::frame::FrameSource for StreamManagerFrameSource {
    fn subscribe_with_capacity(&self, capacity: usize) -> gb28181_rs::frame::FrameSubscription {
        let (tx, rx) = std::sync::mpsc::sync_channel(capacity);
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Registry entry lets unsubscribe() drop the sender, which ends the
        // bridge task's forward loop on its next send.
        self.subscribers.lock().unwrap().insert(id, tx.clone());

        let manager = self.manager.clone();
        let tx = tx;
        tokio::spawn(async move {
            let mut no_camera_ticks: u32 = 0;
            let mut forwarded: u64 = 0;
            loop {
                let camera = manager
                    .list_active_streams()
                    .await
                    .first()
                    .map(|info| info.camera_id.clone());
                let frames = match camera {
                    Some(camera_id) => {
                        tracing::debug!(%camera_id, "gb28181 bridge: subscribed to camera frames");
                        manager.subscribe_frames(&camera_id).await
                    }
                    None => {
                        no_camera_ticks += 1;
                        if no_camera_ticks % 10 == 1 {
                            tracing::warn!(
                                ticks = no_camera_ticks,
                                "gb28181 bridge: no active camera to bridge"
                            );
                        }
                        None
                    }
                };
                let mut frames = match frames {
                    Some(f) => f,
                    None => {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        continue;
                    }
                };
                let mut gatherer = AuGatherer::new();
                loop {
                    match frames.recv().await {
                        Ok(frame) => {
                            let streaming::source::MediaFrame::Video { .. } = frame.as_ref() else {
                                continue; // audio is not part of the H.264 push path
                            };
                            let Some(au) = gatherer.push(&frame) else {
                                continue;
                            };
                            forwarded += 1;
                            if forwarded % 300 == 1 {
                                tracing::info!(
                                    forwarded,
                                    nalus = au.nalus.len(),
                                    key = au.is_key_frame,
                                    "gb28181 bridge: forwarding access units"
                                );
                            }
                            // Bounded, drop-on-full: mirrors the hub's
                            // slow-consumer semantics without blocking the
                            // async bridge on a full channel.
                            use std::sync::mpsc::TrySendError;
                            match tx.try_send(au) {
                                Ok(()) => {}
                                Err(TrySendError::Full(_)) => {}
                                Err(TrySendError::Disconnected(_)) => {
                                    return; // subscriber dropped (BYE / re-INVITE)
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            continue; // slow consumer: skip missed, keep latest
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            break; // stream ended — fall back to camera re-resolution
                        }
                    }
                }
            }
        });

        gb28181_rs::frame::FrameSubscription { id, receiver: rx }
    }

    fn unsubscribe(&self, id: u64) {
        self.subscribers.lock().unwrap().remove(&id);
    }
}

/// Group the capture pipeline's per-NAL [`MediaFrame`]s into H.264 access
/// units for the GB28181 push path.
///
/// The USB capture pipeline emits **one MediaFrame per NAL unit** (start code
/// stripped, `keyframe` true for IDR slices *and* their SPS/PPS). The
/// library's `FrameSource` contract wants whole access units, so the
/// gatherer holds non-VCL NALs (SPS/PPS/SEI/AUD) pending and emits one
/// access unit per VCL NAL (slice, types 1–5) with those prefixes attached.
struct AuGatherer {
    pending: Vec<Vec<u8>>,
}

impl AuGatherer {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// Feed one video MediaFrame; returns a complete access unit when the
    /// fed NAL closes one (i.e. it was a VCL slice).
    fn push(
        &mut self,
        frame: &streaming::source::MediaFrame,
    ) -> Option<gb28181_rs::frame::AccessUnit> {
        let (data, keyframe_hint) = match frame {
            streaming::source::MediaFrame::Video { data, keyframe, .. } => (data, *keyframe),
            streaming::source::MediaFrame::Audio { .. } => return None,
        };
        if data.is_empty() {
            return None;
        }
        let nalu_type = data[0] & 0x1F;
        if !(1..=5).contains(&nalu_type) {
            // Non-VCL prefix (SPS/PPS/SEI/AUD…): keep the latest set, capped
            // so a runaway encoder cannot grow this without bound.
            self.pending.push(data.clone());
            if self.pending.len() > 8 {
                self.pending.remove(0);
            }
            return None;
        }
        let mut nalus: Vec<gb28181_rs::frame::Nalu> = self
            .pending
            .drain(..)
            .chain(std::iter::once(data.clone()))
            .map(|n| {
                let t = n[0] & 0x1F;
                gb28181_rs::frame::Nalu {
                    nalu_type: t,
                    is_idr: t == 5,
                    is_sps: t == 7,
                    is_pps: t == 8,
                    is_aud: t == 9,
                    data: n,
                }
            })
            .collect();
        let is_key_frame = keyframe_hint || nalus.iter().any(|n| n.is_idr);
        // Defensive: drop degenerate empties (cannot happen for a non-empty
        // input, but keeps the contract explicit).
        nalus.retain(|n| !n.data.is_empty());
        if nalus.is_empty() {
            return None;
        }
        Some(gb28181_rs::frame::AccessUnit {
            is_key_frame,
            timestamp: std::time::Instant::now(),
            nalus,
        })
    }
}

async fn graceful_shutdown(
    protocol: &str,
    shutdown_tx: Option<watch::Sender<bool>>,
    handle: Option<JoinHandle<()>>,
) {
    // 1. Send the shutdown signal so the task can exit cleanly.
    if let Some(tx) = shutdown_tx {
        let _ = tx.send(true);
        tracing::info!(protocol, "shutdown signal sent");
    }

    // 2. Wait for the task to finish, or force-abort after the timeout.
    if let Some(handle) = handle {
        tokio::pin!(handle);
        tokio::select! {
            result = &mut handle => {
                match result {
                    Ok(()) => tracing::info!(protocol, "protocol stopped gracefully"),
                    Err(e) => tracing::warn!(
                        protocol, error = %e,
                        "protocol task error during shutdown"
                    ),
                }
            }
            _ = tokio::time::sleep(SHUTDOWN_TIMEOUT) => {
                tracing::warn!(
                    protocol,
                    "graceful shutdown timed out after 5s, force aborting"
                );
                handle.abort();
                match (&mut handle).await {
                    Ok(()) => {}
                    Err(e) if e.is_cancelled() => {}
                    Err(e) => tracing::warn!(
                        protocol, error = %e,
                        "protocol task error after force abort"
                    ),
                }
            }
        }
    }
}

// ── Network helpers (duplicated from main.rs for self-containment) ───────────

/// Get the local IP address that can reach the given server address.
///
/// Creates a UDP socket, connects to the server, and reads the local address.
/// This works on all platforms (uses std::net, not libc).
#[cfg_attr(not(test), allow(dead_code))]
fn get_local_ip_for_server(server_addr: &SocketAddr) -> anyhow::Result<String> {
    use std::net::UdpSocket;
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(server_addr)?;
    let local_addr = socket.local_addr()?;
    Ok(local_addr.ip().to_string())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_runtime_all_stopped() {
        let rt = ProtocolRuntime::new();
        let status = rt.status();
        assert!(!status.onvif.running);
        assert!(!status.gb28181.running);
        assert!(!status.rtmp.running);
    }

    fn video_nal(nalu_type: u8, keyframe: bool) -> streaming::source::MediaFrame {
        streaming::source::MediaFrame::Video {
            data: vec![nalu_type | 0x60, 0xAA, 0xBB],
            keyframe,
            timestamp: 0,
        }
    }

    /// The capture pipeline emits one MediaFrame per NAL (SPS/PPS/IDR all
    /// keyframe=true); the gatherer must emit one AU per slice with the
    /// non-VCL prefix attached.
    #[test]
    fn au_gatherer_groups_per_nal_frames_into_access_units() {
        let mut g = AuGatherer::new();
        assert!(g.push(&video_nal(7, true)).is_none()); // SPS
        assert!(g.push(&video_nal(8, true)).is_none()); // PPS
        let idr = g.push(&video_nal(5, true)).expect("IDR closes the AU");
        assert!(idr.is_key_frame);
        assert_eq!(
            idr.nalus.iter().map(|n| n.nalu_type).collect::<Vec<_>>(),
            vec![7, 8, 5]
        );
        let p = g.push(&video_nal(1, false)).expect("slice closes the AU");
        assert!(!p.is_key_frame);
        assert_eq!(p.nalus.len(), 1);
        // Audio frames are ignored.
        assert!(
            g.push(&streaming::source::MediaFrame::Audio {
                data: vec![0x00],
                timestamp: 0,
            })
            .is_none()
        );
    }

    #[test]
    fn au_gatherer_caps_pending_prefixes() {
        let mut g = AuGatherer::new();
        for _ in 0..20 {
            g.push(&video_nal(6, false)); // SEI-like non-VCL
        }
        assert!(g.pending.len() <= 8);
    }

    #[test]
    fn test_rtmp_enable_disable() {
        let mut rt = ProtocolRuntime::new();
        assert!(!rt.status().rtmp.running);

        // Use block_on for the async methods
        let rt_handle = tokio::runtime::Runtime::new().unwrap();
        rt_handle.block_on(async {
            rt.start_rtmp().await.unwrap();
            assert!(rt.status().rtmp.running);

            rt.stop_rtmp().await;
            assert!(!rt.status().rtmp.running);
        });
    }

    #[test]
    fn test_extract_gb28181_config_defaults() {
        let json = serde_json::json!({});
        let config = extract_gb28181_config(&json);
        assert_eq!(config.sip_port, 5060);
        assert_eq!(config.register_interval, 60);
        assert_eq!(config.heartbeat_interval_secs, 60);
        assert_eq!(config.heartbeat_timeout_count, 3);
        assert_eq!(config.channel_id, "34020000001320000001");
        assert_eq!(config.local_sip_port, 5060);
        assert_eq!(config.register_interval, 60);
    }

    #[test]
    fn test_extract_gb28181_config_from_json() {
        let json = serde_json::json!({
            "device_id": "34020000001320000001",
            "platform_sip_address": "10.0.0.1",
            "platform_sip_port": 5060,
            "password": "secret",
            "sip_domain": "3402000000",
            "register_interval_secs": 120,
            "heartbeat_interval_secs": 30,
            "heartbeat_timeout_count": 5,
            "channel_id": "34020000001320000001",
            "local_sip_port": 7060
        });
        let config = extract_gb28181_config(&json);
        assert_eq!(config.device_id, "34020000001320000001");
        assert_eq!(config.sip_addr, "10.0.0.1");
        assert_eq!(config.sip_port, 5060);
        assert_eq!(config.password, "secret");
        assert_eq!(config.register_interval, 120);
        assert_eq!(config.heartbeat_interval_secs, 30);
        assert_eq!(config.heartbeat_timeout_count, 5);
        assert_eq!(config.channel_id, "34020000001320000001");
        assert_eq!(config.local_sip_port, 7060);
    }

    #[test]
    fn test_build_onvif_config_from_json() {
        let json = serde_json::json!({
            "manufacturer": "TestCorp",
            "model": "CAM-100",
            "serial": "SN12345",
            "firmware_version": "2.0.0"
        });
        let config = build_onvif_config_from_json(&json, "192.168.1.50");
        assert_eq!(config.device.manufacturer, "TestCorp");
        assert_eq!(config.device.model, "CAM-100");
        assert_eq!(config.device.serial_number, "SN12345");
        assert_eq!(config.device.firmware, "2.0.0");
        assert_eq!(config.device.hardware_id, "CAM-100");
        assert_eq!(config.host, "192.168.1.50");
        assert_eq!(config.rtsp_port, 8554);
        // Media profile defaults (720p) until DB keys override them.
        assert_eq!(config.camera_width, 1280);
        assert_eq!(config.camera_height, 720);
    }

    #[test]
    fn test_build_onvif_config_from_json_profile_overrides() {
        let json = serde_json::json!({
            "profile_width": 1920,
            "profile_height": 1080,
            "profile_fps": 30,
            "profile_bitrate": 4000000
        });
        let config = build_onvif_config_from_json(&json, "10.0.0.1");
        assert_eq!(config.camera_width, 1920);
        assert_eq!(config.camera_height, 1080);
        assert_eq!(config.camera_fps, 30);
        assert_eq!(config.camera_bitrate, 4_000_000);
    }

    #[test]
    fn test_get_local_ip_for_server_loopback() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let result = get_local_ip_for_server(&addr).unwrap();
        assert!(result.starts_with("127."));
    }

    #[tokio::test]
    async fn test_stop_onvif_when_not_running_is_noop() {
        let mut rt = ProtocolRuntime::new();
        rt.stop_onvif().await; // should not panic
        assert!(!rt.status().onvif.running);
    }

    #[tokio::test]
    async fn test_stop_gb28181_when_not_running_is_noop() {
        let mut rt = ProtocolRuntime::new();
        rt.stop_gb28181().await; // should not panic
        assert!(!rt.status().gb28181.running);
    }

    #[tokio::test]
    async fn test_stop_rtmp_when_not_running_is_noop() {
        let mut rt = ProtocolRuntime::new();
        rt.stop_rtmp().await; // should not panic
        assert!(!rt.status().rtmp.running);
    }

    #[tokio::test]
    async fn test_shutdown_all() {
        let mut rt = ProtocolRuntime::new();
        rt.rtmp_enabled = true;
        rt.shutdown_all().await;
        let status = rt.status();
        assert!(!status.onvif.running);
        assert!(!status.gb28181.running);
        assert!(!status.rtmp.running);
    }

    #[test]
    fn au_gatherer_classifies_nalu_flags() {
        let mut g = AuGatherer::new();
        // Per-NAL frames of one keyframe encode: SPS(0x67) PPS(0x68) IDR(0x65)
        g.push(&video_nal(0x67 & 0x1F, true));
        g.push(&video_nal(0x68 & 0x1F, true));
        let au = g.push(&video_nal(5, true)).expect("IDR closes the AU");
        assert!(au.is_key_frame);
        assert_eq!(au.nalus.len(), 3);
        assert!(au.nalus[0].is_sps && au.nalus[0].nalu_type == 7);
        assert!(au.nalus[1].is_pps && au.nalus[1].nalu_type == 8);
        assert!(au.nalus[2].is_idr && au.nalus[2].nalu_type == 5);
    }

    #[test]
    fn au_gatherer_marks_p_frame_not_key() {
        let mut g = AuGatherer::new();
        let au = g.push(&video_nal(1, false)).expect("slice closes the AU");
        assert!(!au.is_key_frame);
        assert_eq!(au.nalus.len(), 1);
        assert_eq!(au.nalus[0].nalu_type, 1);
    }

    #[test]
    fn au_gatherer_skips_audio_and_empty() {
        let mut g = AuGatherer::new();
        let audio = streaming::source::MediaFrame::Audio {
            data: vec![0xFF; 160],
            timestamp: 1,
        };
        assert!(g.push(&audio).is_none());

        let empty = streaming::source::MediaFrame::Video {
            keyframe: false,
            data: Vec::new(),
            timestamp: 2,
        };
        assert!(g.push(&empty).is_none());
    }

    #[tokio::test]
    async fn frame_source_subscribe_unsubscribe_tracks_sender() {
        use gb28181_rs::frame::FrameSource;
        use std::sync::Arc;

        // A StreamManager with no active streams exercises the retry path;
        // subscribe/unsubscribe bookkeeping is observable without a camera.
        let manager = Arc::new(StreamManager::new());
        let source = StreamManagerFrameSource::new(manager);

        let sub = source.subscribe_with_capacity(8);
        assert_eq!(sub.id, 1);
        // registry holds the sender for the live subscription
        assert_eq!(source.subscribers.lock().unwrap().len(), 1);
        source.unsubscribe(sub.id);
        assert_eq!(source.subscribers.lock().unwrap().len(), 0);
    }
}
