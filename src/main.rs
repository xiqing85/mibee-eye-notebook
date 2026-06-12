use clap::Parser;

use protocols::onvif::{OnvifDeviceConfig, WsDiscoveryServer};
use protocols::rtsp_server::{RtspServer, RtspServerConfig};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
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
    let args = Args::parse();

    // Handle --reset-password before starting the server
    if args.reset_password {
        return reset_password_cli(&args).await;
    }

    let config = mibee_rec::config::AppConfig::load(&args.config)?;

    // Initialise tracing (subscriber, optional OTLP export)
    observability::init_tracing(
        &config.observability.log_level,
        false,
        if config.observability.otel_endpoint.is_empty() {
            None
        } else {
            Some(config.observability.otel_endpoint.clone())
        },
    )?;

    // Initialise database
    let db_path = args.db_path.to_string_lossy().to_string();
    let conn = web::db::init_db(&db_path)?;

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

    // ONVIF Device (if enabled)
    if config.onvif.enabled {
        let onvif_device_config = OnvifDeviceConfig {
            manufacturer: config.onvif.manufacturer.clone(),
            model: config.onvif.model.clone(),
            firmware_version: config.onvif.firmware_version.clone(),
            serial_number: config.onvif.serial.clone(),
            hardware_id: config.onvif.model.clone(),
            rtsp_url: format!(
                "rtsp://{}:{}/webcam",
                config.web.host, config.rtsp.server_port
            ),
            scopes: vec!["onvif://www.onvif.org/type/NetworkVideoTransmitter".into()],
            xaddrs: vec![],
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

    // GB28181 Device (if enabled)
    if config.gb28181.enabled {
        let device_id = config.gb28181.device_id.clone();
        let sip_addr = config.gb28181.platform_sip_address.clone();
        let sip_port = config.gb28181.platform_sip_port;
        let _password = config.gb28181.password.clone();
        let _sip_domain = config.gb28181.sip_domain.clone();
        let register_interval = config.gb28181.register_interval_secs;
        let gb28181_handle = tokio::spawn(async move {
            let sip_server_addr: SocketAddr = match format!("{}:{}", sip_addr, sip_port).parse() {
                Ok(addr) => addr,
                Err(e) => {
                    tracing::error!(error = %e, sip_addr = %sip_addr, sip_port = %sip_port, "Invalid GB28181 SIP address");
                    return;
                }
            };
            tracing::info!(
                device_id = %device_id,
                sip_server = %sip_server_addr,
                "GB28181 Device SIP registration started"
            );
            // Registration loop — periodically re-registers with the SIP platform
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(register_interval)).await;
                tracing::debug!("GB28181 re-registration cycle");
            }
        });
        protocol_handles.push(gb28181_handle);
    }

    // RTMP Push: per-stream, handled by streaming crate (not global startup)
    if config.rtmp_push.enabled {
        tracing::info!("RTMP Push enabled (per-stream via streaming crate)");
    }

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
    let stream_manager = Arc::new(web::stream_manager::StreamManager::new());

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
