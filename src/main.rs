use clap::Parser;

use protocols::onvif::{OnvifDeviceConfig, WsDiscoveryServer};
use protocols::rtsp_server::{RtspServer, RtspServerConfig};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use streaming::output::Gb28181Output;
use tokio::sync::watch;

#[derive(Parser, Debug)]
#[command(
    name = "mibee-rec",
    about = "MiBee Rec — Professional laptop surveillance agent"
)]
struct Args {
    /// Path to config file
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    /// Path to SQLite database
    #[arg(short = 'd', long, default_value = "mibee_rec.db")]
    db_path: PathBuf,

    /// Reset password for a user (prompts for credentials, does not start the server)
    #[arg(long)]
    reset_password: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {

    // Install rustls crypto provider (required for rustls 0.23+)
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let args = Args::parse();

    // Handle --reset-password before starting the server
    if args.reset_password {
        return reset_password_cli(&args).await;
    }

    let config = mibee_rec::config::AppConfig::load(&args.config)?;
    config.validate()?;

    // Resolve advertised host: use configured value or auto-detect LAN IP
    let advertised_host = match &config.web.advertised_host {
        Some(host) if !host.is_empty() => host.clone(),
        _ => {
            let target: std::net::SocketAddr = "8.8.8.8:53".parse().unwrap();
            get_local_ip_for_server(&target).unwrap_or_else(|_| "127.0.0.1".to_string())
        }
    };
    tracing::info!(advertised_host = %advertised_host, "resolved advertised host");

    // Initialise rate limit config from security settings
    security::rate_limit::init_rate_limit_config(
        config.security.rate_limit_max,
        config.security.rate_limit_window_secs,
    );

    // Initialise tracing (subscriber, optional OTLP export + Loki log shipping)
    let loki_endpoint = config
        .observability
        .logs
        .as_ref()
        .map(|l| l.endpoint.clone());
    let loki_labels = config
        .observability
        .logs
        .as_ref()
        .map(|l| l.labels.clone())
        .unwrap_or_default();
    observability::init_tracing(
        &config.observability.log_level,
        false,
        if config.observability.otel_endpoint.is_empty() {
            None
        } else {
            Some(config.observability.otel_endpoint.clone())
        },
        loki_endpoint,
        loki_labels,
    )?;

    // Initialise database
    let db_path = args.db_path.to_string_lossy().to_string();
    let conn = web::db::init_db(&db_path)?;

    // Seed protocol_configs table from config.toml on first run (when empty).
    // Subsequent runs use the persisted values; users' Web UI edits survive restart.
    if web::db::protocol_configs_is_empty(&conn)? {
        tracing::info!("seeding protocol_configs from config.toml (first run)");
        web::db::set_protocol_config(&conn, "onvif", &serde_json::to_value(&config.onvif)?)?;
        web::db::set_protocol_config(&conn, "gb28181", &serde_json::to_value(&config.gb28181)?)?;
        web::db::set_protocol_config(
            &conn,
            "rtmp_push",
            &serde_json::to_value(&config.rtmp_push)?,
        )?;
        web::db::set_protocol_config(
            &conn,
            "recording",
            &serde_json::to_value(&config.recording)?,
        )?;
    } else {
        tracing::debug!("protocol_configs table already populated; keeping persisted values");
    }

    // Auto-discover physically attached cameras (USB webcams etc.)
    let discovered = web::db::auto_discover_cameras(&conn)?;
    if discovered > 0 {
        tracing::info!(count = discovered, "auto-discovered cameras on startup");
    }

    // Collect protocol JoinHandles for graceful shutdown
    let mut protocol_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // Start RTSP server in background task
    let rtsp_config = RtspServerConfig {
        port: config.rtsp.server_port,
        ..Default::default()
    };
    let rtsp_server = Arc::new(RtspServer::new(rtsp_config));
    let rtsp_server_clone = rtsp_server.clone();
    let rtsp_handle = tokio::spawn(async move {
        if let Err(e) = rtsp_server_clone.run().await {
            tracing::error!(error = %e, "RTSP server error");
        }
    });
    protocol_handles.push(rtsp_handle);
    tracing::info!(port = config.rtsp.server_port, "RTSP server started");

    // Determine ONVIF / GB28181 enable flags. The DB is the source of
    // truth (seeded from config.toml on first run, mutated via Web UI).
    // Fall back to config.toml if the DB lookup fails (e.g., fresh install
    // before seeding has run).
    let onvif_enabled = web::db::get_protocol_config(&conn, "onvif")
        .ok()
        .flatten()
        .and_then(|v| v.get("enabled").and_then(|e| e.as_bool()))
        .unwrap_or(config.onvif.enabled);
    let gb28181_enabled = web::db::get_protocol_config(&conn, "gb28181")
        .ok()
        .flatten()
        .and_then(|v| v.get("enabled").and_then(|e| e.as_bool()))
        .unwrap_or(config.gb28181.enabled);

    // ONVIF Device (if enabled)
    if onvif_enabled {
        let onvif_device_config = OnvifDeviceConfig {
            manufacturer: config.onvif.manufacturer.clone(),
            model: config.onvif.model.clone(),
            firmware_version: config.onvif.firmware_version.clone(),
            serial_number: config.onvif.serial.clone(),
            hardware_id: config.onvif.model.clone(),
            rtsp_url: format!(
                "rtsp://{}:{}/webcam",
                advertised_host, config.rtsp.server_port
            ),
            scopes: vec!["onvif://www.onvif.org/type/NetworkVideoTransmitter".into()],
            xaddrs: get_onvif_xaddrs(ONVIF_HTTP_PORT),
        };
        let onvif_handle = tokio::spawn(async move {
            match WsDiscoveryServer::bind(onvif_device_config, "0.0.0.0:3702").await {
                Ok(server) => {
                    tracing::info!("ONVIF WS-Discovery server started on UDP 3702");
                    if let Err(e) = server.run().await {
                        tracing::error!(error = %e, "ONVIF WS-Discovery server error");
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to start ONVIF WS-Discovery server");
                }
            }
        });
        protocol_handles.push(onvif_handle);
    }

    // Create StreamManager early so protocol handlers (ONVIF/GB28181) can
    // reference it for runtime output attachment.
    // Open a second DB connection for StreamManager (WAL mode supports
    // concurrent readers; this lets StreamManager read protocol configs at
    // stream-creation time without contending with the web server's lock).
    let streamer_db_conn = web::db::init_db(&db_path)?;
    let streamer_db = Arc::new(tokio::sync::Mutex::new(streamer_db_conn));
    let stream_manager = Arc::new(web::stream_manager::StreamManager::with_host_and_db(
        advertised_host.clone(),
        streamer_db,
    ));

    // Auto-start streams for cameras with DB status "running" (resume across restart).
    // This ensures the in-memory StreamManager state matches the persistent DB state.
    {
        let running_cameras = {
            let conn = web::db::init_db(&db_path)?;
            let all = web::db::list_cameras(&conn)?;
            all.into_iter().filter(|c| c.status == "running").collect::<Vec<_>>()
        };
        let count = running_cameras.len();
        if count > 0 {
            tracing::info!(count = count, "found cameras with status 'running', auto-starting");
        }
        for camera in running_cameras {
            tracing::info!(camera_id = %camera.id, name = %camera.name, "auto-starting stream");
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_else(|_| "0".to_string());
            match stream_manager
                .create_stream(
                    camera.id.clone(),
                    &camera.camera_type,
                    &camera.config,
                    Some(&rtsp_server),
                )
                .await
            {
                Ok(info) => {
                    tracing::info!(camera_id = %camera.id, rtsp_url = ?info.rtsp_url, "stream auto-started");
                }
                Err(e) => {
                    tracing::warn!(
                        camera_id = %camera.id,
                        error = %e,
                        "failed to auto-start stream; marking as stopped"
                    );
                    // Update DB status to "stopped" since stream cannot start.
                    if let Ok(conn) = web::db::init_db(&db_path) {
                        let _ = web::db::update_camera(
                            &conn,
                            &web::db::CameraRow {
                                status: "stopped".to_string(),
                                updated_at: now,
                                ..camera
                            },
                        );
                    }
                }
            }
        }
        if count > 0 {
            tracing::info!(count = count, "auto-start complete");
        }
    }

    if gb28181_enabled {
        let device_id = config.gb28181.device_id.clone();
        let sip_addr = config.gb28181.platform_sip_address.clone();
        let sip_port = config.gb28181.platform_sip_port;
        let password = config.gb28181.password.clone();
        let sip_domain = config.gb28181.sip_domain.clone();
        let register_interval = config.gb28181.register_interval_secs;

        let stream_manager_for_gb = stream_manager.clone();
        let gb28181_handle = tokio::spawn(async move {
            // Parse SIP server address
            let sip_server_addr: SocketAddr = match format!("{}:{}", sip_addr, sip_port).parse() {
                Ok(addr) => addr,
                Err(e) => {
                    tracing::error!(error = %e, sip_addr = %sip_addr, sip_port = %sip_port, "Invalid GB28181 SIP address");
                    return;
                }
            };

            // Get local IP address for SIP messages
            let local_ip = get_local_ip_for_server(&sip_server_addr)
                .unwrap_or_else(|_| "127.0.0.1".to_string());

            tracing::info!(
                device_id = %device_id,
                sip_server = %sip_server_addr,
                local_ip = %local_ip,
                "GB28181 Device SIP registration starting"
            );

            // Bind UDP socket for SIP communication
            let sip_socket = match tokio::net::UdpSocket::bind("0.0.0.0:0").await {
                Ok(socket) => socket,
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
            use std::collections::HashSet;
            use std::sync::Arc;
            use tokio::sync::Mutex;
            let processed_invites: Arc<Mutex<HashSet<String>>> =
                Arc::new(Mutex::new(HashSet::new()));
            // Track active RTP push outputs by Call-ID → camera_id for BYE cleanup.
            // (Outputs are now attached to StreamHub via stream_manager; we only
            // track the mapping here so BYE can log which camera was involved.)
            let mut gb28181_outputs: HashMap<String, (String, streaming::hub::OutputId)> =
                HashMap::new();

            // Registration state
            let mut registered = false;
            let mut retry_count = 0u32;
            let mut backoff_secs = 1u64;
            const MAX_RETRIES: u32 = 5;

            // SIP message buffer
            let mut recv_buf = [0u8; 8192];

            loop {
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
                            // Max retries reached, switch to 60s interval
                            backoff_secs = 60;
                        } else {
                            backoff_secs = backoff_secs.min(8) * 2; // Exponential backoff: 1, 2, 4, 8
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;
                        continue;
                    }

                    tracing::info!("REGISTER sent to {}", sip_server_addr);
                }

                // Wait for response with timeout
                match tokio::time::timeout(
                    tokio::time::Duration::from_secs(5),
                    sip_socket.recv_from(&mut recv_buf),
                )
                .await
                {
                    Ok(Ok((len, from))) => {
                        if from != sip_server_addr {
                            tracing::debug!(
                                "Ignoring SIP message from {} (expected {})",
                                from,
                                sip_server_addr
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
                                                        tracing::warn!(error = %e, "Failed to send authenticated REGISTER");
                                                    } else {
                                                        tracing::info!(
                                                            "Authenticated REGISTER sent"
                                                        );
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::error!(error = %e, "Failed to parse 401 challenge");
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
                                            // Extract Call-ID for deduplication
                                            let call_id =
                                                msg.get_header("Call-ID").unwrap_or("").to_string();

                                            let mut invites = processed_invites.lock().await;
                                            if invites.contains(&call_id) {
                                                tracing::debug!(call_id = %call_id, "Duplicate INVITE, ignoring");
                                                drop(invites);
                                                continue;
                                            }
                                            invites.insert(call_id.clone());
                                            drop(invites);

                                            tracing::info!(call_id = %call_id, "Received INVITE");

                                            // Parse INVITE to get stream info
                                            match protocols::gb28181::parse_invite(&msg) {
                                                Ok(invite_info) => {
                                                    tracing::info!(
                                                        call_id = %call_id,
                                                        media_address = %invite_info.media_address,
                                                        media_port = %invite_info.media_port,
                                                        "INVITE parsed successfully"
                                                    );

                                                    // Build SDP response (we're sending, so 'sendonly')
                                                    let local_sdp = format!(
                                                        "v=0\r\no={} 0 0 IN IP4 {}\r\ns=Play\r\nc=IN IP4 {}\r\nt=0 0\r\nm=video 0 RTP/AVP 96\r\na=sendonly\r\na=rtpmap:96 PS/90000\r\na=ssrc:{}\r\n",
                                                        device_id,
                                                        local_ip,
                                                        local_ip,
                                                        invite_info.ssrc
                                                    );

                                                    // Build and send 200 OK response
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
                                                            local_tag, cseq,
                                                        );
                                                    let serialized = response.serialize();
                                                    if let Err(e) = sip_socket
                                                        .send_to(
                                                            serialized.as_bytes(),
                                                            sip_server_addr,
                                                        )
                                                        .await
                                                    {
                                                        tracing::error!(error = %e, call_id = %call_id, "Failed to send 200 OK to INVITE");
                                                    } else {
                                                        tracing::info!(call_id = %call_id, "Sent 200 OK to INVITE");
                                                        // Attach GB28181 RTP output to the first active local stream.
                                                        // The platform expects "this device's video feed";
                                                        // with a single webcam, the first active stream is it.
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
                                                                let active = stream_manager_for_gb
                                                                    .list_active_streams()
                                                                    .await;
                                                                match active.first() {
                                                                    Some(info) => {
                                                                        let camera_id =
                                                                            info.camera_id.clone();
                                                                        match stream_manager_for_gb
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
                                                                                    "GB28181 RTP output attached to active stream"
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
                                                                                    "Failed to attach GB28181 output to stream"
                                                                                );
                                                                            }
                                                                        }
                                                                    }
                                                                    None => {
                                                                        tracing::warn!(
                                                                            call_id = %call_id,
                                                                            "GB28181 INVITE received but no active local stream; start a camera first"
                                                                        );
                                                                    }
                                                                }
                                                            }
                                                            Err(e) => {
                                                                tracing::error!(error = %e, call_id = %call_id, media_address = %invite_info.media_address, "Invalid media IP in INVITE");
                                                            }
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::error!(error = %e, call_id = %call_id, "Failed to parse INVITE");
                                                }
                                            }
                                        }
                                        protocols::gb28181::SipMethod::Bye => {
                                            let call_id =
                                                msg.get_header("Call-ID").unwrap_or("").to_string();
                                            tracing::info!(call_id = %call_id, "Received BYE, ending session");
                                            if let Some((camera_id, output_id)) =
                                                gb28181_outputs.remove(&call_id)
                                            {
                                                match stream_manager_for_gb
                                                    .remove_output_from_stream(
                                                        &camera_id, output_id,
                                                    )
                                                    .await
                                                {
                                                    Ok(()) => {
                                                        tracing::info!(call_id = %call_id, camera_id = %camera_id, "GB28181 output detached on BYE")
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!(error = %e, call_id = %call_id, "Failed to detach GB28181 output")
                                                    }
                                                }
                                            } else {
                                                tracing::debug!(call_id = %call_id, "No active RTP push for this session");
                                            }
                                        }
                                        _ => {
                                            tracing::debug!(method = %method, "Received unhandled SIP request");
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Failed to parse SIP message");
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "SIP socket receive error");
                    }
                    Err(_) => {
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
        });
        protocol_handles.push(gb28181_handle);
    }

    // RTMP Push: per-stream, handled by streaming crate (not global startup)
    if config.rtmp_push.enabled {
        tracing::info!("RTMP Push enabled (per-stream via streaming crate)");
    }

    // Periodic session cleanup — runs every 5 minutes to purge expired auth sessions
    let cleanup_db_path = db_path.clone();
    let cleanup_handle = tokio::spawn(async move {
        let interval = tokio::time::Duration::from_secs(300);
        loop {
            tokio::time::sleep(interval).await;
            let path = cleanup_db_path.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || {
                let conn = web::db::init_db(&path)?;
                let removed = security::auth::cleanup_expired_sessions(&conn)?;
                if removed > 0 {
                    tracing::info!(expired_sessions = removed, "Cleaned up expired sessions");
                } else {
                    tracing::debug!("Session cleanup: no expired sessions");
                }
                Ok::<(), anyhow::Error>(())
            })
            .await
            {
                tracing::warn!(error = %e, "Session cleanup task failed");
            }
        }
    });
    protocol_handles.push(cleanup_handle);

