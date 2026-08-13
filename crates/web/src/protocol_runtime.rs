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

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use protocols::gb28181::{SipMessage, SipMethod};
use protocols::onvif::{OnvifDeviceConfig, WsDiscoveryServer};
use serde::Serialize;
use streaming::output::Gb28181Output;
use tokio::sync::{mpsc, watch, Notify};
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
) -> OnvifDeviceConfig {
    let get_str = |key: &str, default: &str| {
        db_config
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    };

    let model = get_str("model", "Rec-01");

    OnvifDeviceConfig {
        manufacturer: get_str("manufacturer", "MiBee"),
        firmware_version: get_str("firmware_version", "1.0.0"),
        serial_number: get_str("serial", "NC00000001"),
        hardware_id: model.clone(),
        rtsp_url: format!("rtsp://{}:{}/webcam", advertised_host, RTSP_PORT),
        scopes: vec!["onvif://www.onvif.org/type/NetworkVideoTransmitter".into()],
        xaddrs: get_onvif_xaddrs(ONVIF_HTTP_PORT),
        model,
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
        enabled: db_config.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
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
    }
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
}

// ── ProtocolRuntime ──────────────────────────────────────────────────────────

/// Manages hot-toggle lifecycle for ONVIF, GB28181, and RTMP protocols.
///
/// Each protocol runs as an independent background task. Stopping sends
/// a shutdown signal and waits up to 5s for graceful exit; on timeout the
/// task is force-aborted. A failure in one protocol does NOT affect others.
pub struct ProtocolRuntime {
    onvif_handle: Option<JoinHandle<()>>,
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
            gb28181_handle: None,
            rtmp_handle: None,
            shutdown_txs: HashMap::new(),
            rtmp_enabled: false,
        }
    }

    // ── ONVIF ────────────────────────────────────────────────────────────

    /// Start ONVIF WS-Discovery server. Stops the existing instance first.
    #[tracing::instrument(skip(self, config))]
    pub async fn start_onvif(&mut self, config: OnvifDeviceConfig) -> anyhow::Result<()> {
        // Graceful stop any existing ONVIF task before starting a new one.
        self.stop_onvif().await;

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        self.shutdown_txs.insert("onvif".into(), shutdown_tx);

        let handle = tokio::spawn(async move {
            tracing::info!("ONVIF WS-Discovery starting on UDP 3702");
            match WsDiscoveryServer::bind(config, "0.0.0.0:3702").await {
                Ok(server) => {
                    tracing::info!("ONVIF WS-Discovery server started on UDP 3702");
                    let mut rx = shutdown_rx;
                    tokio::select! {
                        res = server.run() => {
                            if let Err(e) = res {
                                tracing::error!(error = %e, "ONVIF WS-Discovery server error");
                            }
                        }
                        _ = rx.changed() => {
                            tracing::info!("ONVIF WS-Discovery graceful shutdown signal received");
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to start ONVIF WS-Discovery server");
                }
            }
        });

        self.onvif_handle = Some(handle);
        tracing::info!("ONVIF protocol started");
        Ok(())
    }

    /// Stop ONVIF WS-Discovery server with graceful 5s timeout.
    #[tracing::instrument(skip(self))]
    pub async fn stop_onvif(&mut self) {
        let tx = self.shutdown_txs.remove("onvif");
        let handle = self.onvif_handle.take();
        if tx.is_none() && handle.is_none() {
            return;
        }
        graceful_shutdown("onvif", tx, handle).await;
        tracing::info!("ONVIF protocol stopped");
    }

    // ── GB28181 ──────────────────────────────────────────────────────────

    /// Start GB28181 SIP device registration loop. Stops existing first.
    #[tracing::instrument(skip(self, stream_manager))]
    pub async fn start_gb28181(
        &mut self,
        config: &Gb28181RuntimeConfig,
        stream_manager: Arc<StreamManager>,
    ) -> anyhow::Result<()> {
        // Graceful stop any existing GB28181 task before starting a new one.
        self.stop_gb28181().await;

        // Parse SIP server address
        let sip_server_addr: SocketAddr = format!("{}:{}", config.sip_addr, config.sip_port)
            .parse()
            .map_err(|e| {
                anyhow::anyhow!(
                    "invalid GB28181 SIP address {}:{}: {}",
                    config.sip_addr,
                    config.sip_port,
                    e
                )
            })?;

        // Get local IP for SIP messages
        let local_ip =
            get_local_ip_for_server(&sip_server_addr).unwrap_or_else(|_| "127.0.0.1".to_string());

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        self.shutdown_txs.insert("gb28181".into(), shutdown_tx);

        let device_id = config.device_id.clone();
        let heartbeat_interval = config.heartbeat_interval_secs;
        let heartbeat_timeout = config.heartbeat_timeout_count;
        let password = config.password.clone();
        let sip_domain = config.sip_domain.clone();
        let register_interval = config.register_interval;

        let handle = tokio::spawn(async move {
            run_gb28181_loop(
                device_id,
                sip_server_addr,
                local_ip,
                heartbeat_interval,
                heartbeat_timeout,
                password,
                sip_domain,
                register_interval,
                stream_manager,
                shutdown_rx,
            )
            .await;
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

// ── Graceful shutdown helper ─────────────────────────────────────────────────

/// Send the shutdown signal, wait up to 5s for graceful exit, then force-abort.
///
/// This is a standalone async function so it can be called for any protocol
/// without borrowing `&mut self` (callers extract tx + handle first).
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

// ── GB28181 SIP device loop ──────────────────────────────────────────────────

/// The GB28181 SIP device registration + INVITE/BYE handling loop.
///
/// This is moved here from `main.rs` so it can be started/stopped at runtime
/// via `ProtocolRuntime`. The protocol crate itself (`crates/protocols/src/gb28181/`)
/// is NOT modified — we only use its public API here.
#[allow(clippy::too_many_arguments)]
async fn run_gb28181_loop(
    device_id: String,
    sip_server_addr: SocketAddr,
    local_ip: String,
    heartbeat_interval_secs: u64,
    heartbeat_timeout_count: u32,
    password: String,
    sip_domain: String,
    register_interval: u64,
    stream_manager: Arc<StreamManager>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    tracing::info!(
        device_id = %device_id,
        sip_server = %sip_server_addr,
        local_ip = %local_ip,
        "GB28181 Device SIP registration starting"
    );

    // Bind UDP socket for SIP communication (shared with the keepalive task)
    let sip_socket = match tokio::net::UdpSocket::bind("0.0.0.0:0").await {
        Ok(socket) => Arc::new(socket),
        Err(e) => {
            tracing::error!(error = %e, "Failed to bind UDP socket for SIP");
            return;
        }
    };

    // Create SIP device client
    let mut sip_client = protocols::gb28181::SipDeviceClient::new(
        &device_id,
        sip_server_addr,
        &local_ip,
        5060, // local port for Via header
        &sip_domain,
        &password,
        register_interval as u32,
    );

    // Track processed INVITEs by Call-ID for deduplication
    let processed_invites: Arc<tokio::sync::Mutex<HashSet<String>>> =
        Arc::new(tokio::sync::Mutex::new(HashSet::new()));

    // Track active RTP push outputs by Call-ID → (camera_id, output_id) for BYE cleanup.
    let mut gb28181_outputs: HashMap<String, (String, streaming::hub::OutputId)> = HashMap::new();

    // Registration state
    let mut registered = false;
    let mut retry_count = 0u32;
    let mut backoff_secs = 1u64;
    const MAX_RETRIES: u32 = 5;

    // Keepalive heartbeat channel: main loop reports whether the platform's
    // response to a Keepalive MESSAGE was 200 OK (true) or not (false).
    let (keepalive_tx, keepalive_rx) = mpsc::channel::<bool>(16);
    let mut keepalive_rx = Some(keepalive_rx);
    // One-shot re-REGISTER trigger for the keepalive task.
    let re_register_notify = Arc::new(Notify::new());
    let mut keepalive_handle: Option<JoinHandle<()>> = None;

    // SIP message buffer
    let mut recv_buf = [0u8; 8192];

    loop {
        // Check if shutdown was signaled
        if *shutdown_rx.borrow() {
            tracing::info!("GB28181 shutdown signal received, exiting loop");
            break;
        }

        // Initial registration or re-registration
        if !registered {
            let register = sip_client.build_register();
            let serialized = register.serialize();

            if let Err(e) = sip_socket
                .send_to(serialized.as_bytes(), sip_server_addr)
                .await
            {
                tracing::warn!(error = %e, "Failed to send REGISTER");
                retry_count += 1;
                if retry_count >= MAX_RETRIES {
                    backoff_secs = 60;
                } else {
                    backoff_secs = backoff_secs.min(8) * 2;
                }

                // Wait with shutdown check
                tokio::select! {
                    biased;
                    _ = shutdown_rx.changed() => {
                        tracing::info!("GB28181 shutdown during backoff");
                        break;
                    }
                    _ = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {}
                }
                continue;
            }

            tracing::info!("REGISTER sent to {}", sip_server_addr);
        }

        // Wait for SIP message, timeout, or shutdown
        tokio::select! {
            biased;
            _ = shutdown_rx.changed() => {
                tracing::info!("GB28181 shutdown signal received during recv");
                break;
            }
            _ = re_register_notify.notified() => {
                tracing::warn!("Keepalive timeout reached, re-registering");
                registered = false;
                retry_count = 0;
                backoff_secs = 1;
            }
            result = sip_socket.recv_from(&mut recv_buf) => {
                match result {
                    Ok((len, from)) => {
                        if from != sip_server_addr {
                            tracing::debug!(
                                "Ignoring SIP message from {} (expected {})",
                                from, sip_server_addr
                            );
                            continue;
                        }

                        let data = &recv_buf[..len];
                        let msg_str = match std::str::from_utf8(data) {
                            Ok(s) => s,
                            Err(e) => {
                                tracing::warn!(error = %e, "Received non-UTF8 SIP data");
                                continue;
                            }
                        };

                        match protocols::gb28181::SipMessage::parse(msg_str) {
                            Ok(msg) => {
                                if let Some(status_code) = msg.status_code {
                                    // Response to our request

                                    // Report keepalive MESSAGE responses to the
                                    // keepalive task (non-blocking; a full channel
                                    // just means the task will time out instead).
                                    let cseq_method = msg
                                        .get_header("CSeq")
                                        .and_then(|s| s.split_whitespace().nth(1))
                                        .unwrap_or("");
                                    if cseq_method == "MESSAGE" {
                                        let is_ok = matches!(
                                            status_code,
                                            protocols::gb28181::SipStatusCode::Ok
                                        );
                                        let _ = keepalive_tx.try_send(is_ok);
                                    }

                                    match status_code {
                                        protocols::gb28181::SipStatusCode::Ok => {
                                            tracing::debug!(
                                                "SIP response: {} {}",
                                                status_code.code(),
                                                status_code.reason()
                                            );
                                            if !registered {
                                                registered = true;
                                                retry_count = 0;
                                                backoff_secs = register_interval;
                                                tracing::info!("SIP registration successful");

                                                // Spawn the keepalive heartbeat task once,
                                                // after the first successful REGISTER.
                                                if keepalive_handle.is_none() {
                                                    if let Some(rx) = keepalive_rx.take() {
                                                        let keepalive_socket = sip_socket.clone();
                                                        let keepalive_shutdown = shutdown_rx.clone();
                                                        let re_register = re_register_notify.clone();
                                                        let ka_device_id = device_id.clone();
                                                        let ka_local_ip = local_ip.clone();
                                                        let ka_domain = sip_domain.clone();
                                                        keepalive_handle = Some(tokio::spawn(
                                                            run_keepalive_loop(
                                                                keepalive_socket,
                                                                sip_server_addr,
                                                                ka_device_id,
                                                                ka_local_ip,
                                                                ka_domain,
                                                                heartbeat_interval_secs,
                                                                heartbeat_timeout_count,
                                                                keepalive_shutdown,
                                                                rx,
                                                                re_register,
                                                            ),
                                                        ));
                                                        tracing::info!(
                                                            heartbeat_interval_secs,
                                                            "GB28181 keepalive loop started"
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        protocols::gb28181::SipStatusCode::Unauthorized => {
                                            tracing::info!(
                                                "Received 401 Unauthorized, sending authenticated REGISTER"
                                            );
                                            match protocols::gb28181::parse_401_challenge(&msg) {
                                                Ok(auth_params) => {
                                                    sip_client.inc_cseq();
                                                    let auth_register = sip_client
                                                        .build_register_with_auth(&auth_params);
                                                    let serialized = auth_register.serialize();
                                                    if let Err(e) = sip_socket
                                                        .send_to(
                                                            serialized.as_bytes(),
                                                            sip_server_addr,
                                                        )
                                                        .await
                                                    {
                                                        tracing::warn!(
                                                            error = %e,
                                                            "Failed to send authenticated REGISTER"
                                                        );
                                                    } else {
                                                        tracing::info!(
                                                            "Authenticated REGISTER sent"
                                                        );
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::error!(
                                                        error = %e,
                                                        "Failed to parse 401 challenge"
                                                    );
                                                }
                                            }
                                        }
                                        _ => {
                                            tracing::debug!(
                                                "SIP response: {} {}",
                                                status_code.code(),
                                                status_code.reason()
                                            );
                                        }
                                    }
                                } else if let Some(method) = msg.method {
                                    // Incoming request
                                    match method {
                                        protocols::gb28181::SipMethod::Invite => {
                                            let call_id = msg
                                                .get_header("Call-ID")
                                                .unwrap_or("")
                                                .to_string();

                                            let mut invites = processed_invites.lock().await;
                                            if invites.contains(&call_id) {
                                                tracing::debug!(
                                                    call_id = %call_id,
                                                    "Duplicate INVITE, ignoring"
                                                );
                                                drop(invites);
                                                continue;
                                            }
                                            invites.insert(call_id.clone());
                                            drop(invites);

                                            tracing::info!(call_id = %call_id, "Received INVITE");

                                            match protocols::gb28181::parse_invite(&msg) {
                                                Ok(invite_info) => {
                                                    tracing::info!(
                                                        call_id = %call_id,
                                                        media_address = %invite_info.media_address,
                                                        media_port = %invite_info.media_port,
                                                        "INVITE parsed successfully"
                                                    );
                                                    // Bind the local UDP socket for RTP media BEFORE sending 200 OK so
                                                    // the advertised `m=video` port matches where we push from. The
                                                    // SIP dialog must complete before the RTP pusher starts.
                                                    let media_socket =
                                                        match tokio::net::UdpSocket::bind("0.0.0.0:0").await
                                                        {
                                                            Ok(s) => Some(s),
                                                            Err(e) => {
                                                                tracing::error!(
                                                                    error = %e,
                                                                    call_id = %call_id,
                                                                    "Failed to bind media UDP socket"
                                                                );
                                                                None
                                                            }
                                                        };
                                                    let device_rtp_port = media_socket
                                                        .as_ref()
                                                        .and_then(|s| s.local_addr().ok())
                                                        .map(|a| a.port())
                                                        .unwrap_or(0);

                                                    // Build device SDP answer (GB/T 28181-2022). The `y=` field
                                                    // echoes the SSRC from the INVITE's SDP.
                                                    let local_sdp = build_invite_sdp_answer(
                                                        &device_id,
                                                        &local_ip,
                                                        device_rtp_port,
                                                        invite_info.ssrc,
                                                    );

                                                    let local_tag = sip_client.cseq;
                                                    sip_client.inc_cseq();
                                                    let cseq = msg
                                                        .get_header("CSeq")
                                                        .and_then(|s| s.split_whitespace().next())
                                                        .and_then(|s| s.parse::<u32>().ok())
                                                        .unwrap_or(sip_client.cseq);
                                                    let response =
                                                        protocols::gb28181::build_invite_response(
                                                            &msg, &device_id, &local_sdp,
                                                            local_tag, cseq, &local_ip, 5060,
                                                        );
                                                    let serialized = response.serialize();
                                                    if let Err(e) = sip_socket
                                                        .send_to(
                                                            serialized.as_bytes(),
                                                            sip_server_addr,
                                                        )
                                                        .await
                                                    {
                                                        tracing::error!(
                                                            error = %e,
                                                            call_id = %call_id,
                                                            "Failed to send 200 OK to INVITE"
                                                        );
                                                    } else {
                                                        tracing::info!(
                                                            call_id = %call_id,
                                                            "Sent 200 OK to INVITE"
                                                        );
                                                        // Attach GB28181 RTP output to first active stream
                                                        match invite_info
                                                            .media_address
                                                            .parse::<std::net::IpAddr>()
                                                        {
                                                            Ok(ip) => {
                                                                let dest = SocketAddr::new(
                                                                    ip,
                                                                    invite_info.media_port,
                                                                );
                                                                let output = Gb28181Output::new(
                                                                    dest,
                                                                    invite_info.ssrc,
                                                                    invite_info.payload_type,
                                                                    &call_id,
                                                                );
                                                                // Attach the pre-bound media socket so RTP is pushed from
                                                                // the port advertised in the 200 OK SDP answer.
                                                                let output = match media_socket {
                                                                    Some(socket) => output.with_socket(socket),
                                                                    None => output,
                                                                };
                                                                let active = stream_manager
                                                                    .list_active_streams()
                                                                    .await;
                                                                match active.first() {
                                                                    Some(info) => {
                                                                        let camera_id =
                                                                            info.camera_id.clone();
                                                                        match stream_manager
                                                                            .add_output_to_stream(
                                                                                &camera_id,
                                                                                Box::new(output),
                                                                            )
                                                                            .await
                                                                        {
                                                                            Ok(output_id) => {
                                                                                tracing::info!(
                                                                                    call_id = %call_id,
                                                                                    camera_id = %camera_id,
                                                                                    dest = %dest,
                                                                                    ssrc = %invite_info.ssrc,
                                                                                    "GB28181 RTP output attached"
                                                                                );
                                                                                gb28181_outputs
                                                                                    .insert(
                                                                                        call_id.clone(),
                                                                                        (
                                                                                            camera_id,
                                                                                            output_id,
                                                                                        ),
                                                                                    );
                                                                            }
                                                                            Err(e) => {
                                                                                tracing::error!(
                                                                                    error = %e,
                                                                                    call_id = %call_id,
                                                                                    "Failed to attach GB28181 output"
                                                                                );
                                                                            }
                                                                        }
                                                                    }
                                                                    None => {
                                                                        tracing::warn!(
                                                                            call_id = %call_id,
                                                                            "INVITE received but no active stream"
                                                                        );
                                                                    }
                                                                }
                                                            }
                                                            Err(e) => {
                                                                tracing::error!(
                                                                    error = %e,
                                                                    call_id = %call_id,
                                                                    media_address = %invite_info.media_address,
                                                                    "Invalid media IP in INVITE"
                                                                );
                                                            }
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::error!(
                                                        error = %e,
                                                        call_id = %call_id,
                                                        "Failed to parse INVITE"
                                                    );
                                                }
                                            }
                                        }
                                        protocols::gb28181::SipMethod::Bye => {
                                            let call_id = msg
                                                .get_header("Call-ID")
                                                .unwrap_or("")
                                                .to_string();
                                            tracing::info!(
                                                call_id = %call_id,
                                                "Received BYE, ending session"
                                            );
                                            if let Some((camera_id, output_id)) =
                                                gb28181_outputs.remove(&call_id)
                                            {
                                                match stream_manager
                                                    .remove_output_from_stream(
                                                        &camera_id,
                                                        output_id,
                                                    )
                                                    .await
                                                {
                                                    Ok(()) => {
                                                        tracing::info!(
                                                            call_id = %call_id,
                                                            camera_id = %camera_id,
                                                            "GB28181 output detached on BYE"
                                                        )
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!(
                                                            error = %e,
                                                            call_id = %call_id,
                                                            "Failed to detach GB28181 output"
                                                        )
                                                    }
                                                }
                                            } else {
                                                tracing::debug!(
                                                    call_id = %call_id,
                                                    "No active RTP push for this session"
                                                );
                                            }
                                        }
                                        protocols::gb28181::SipMethod::Message => {
                                            tracing::info!("Received MESSAGE request");

                                            match protocols::gb28181::client::dispatch_inbound_message(&msg) {
                                                Ok((ok_response, queued)) => {
                                                    // Send 200 OK acknowledgement
                                                    let ok_serialized = ok_response.serialize();
                                                    if let Err(e) = sip_socket
                                                        .send_to(
                                                            ok_serialized.as_bytes(),
                                                            sip_server_addr,
                                                        )
                                                        .await
                                                    {
                                                        tracing::warn!(
                                                            error = %e,
                                                            "Failed to send 200 OK to MESSAGE"
                                                        );
                                                    } else {
                                                        tracing::debug!("Sent 200 OK to MESSAGE");
                                                    }

                                                    // Send queued response if any (Catalog/DeviceInfo)
                                                    if let Some(queued_msg) = queued {
                                                        let queued_serialized = queued_msg.serialize();
                                                        if let Err(e) = sip_socket
                                                            .send_to(
                                                                queued_serialized.as_bytes(),
                                                                sip_server_addr,
                                                            )
                                                            .await
                                                        {
                                                            tracing::warn!(
                                                                error = %e,
                                                                "Failed to send queued MESSAGE response"
                                                            );
                                                        } else {
                                                            tracing::debug!("Sent queued MESSAGE response");
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::error!(
                                                        error = %e,
                                                        "Failed to dispatch MESSAGE"
                                                    );
                                                }
                                            }
                                        }
                                        _ => {
                                            tracing::debug!(
                                                method = %method,
                                                "Received unhandled SIP request"
                                            );
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Failed to parse SIP message");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "SIP socket receive error");
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                // Timeout - send periodic re-registration if already registered
                if registered {
                    sip_client.inc_cseq();
                    let register = sip_client.build_register();
                    let serialized = register.serialize();
                    if let Err(e) = sip_socket
                        .send_to(serialized.as_bytes(), sip_server_addr)
                        .await
                    {
                        tracing::warn!(error = %e, "Failed to send re-registration");
                        registered = false;
                        retry_count = 1;
                        backoff_secs = 1;
                    } else {
                        tracing::debug!("Re-registration sent");
                    }
                }
            }
        }
    }

    tracing::info!("GB28181 SIP loop exited");
}

// ── GB28181 keepalive heartbeat ──────────────────────────────────────────────

/// Build a SIP MESSAGE request carrying a Keepalive Notify body.
///
/// `build_keepalive_notify` (from the protocols crate) provides the MANSCDP
/// XML body; we wrap it with the SIP headers required for a routable MESSAGE.
fn build_keepalive_message(
    device_id: &str,
    local_ip: &str,
    sip_domain: &str,
    sn: u32,
    cseq: u32,
) -> anyhow::Result<SipMessage> {
    let notify = protocols::gb28181::client::build_keepalive_notify(
        &sn.to_string(),
        device_id,
        "OK",
    )?;
    let mut headers = Vec::new();
    headers.push((
        "Via".to_string(),
        format!("SIP/2.0/UDP {}:{};rport;branch=z9hG4bK{}", local_ip, 5060, cseq),
    ));
    headers.push((
        "From".to_string(),
        format!("<sip:{}@{}>;tag={}", device_id, sip_domain, cseq),
    ));
    headers.push(("To".to_string(), format!("<sip:{}@{}>", sip_domain, sip_domain)));
    headers.push(("Call-ID".to_string(), format!("{}-{}", device_id, sn)));
    headers.push(("CSeq".to_string(), format!("{} MESSAGE", cseq)));
    headers.push(("Max-Forwards".to_string(), "70".to_string()));
    headers.push(("User-Agent".to_string(), "mibee-rec/0.1".to_string()));
    headers.push(("Content-Type".to_string(), "Application/MANSCDP+xml".to_string()));
    headers.push(("Content-Length".to_string(), notify.body.len().to_string()));

    Ok(SipMessage {
        start_line: format!("MESSAGE sip:{} SIP/2.0", sip_domain),
        method: Some(SipMethod::Message),
        status_code: None,
        uri: Some(format!("sip:{}", sip_domain)),
        version: "SIP/2.0".to_string(),
        headers,
        body: notify.body,
    })
}

/// Build the device SDP answer for a SIP INVITE (GB/T 28181-2022).
///
/// The `y=` field echoes the SSRC from the INVITE's SDP; `device_rtp_port`
/// is the local UDP port the device will push RTP from.
fn build_invite_sdp_answer(
    device_id: &str,
    local_ip: &str,
    device_rtp_port: u16,
    ssrc: u32,
) -> String {
    format!(
        "v=0\r\no={} 0 0 IN IP4 {}\r\ns=Play\r\nc=IN IP4 {}\r\nm=video {} RTP/AVP 96\r\na=sendonly\r\na=rtpmap:96 PS/90000\r\ny={}\r\n",
        device_id, local_ip, local_ip, device_rtp_port, ssrc
    )
}

/// Run the keepalive heartbeat loop.
///
/// Sends a Keepalive MESSAGE every `heartbeat_interval_secs`. The main SIP
/// loop reports each platform response via `response_rx` (true = 200 OK).
/// After `heartbeat_timeout_count` consecutive failures (send error, non-200
/// response, or no response within the wait window), triggers a re-REGISTER
/// via `re_register_notify`. Exits on shutdown.
#[allow(clippy::too_many_arguments)]
async fn run_keepalive_loop(
    sip_socket: Arc<tokio::net::UdpSocket>,
    sip_server_addr: SocketAddr,
    device_id: String,
    local_ip: String,
    sip_domain: String,
    heartbeat_interval_secs: u64,
    heartbeat_timeout_count: u32,
    mut shutdown_rx: watch::Receiver<bool>,
    mut response_rx: mpsc::Receiver<bool>,
    re_register_notify: Arc<Notify>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(heartbeat_interval_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut consecutive_failures = 0u32;
    let mut sn = 1u32;
    let mut cseq = 1u32;

    loop {
        tokio::select! {
            biased;
            _ = shutdown_rx.changed() => break,
            _ = interval.tick() => {
                let msg = match build_keepalive_message(
                    &device_id, &local_ip, &sip_domain, sn, cseq,
                ) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to build keepalive MESSAGE");
                        consecutive_failures += 1;
                        continue;
                    }
                };
                sn = sn.wrapping_add(1);
                cseq = cseq.wrapping_add(1);
                let serialized = msg.serialize();
                match sip_socket
                    .send_to(serialized.as_bytes(), sip_server_addr)
                    .await
                {
                    Ok(_) => {
                        tracing::debug!("Keepalive MESSAGE sent");
                        // Wait for the platform's response (reported by the main
                        // loop) with a bounded window so a silent platform is
                        // detected as a failure.
                        tokio::select! {
                            resp = response_rx.recv() => {
                                match resp {
                                    Some(true) => consecutive_failures = 0,
                                    Some(false) => consecutive_failures += 1,
                                    None => break,
                                }
                            }
                            _ = shutdown_rx.changed() => break,
                            _ = tokio::time::sleep(Duration::from_secs(10)) => {
                                consecutive_failures += 1;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to send keepalive MESSAGE");
                        consecutive_failures += 1;
                    }
                }
            }
        }

        if consecutive_failures >= heartbeat_timeout_count {
            tracing::warn!(
                consecutive_failures,
                "Keepalive timeout reached, triggering re-REGISTER"
            );
            re_register_notify.notify_one();
            consecutive_failures = 0;
        }
    }
}

// ── Network helpers (duplicated from main.rs for self-containment) ───────────

/// Get the local IP address that can reach the given server address.
///
/// Creates a UDP socket, connects to the server, and reads the local address.
/// This works on all platforms (uses std::net, not libc).
fn get_local_ip_for_server(server_addr: &SocketAddr) -> anyhow::Result<String> {
    use std::net::UdpSocket;
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(server_addr)?;
    let local_addr = socket.local_addr()?;
    Ok(local_addr.ip().to_string())
}

/// Get ONVIF XAddrs (device service URLs) for all non-loopback IPv4 interfaces.
///
/// Each XAddr is in the format `http://{ip}:{port}/onvif/device_service`.
/// Loopback and unspecified addresses are excluded.
#[cfg(unix)]
fn get_onvif_xaddrs(port: u16) -> Vec<String> {
    let mut xaddrs = Vec::new();
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return xaddrs;
        }
        let mut ptr = ifap;
        while !ptr.is_null() {
            let ifa = &*ptr;
            if let Some(addr) = ifa.ifa_addr.as_ref() {
                if addr.sa_family as libc::c_uint == libc::AF_INET as libc::c_uint {
                    let sin = addr as *const libc::sockaddr as *const libc::sockaddr_in;
                    let ip = Ipv4Addr::from(u32::from_be((*sin).sin_addr.s_addr));
                    if !ip.is_loopback() && !ip.is_unspecified() {
                        xaddrs.push(format!("http://{}:{}/onvif/device_service", ip, port));
                    }
                }
            }
            ptr = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    xaddrs
}

/// Fallback for non-Unix platforms (Windows/macOS may need platform-specific
/// enumeration in the future).
#[cfg(not(unix))]
fn get_onvif_xaddrs(_port: u16) -> Vec<String> {
    tracing::warn!(
        "ONVIF XAddr enumeration not implemented on this platform; returning empty list"
    );
    Vec::new()
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
    }

    #[test]
    fn test_extract_gb28181_config_from_json() {
        let json = serde_json::json!({
            "device_id": "34020000001320000001",
            "platform_sip_address": "10.0.0.1",
            "platform_sip_port": 5060,
            "password": "secret",
            "sip_domain": "3402000000",
            "register_interval_secs": 120
        });
        let config = extract_gb28181_config(&json);
        assert_eq!(config.device_id, "34020000001320000001");
        assert_eq!(config.sip_addr, "10.0.0.1");
        assert_eq!(config.sip_port, 5060);
        assert_eq!(config.password, "secret");
        assert_eq!(config.register_interval, 120);
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
        assert_eq!(config.manufacturer, "TestCorp");
        assert_eq!(config.model, "CAM-100");
        assert_eq!(config.serial_number, "SN12345");
        assert_eq!(config.firmware_version, "2.0.0");
        assert_eq!(config.hardware_id, "CAM-100");
        assert!(config.rtsp_url.contains("192.168.1.50"));
        assert!(config.rtsp_url.contains("8554"));
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
    // ── GB28181 keepalive + INVITE tests ────────────────────────────────

    /// Build a synthetic SIP INVITE with the given SDP body.
    fn build_mock_invite(sdp_body: &str) -> SipMessage {
        SipMessage {
            start_line: "INVITE sip:34020000001320000001@3402000000 SIP/2.0".to_string(),
            method: Some(SipMethod::Invite),
            status_code: None,
            uri: Some("sip:34020000001320000001@3402000000".to_string()),
            version: "SIP/2.0".to_string(),
            headers: vec![
                (
                    "Via".to_string(),
                    "SIP/2.0/UDP 192.168.1.200:5060;rport;branch=z9hG4bK12345".to_string(),
                ),
                (
                    "From".to_string(),
                    "<sip:34020000001320000001@3402000000>;tag=123456".to_string(),
                ),
                (
                    "To".to_string(),
                    "<sip:34020000002000000001@3402000000>".to_string(),
                ),
                ("Call-ID".to_string(), "test-invite-call-id".to_string()),
                ("CSeq".to_string(), "7 INVITE".to_string()),
                (
                    "Contact".to_string(),
                    "<sip:34020000001320000001@192.168.1.200:5060>".to_string(),
                ),
                ("Content-Type".to_string(), "application/sdp".to_string()),
            ],
            body: sdp_body.to_string(),
        }
    }

    #[test]
    fn test_invite_handler_sends_200_ok_with_sdp() {
        let invite = build_mock_invite(
            "v=0\r\no=34020000001320000001 0 0 IN IP4 192.168.1.200\r\ns=Play\r\nc=IN IP4 192.168.1.200\r\nt=0 0\r\nm=video 10000 RTP/AVP 96\r\na=sendonly\r\na=rtpmap:96 PS/90000\r\ny=2271560481\r\n",
        );
        let invite_info = protocols::gb28181::parse_invite(&invite).unwrap();

        // Build the device SDP answer exactly as the INVITE handler does.
        let local_sdp = build_invite_sdp_answer(
            "34020000001320000001",
            "192.168.1.100",
            45000,
            invite_info.ssrc,
        );
        assert!(local_sdp.contains("m=video 45000 RTP/AVP 96"));
        assert!(local_sdp.contains("a=rtpmap:96 PS/90000"));
        assert!(local_sdp.contains("y=2271560481"));

        let response = protocols::gb28181::build_invite_response(
            &invite,
            "34020000001320000001",
            &local_sdp,
            42,
            7,
            "192.168.1.100",
            5060,
        );
        let serialized = response.serialize();
        assert!(serialized.contains("SIP/2.0 200 OK"));
        assert!(serialized.contains("m=video 45000 RTP/AVP 96"));
        assert!(serialized.contains("a=rtpmap:96 PS/90000"));
        assert!(serialized.contains("y=2271560481"));
    }

    #[test]
    fn test_invite_handler_echoes_ssrc() {
        // Leading-zero decimal SSRC from the platform's INVITE.
        let invite = build_mock_invite(
            "v=0\r\no=34020000001320000001 0 0 IN IP4 192.168.1.200\r\ns=Play\r\nc=IN IP4 192.168.1.200\r\nt=0 0\r\nm=video 10000 RTP/AVP 96\r\na=sendonly\r\na=rtpmap:96 PS/90000\r\ny=0100000001\r\n",
        );
        let invite_info = protocols::gb28181::parse_invite(&invite).unwrap();
        assert_eq!(invite_info.ssrc, 100_000_001);

        let local_sdp = build_invite_sdp_answer(
            "34020000001320000001",
            "192.168.1.100",
            45000,
            invite_info.ssrc,
        );
        // The echoed value is the normalized decimal form (no leading zero).
        assert!(local_sdp.contains("y=100000001"));
        assert!(!local_sdp.contains("y=0100000001"));
    }

    #[tokio::test]
    async fn test_keepalive_loop_sends_message() {
        // Platform receiver socket (the keepalive MESSAGE is sent to it).
        let platform = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let platform_addr = platform.local_addr().unwrap();
        // Device socket the keepalive task sends from.
        let device_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

        let (_, response_rx) = mpsc::channel::<bool>(16);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let re_register = Arc::new(Notify::new());

        let handle = tokio::spawn(run_keepalive_loop(
            Arc::new(device_socket),
            platform_addr,
            "34020000001320000001".to_string(),
            "127.0.0.1".to_string(),
            "3402000000".to_string(),
            1, // heartbeat_interval_secs (first tick fires immediately)
            3,  // heartbeat_timeout_count
            shutdown_rx,
            response_rx,
            re_register,
        ));

        // The first interval tick fires immediately, so the first Keepalive
        // MESSAGE is sent right away. Wait for it with a real-time bound.
        let mut buf = [0u8; 2048];
        let (len, _) = tokio::time::timeout(
            Duration::from_secs(5),
            platform.recv_from(&mut buf),
        )
        .await
        .expect("keepalive MESSAGE should arrive within 5s")
        .unwrap();
        let data = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(data.contains("<CmdType>Keepalive</CmdType>"));
        assert!(data.contains("MESSAGE sip:3402000000 SIP/2.0"));

        let _ = shutdown_tx.send(true);
        handle.await.unwrap();
    }
}
