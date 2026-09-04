use clap::Parser;

use protocols::rtsp_server::{RtspServer, RtspServerConfig};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, watch};
use web::protocol_runtime::ProtocolRuntime;

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
        _ => get_first_non_loopback_ipv4().unwrap_or_else(|| "127.0.0.1".to_string()),
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

    // Initialise database - create SqlitePool for web CRUD and Connection for security calls
    let db_path_str = args.db_path.to_string_lossy().to_string();
    let pool = web::db::init_pool(&db_path_str).await?;
    let auth_db_conn = web::db::init_auth_db(&db_path_str)?;
    let auth_db = Arc::new(tokio::sync::Mutex::new(auth_db_conn));
    // Seed protocol_configs table from config.toml on first run (when empty).
    // Subsequent runs use the persisted values; users' Web UI edits survive restart.
    if web::db::protocol_configs_is_empty(&pool).await? {
        tracing::info!("seeding protocol_configs from config.toml (first run)");
        web::db::set_protocol_config(&pool, "onvif", &serde_json::to_value(&config.onvif)?).await?;
        web::db::set_protocol_config(&pool, "gb28181", &serde_json::to_value(&config.gb28181)?)
            .await?;
        web::db::set_protocol_config(
            &pool,
            "rtmp_push",
            &serde_json::to_value(&config.rtmp_push)?,
        )
        .await?;
        web::db::set_protocol_config(
            &pool,
            "recording",
            &serde_json::to_value(&config.recording)?,
        )
        .await?;
        web::db::set_protocol_config(&pool, "webrtc", &serde_json::to_value(&config.webrtc)?)
            .await?;
    } else {
        tracing::debug!("protocol_configs table already populated; keeping persisted values");
    }
    let discovered = web::db::auto_discover_cameras(&pool).await?;
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

    // Protocol startup is now handled by ProtocolRuntime (below, after
    // StreamManager is created and streams are auto-started).
    // This allows hot-toggling ONVIF/GB28181/RTMP without server restart.

    // SSE event bus — created early so the AI engine bridge (below) and the
    // hot-plug monitor share one bus with the web server.
    let event_tx = Arc::new(web::routes::events::new_event_bus());

    // AI detection engine (fail-open: a missing model / ONNX Runtime library
    // leaves it inactive; the service runs on without AI).
    let ai_engine = Arc::new(streaming::ai::AiEngine::from_config(&config.ai));
    if !ai_engine.is_active() {
        tracing::info!(reason = %ai_engine.inactive_reason(), "ai: detection disabled");
    }

    // Bridge AI detection events into the SSE event bus (SPEC v1 §6).
    {
        let mut ai_events = ai_engine.subscribe_events();
        let bridge_tx = event_tx.clone();
        tokio::spawn(async move {
            loop {
                match ai_events.recv().await {
                    Ok(ev) => {
                        let _ = bridge_tx.send(web::routes::events::CameraEvent::AiDetection {
                            camera_id: ev.camera_id,
                            detections: ev.detections,
                            frame_number: ev.frame_number,
                        });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "ai: SSE bridge lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // Create StreamManager early so protocol handlers (ONVIF/GB28181) can
    // reference it for runtime output attachment.
    // Open a second DB connection for StreamManager (WAL mode supports
    // concurrent readers; this lets StreamManager read protocol configs at
    // stream-creation time without contending with the web server's lock).
    let streamer_db = pool.clone();
    let stream_manager = Arc::new(
        web::stream_manager::StreamManager::with_host_and_db(advertised_host.clone(), streamer_db)
            .with_ai(ai_engine.clone()),
    );

    // Auto-start streams for cameras with DB status "running" (resume across restart).
    // This ensures the in-memory StreamManager state matches the persistent DB state.
    {
        let running_cameras = {
            let all = web::db::list_cameras(&pool).await?;
            all.into_iter()
                .filter(|c| c.status == "running")
                .collect::<Vec<_>>()
        };
        let count = running_cameras.len();
        if count > 0 {
            tracing::info!(
                count = count,
                "found cameras with status 'running', auto-starting"
            );
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
                    let _ = web::db::update_camera(
                        &pool,
                        &web::db::CameraRow {
                            status: "stopped".to_string(),
                            updated_at: now,
                            ..camera
                        },
                    )
                    .await;
                }
            }
        }
        if count > 0 {
            tracing::info!(count = count, "auto-start complete");
        }
    }

    // ── ProtocolRuntime: hot-toggle ONVIF / GB28181 / RTMP ───────────────
    //
    // Check DB for enabled protocols and start them. This replaces the old
    // inline ONVIF/GB28181 startup code and enables hot-toggling via Web UI.
    let mut protocol_runtime = ProtocolRuntime::new();

    // ONVIF
    {
        let cfg = web::db::get_protocol_config(&pool, "onvif")
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| serde_json::to_value(&config.onvif).unwrap_or_default());
        let enabled = cfg
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if enabled {
            let onvif_config =
                web::protocol_runtime::build_onvif_config_from_json(&cfg, &advertised_host);
            if let Err(e) = protocol_runtime
                .start_onvif(onvif_config, stream_manager.clone())
                .await
            {
                tracing::warn!(error = %e, "failed to start ONVIF at startup");
            }
        }
    }

    // GB28181
    {
        let cfg = web::db::get_protocol_config(&pool, "gb28181")
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| serde_json::to_value(&config.gb28181).unwrap_or_default());
        let enabled = cfg
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if enabled {
            let gb_config = web::protocol_runtime::extract_gb28181_config(&cfg);
            if let Err(e) = protocol_runtime
                .start_gb28181(&gb_config, stream_manager.clone())
                .await
            {
                tracing::warn!(error = %e, "failed to start GB28181 at startup");
            }
        }
    }

    // RTMP (per-stream; just track enabled state)
    {
        let cfg = web::db::get_protocol_config(&pool, "rtmp_push")
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| serde_json::to_value(&config.rtmp_push).unwrap_or_default());
        let enabled = cfg
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if enabled && let Err(e) = protocol_runtime.start_rtmp().await {
            tracing::warn!(error = %e, "failed to enable RTMP at startup");
        }
    }

    let protocol_runtime = Arc::new(Mutex::new(protocol_runtime));

    // ── Hot-plug monitor: auto-discover plugged cameras, mark unplugged ──
    //
    // Listens to kernel netlink uevents for the video4linux subsystem.
    // On ADD: re-runs auto-discovery (does NOT auto-start the stream).
    // On REMOVE: marks the camera offline in DB and gracefully stops its
    //   active stream (flushing any in-progress recording segment).
    let hotplug_tx = event_tx.clone();
    let hotplug_db = pool.clone();
    let hotplug_stream_manager = stream_manager.clone();
    let hotplug_handle = tokio::spawn(async move {
        // Diagnostic escape hatch: MIBEE_DISABLE_HOTPLUG=1 skips the netlink
        // monitor entirely (used to bisect runtime starvation).
        if std::env::var_os("MIBEE_DISABLE_HOTPLUG").is_some_and(|v| v != "0") {
            tracing::warn!("hot-plug monitor disabled via MIBEE_DISABLE_HOTPLUG");
            return;
        }
        let monitor = match capture::hotplug::HotplugMonitor::new() {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "failed to start hot-plug monitor; camera plug/unplug will not be detected");
                return;
            }
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        tokio::spawn(async move {
            monitor.run(tx).await;
        });

        while let Some(event) = rx.recv().await {
            match event {
                capture::hotplug::HotplugEvent::Added { device_index, .. } => {
                    tracing::info!(device_index, "camera plugged in, running auto-discovery");
                    // Re-run discovery. Does NOT auto-start (user decision).
                    match web::db::auto_discover_cameras(&hotplug_db).await {
                        Ok(n) if n > 0 => {
                            // Find the newly-added camera to broadcast event.
                            let cameras =
                                web::db::list_cameras(&hotplug_db).await.unwrap_or_default();
                            for cam in cameras.iter().filter(|c| {
                                c.camera_type == "usb"
                                    && c.config.get("device_index").and_then(|v| v.as_u64())
                                        == Some(device_index as u64)
                            }) {
                                let _ = hotplug_tx.send(
                                    web::routes::events::CameraEvent::CameraAdded {
                                        camera_id: cam.id.clone(),
                                        device_index,
                                        name: cam.name.clone(),
                                    },
                                );
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(error = %e, "auto-discovery after hot-plug failed")
                        }
                    }
                }
                capture::hotplug::HotplugEvent::Removed { device_index } => {
                    tracing::info!(device_index, "camera unplugged, marking offline");
                    match web::db::mark_cameras_offline_by_device_index(
                        &hotplug_db,
                        device_index as i64,
                    )
                    .await
                    {
                        Ok(camera_ids) => {
                            for cam_id in &camera_ids {
                                // Gracefully stop the active stream (flushes FileOutput).
                                if hotplug_stream_manager.has_stream(cam_id).await {
                                    tracing::info!(camera_id = %cam_id, "stopping stream for unplugged camera");
                                    if let Err(e) = hotplug_stream_manager.stop_stream(cam_id).await
                                    {
                                        tracing::warn!(camera_id = %cam_id, error = %e, "failed to stop stream for unplugged camera");
                                    }
                                }
                                // Broadcast SSE event.
                                let _ = hotplug_tx.send(
                                    web::routes::events::CameraEvent::CameraOfflined {
                                        camera_id: cam_id.clone(),
                                        device_index,
                                    },
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, device_index, "failed to mark camera offline");
                        }
                    }
                }
            }
        }
    });
    protocol_handles.push(hotplug_handle);
    // Periodic session cleanup — runs every 5 minutes to purge expired auth sessions
    let cleanup_db_path = db_path_str.clone();
    let cleanup_handle = tokio::spawn(async move {
        let interval = tokio::time::Duration::from_secs(300);
        loop {
            tokio::time::sleep(interval).await;
            let path = cleanup_db_path.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || {
                let conn = web::db::init_auth_db(&path)?;
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

    // Clone for post-server-shutdown cleanup
    let protocol_runtime_for_shutdown = protocol_runtime.clone();

    web::server::run_with_shutdown(
        &config.web.host,
        config.web.port,
        config.web.http_port,
        pool,
        auth_db,
        stream_manager.clone(),
        rtsp_server,
        protocol_configs,
        protocol_runtime,
        shutdown_rx,
        advertised_host.clone(),
        event_tx,
        ai_engine,
    )
    .await?;

    // Abort the signal handler task
    signal_handle.abort();

    // Stop all active streams
    stream_manager.shutdown_all().await;

    // Graceful shutdown of all protocols via ProtocolRuntime
    {
        let mut rt = protocol_runtime_for_shutdown.lock().await;
        rt.shutdown_all().await;
    }

    // Abort remaining background tasks (RTSP server, session cleanup)
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
    let conn = web::db::init_auth_db(&db_path)?;

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

/// Get the first non-loopback IPv4 address via interface enumeration.
/// Falls back to "127.0.0.1" if no suitable interface is found.
fn get_first_non_loopback_ipv4() -> Option<String> {
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    unsafe {
        if libc::getifaddrs(&mut ifap) != 0 {
            return None;
        }
        let mut ip = None;
        let mut ptr = ifap;
        while !ptr.is_null() {
            let ifa = &*ptr;
            if let Some(addr) = ifa.ifa_addr.as_ref()
                && addr.sa_family as libc::c_uint == libc::AF_INET as libc::c_uint
            {
                let sin = addr as *const libc::sockaddr as *const libc::sockaddr_in;
                let ip_addr = Ipv4Addr::from(u32::from_be((*sin).sin_addr.s_addr));
                if !ip_addr.is_loopback() && !ip_addr.is_unspecified() {
                    ip = Some(ip_addr.to_string());
                    break;
                }
            }
            ptr = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
        ip
    }
}