    // Shutdown coordination signal
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Signal handler task: listen for SIGINT (ctrl-c) and SIGTERM
    let signal_handle = tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();

        #[cfg(unix)]
        let terminate = async {
            let mut sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("Failed to register SIGTERM handler");
            sig.recv().await;
        };
        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => tracing::info!("Received SIGINT, shutting down"),
            _ = terminate => tracing::info!("Received SIGTERM, shutting down"),
        }

        // Signal the web server to stop. Second signal is a no-op (watch already true).
        let _ = shutdown_tx.send(true);
    });

    println!(
        "mibee-rec server starting on {}:{}...",
        config.web.host, config.web.port
    );
    // Create StreamManager and run server (blocks until shutdown)
    // (StreamManager was created earlier so protocol handlers can reference it.)

    // Build protocol config store for REST API
    let mut protocol_configs = HashMap::new();
    protocol_configs.insert("onvif".into(), serde_json::to_value(&config.onvif).unwrap());
    protocol_configs.insert(
        "gb28181".into(),
        serde_json::to_value(&config.gb28181).unwrap(),
    );
    protocol_configs.insert(
        "rtmp_push".into(),
        serde_json::to_value(&config.rtmp_push).unwrap(),
    );
    let protocol_configs = Arc::new(tokio::sync::Mutex::new(protocol_configs));

    // Run server with graceful shutdown signal
    web::server::run_with_shutdown(
        &config.web.host,
        config.web.port,
        conn,
        stream_manager.clone(),
        rtsp_server,
        protocol_configs,
        shutdown_rx,
        advertised_host.clone(),
    )
    .await?;

    // Abort the signal handler task
    signal_handle.abort();

    // Stop all active streams
    stream_manager.shutdown_all().await;

    // Graceful shutdown: abort all protocol background tasks
    for handle in protocol_handles {
        handle.abort();
    }
    tracing::info!("All protocol tasks shut down");
    Ok(())
}

/// Handle the `--reset-password` CLI flag.
///
/// Prompts for username, current password, and new password, then calls
/// [`security::auth::reset_password`] and prints the result.
async fn reset_password_cli(args: &Args) -> anyhow::Result<()> {
    let db_path = args.db_path.to_string_lossy().to_string();
    let conn = web::db::init_db(&db_path)?;

    let mut input = String::new();

    eprint!("Username: ");
    input.clear();
    std::io::stdin().read_line(&mut input)?;
    let username = input.trim().to_string();

    eprint!("Current password: ");
    input.clear();
    std::io::stdin().read_line(&mut input)?;
    let old_password = input.trim().to_string();

    eprint!("New password: ");
    input.clear();
    std::io::stdin().read_line(&mut input)?;
    let new_password = input.trim().to_string();

    eprint!("Confirm new password: ");
    input.clear();
    std::io::stdin().read_line(&mut input)?;
    let confirm = input.trim().to_string();

    if new_password != confirm {
        eprintln!("Error: passwords do not match");
        std::process::exit(1);
    }

    match security::auth::reset_password(&conn, &username, &old_password, &new_password) {
        Ok(()) => {
            println!("Password reset successfully for user '{}'", username);
            println!("All existing sessions have been invalidated — please log in again.");
            Ok(())
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

/// Get the local IP address that can reach the given server address.
///
/// Uses a simple heuristic by creating a UDP socket and connecting to the server,
/// then reading the local address. This works for both IPv4 and IPv6.
fn get_local_ip_for_server(server_addr: &SocketAddr) -> anyhow::Result<String> {
    use std::net::UdpSocket;

    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(server_addr)?;
    let local_addr = socket.local_addr()?;
    Ok(local_addr.ip().to_string())
}

/// Default port for the ONVIF device service HTTP endpoint.
///
/// This is a conventional port for ONVIF SOAP/HTTP device service.
/// WS-Discovery uses UDP 3702, but the device service runs on a separate HTTP port.
const ONVIF_HTTP_PORT: u16 = 8080;

/// Get ONVIF XAddrs (device service URLs) for all non-loopback IPv4 interfaces.
///
/// Each XAddr is in the format `http://{ip}:{port}/onvif/device_service`.
/// Loopback addresses (127.x.x.x) and unspecified addresses (0.0.0.0) are excluded.
/// If enumeration fails (e.g., on unsupported platforms), returns an empty vec.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_onvif_xaddrs_url_format() {
        let xaddrs = get_onvif_xaddrs(8080);
        for xaddr in &xaddrs {
            assert!(
                xaddr.starts_with("http://"),
                "xaddr should start with http://"
            );
            assert!(
                xaddr.ends_with("/onvif/device_service"),
                "xaddr should end with /onvif/device_service"
            );
            assert!(
                xaddr.contains(":8080/"),
                "xaddr should contain specified port"
            );
        }
        // Verify no loopback or unspecified addresses
        for xaddr in &xaddrs {
            let ip_str = xaddr
                .strip_prefix("http://")
                .and_then(|s| s.split(':').next())
                .expect("should have IP part");
            let ip: Ipv4Addr = ip_str.parse().expect("should be valid IPv4");
            assert!(!ip.is_loopback(), "loopback should be excluded");
            assert!(!ip.is_unspecified(), "unspecified should be excluded");
        }
    }

    #[test]
    fn test_get_onvif_xaddrs_port_parameter() {
        let xaddrs_8080 = get_onvif_xaddrs(8080);
        let xaddrs_8443 = get_onvif_xaddrs(8443);
        assert_eq!(
            xaddrs_8080.len(),
            xaddrs_8443.len(),
            "same interfaces should produce same count for different ports"
        );
        for (a, b) in xaddrs_8080.iter().zip(xaddrs_8443.iter()) {
            assert!(a.contains(":8080/"));
            assert!(b.contains(":8443/"));
        }
    }

    #[test]
    fn test_get_onvif_xaddrs_empty_without_interfaces() {
        // When no interfaces are available, function returns empty vec
        // This test validates the function doesn't panic
        let xaddrs = get_onvif_xaddrs(8080);
        // If we have interfaces, they should all be valid;
        // if we don't, the empty vec is fine
        for xaddr in &xaddrs {
            assert!(xaddr.starts_with("http://"));
        }
    }
}
