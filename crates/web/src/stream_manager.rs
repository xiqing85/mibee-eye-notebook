//! Central manager for camera stream lifecycles.
//!
//! [`StreamManager`] tracks active streams in a [`HashMap`], creates appropriate
//! [`Source`] + [`Output`] + [`StreamHub`] pipelines for each camera, and
//! manages the start/stop lifecycle of each stream.
//!
//! # Resource limits
//!
//! The manager uses [`ResourceController`] to bound concurrent streams
//! (default max 16). Stream creation will block or fail when the limit
//! is reached.
//!
//! # Example
//!
//! ```ignore
//! let manager = StreamManager::new();
//!
//! // Start a USB camera on /dev/video0
//! let info = manager
//!     .create_stream(
//!         "cam-1".into(),
//!         "usb",
//!         &serde_json::json!({"device_index": 0}),
//!         None,
//!     )
//!     .await?;
//!
//! // Stop it
//! manager.stop_stream("cam-1").await?;
//! ```

use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::{RwLock, oneshot, watch};
use tracing::{info, warn};
use uuid::Uuid;

use protocols::rtsp_server::RtspServer;
use streaming::hub::{HubHandle, OutputId, StreamHub};
use streaming::output::{FileOutput, Output, RtmpOutput, RtspOutput};
use streaming::resource::ResourceController;
use streaming::source::Source;

// ── Constants ────────────────────────────────────────────────────────────────

/// Minimal SDP body for H.264 video over RTP.
const SDP_BODY: &str = concat!(
    "v=0\r\n",
    "o=- 0 0 IN IP4 0.0.0.0\r\n",
    "s=mibee-rec\r\n",
    "c=IN IP4 0.0.0.0\r\n",
    "t=0 0\r\n",
    "m=video 0 RTP/AVP 96\r\n",
    "a=rtpmap:96 H264/90000\r\n",
    "a=fmtp:96 packetization-mode=1\r\n",
    "a=control:track1\r\n",
);

/// Default RTSP port for stream URLs.
const RTSP_PORT: u16 = 8554;

/// Default maximum concurrent streams.
const DEFAULT_MAX_STREAMS: usize = 16;

// ── StreamStatus ─────────────────────────────────────────────────────────────

/// The operational status of a managed stream.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum StreamStatus {
    /// Stream is actively running.
    Running,
    /// Stream has been stopped (graceful shutdown).
    Stopped,
    /// Stream encountered an error.
    #[serde(rename = "error")]
    Error(String),
}

// ── StreamInfo ───────────────────────────────────────────────────────────────

/// Response type for stream info queries.
#[derive(Debug, Clone, Serialize)]
pub struct StreamInfo {
    /// Camera identifier.
    pub camera_id: String,
    /// RTSP URL for clients to connect to (if configured).
    pub rtsp_url: Option<String>,
    /// RTMP URL for clients to connect to (if configured).
    pub rtmp_url: Option<String>,
    /// Current stream status.
    pub status: StreamStatus,
}

// ── StreamHandle ─────────────────────────────────────────────────────────────

struct StreamHandle {
    /// Join handle for the spawned pipeline task.
    join_handle: Option<tokio::task::JoinHandle<()>>,
    /// Watch sender to signal stream stop.
    stop_tx: Option<watch::Sender<bool>>,
    /// RTSP URL for external access.
    rtsp_url: Option<String>,
    /// RTMP URL we are pushing to (if RTMP push enabled).
    rtmp_url: Option<String>,
    /// Handle for attaching outputs at runtime (None if stream not running
    /// or hub handle already extracted).
    hub_handle: Option<HubHandle>,
    /// Current status.
    status: StreamStatus,
}

// ── StreamManager ────────────────────────────────────────────────────────────

/// Central manager for camera stream lifecycles.
///
/// Tracks active streams, creates capture → encode → stream pipelines,
/// and enforces resource limits via [`ResourceController`].
pub struct StreamManager {
    /// Active streams keyed by camera ID.
    streams: RwLock<HashMap<String, StreamHandle>>,
    /// Resource controller for bounding concurrent streams.
    resource_controller: ResourceController,
    /// Hostname/IP advertised in stream URLs returned to clients.
    /// Resolved at startup from config or auto-detected LAN IP.
    advertised_host: String,
    /// Optional DB connection for reading protocol configs (RTMP push, etc.).
    /// When None, no protocol-driven outputs are auto-attached.
    db: Option<SqlitePool>,
}

impl StreamManager {
    /// Create a new stream manager with the default max (16) concurrent streams.
    #[tracing::instrument(skip_all)]
    pub fn new() -> Self {
        Self {
            streams: RwLock::new(HashMap::new()),
            resource_controller: ResourceController::new(DEFAULT_MAX_STREAMS),
            advertised_host: "localhost".to_string(),
            db: None,
        }
    }

    /// Create a new stream manager with a custom maximum number of concurrent
    /// streams.
    pub fn with_max_streams(max_streams: usize) -> Self {
        Self {
            streams: RwLock::new(HashMap::new()),
            resource_controller: ResourceController::new(max_streams),
            advertised_host: "localhost".to_string(),
            db: None,
        }
    }

    /// Create a new stream manager with an explicit advertised host.
    ///
    /// The `advertised_host` is used when constructing RTSP/RTMP/etc. URLs
    /// returned to clients so they can reach this machine from the network.
    /// Use this in production; [`new`](Self::new) defaults to `"localhost`.
    pub fn with_host(advertised_host: String) -> Self {
        Self {
            streams: RwLock::new(HashMap::new()),
            resource_controller: ResourceController::new(DEFAULT_MAX_STREAMS),
            advertised_host,
            db: None,
        }
    }

    /// Create a new stream manager with advertised host AND DB connection.
    ///
    /// The DB is used to read protocol configs (e.g., RTMP push enable/url)
    /// at stream-creation time, so Web UI config changes take effect on the
    /// next stream start (no restart required for new streams).
    #[tracing::instrument(skip_all)]
    pub fn with_host_and_db(advertised_host: String, db: SqlitePool) -> Self {
        Self {
            streams: RwLock::new(HashMap::new()),
            resource_controller: ResourceController::new(DEFAULT_MAX_STREAMS),
            advertised_host,
            db: Some(db),
        }
    }

    /// Return the number of available stream slots.
    pub fn available_permits(&self) -> usize {
        self.resource_controller.available_permits()
    }

    /// Return the maximum number of concurrent streams.
    pub fn max_streams(&self) -> usize {
        self.resource_controller.max_streams()
    }

    /// Start streaming from a camera.
    ///
    /// Creates the appropriate [`Source`] based on `camera_type`, sets up
    /// protocol outputs (e.g., RTSP via [`RtspOutput`]), constructs the
    /// pipeline through [`StreamHub`], and spawns it as a background task.
    ///
    /// # Parameters
    ///
    /// * `camera_id` — Unique identifier for the camera.
    /// * `camera_type` — Type of camera (`"usb"`).
    /// * `config` — JSON configuration with type-specific fields:
    ///   - `"usb"`: `{ "device_index": <u64> }`
    /// * `rtsp_server` — Optional [`RtspServer`] for publishing the stream
    ///   so RTSP clients can connect.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// * The camera type is unsupported.
    /// * The resource limit has been reached (use [`available_permits`](Self::available_permits)).
    /// * A stream for this `camera_id` already exists.
    /// * Source creation or pipeline setup fails.
    #[tracing::instrument(skip_all, fields(camera_id))]
    pub async fn create_stream(
        &self,
        camera_id: String,
        camera_type: &str,
        config: &serde_json::Value,
        rtsp_server: Option<&RtspServer>,
    ) -> Result<StreamInfo> {
        // ── 1. Guard: resource limit ───────────────────────────────────
        //
        // Acquire a permit *before* creating the source so we fail fast
        // when the system is at capacity.
        let _permit = self.resource_controller.try_acquire().ok_or_else(|| {
            anyhow::anyhow!(
                "all {} stream slots are exhausted; try again later",
                DEFAULT_MAX_STREAMS
            )
        })?;

        // ── 2. Guard: duplicate stream ─────────────────────────────────
        {
            let streams = self.streams.read().await;
            if streams.contains_key(&camera_id) {
                anyhow::bail!("stream already exists for camera {camera_id}");
            }
        }

        // ── 3. Create source based on camera type ───────────────────────
        let source: Box<dyn Source> = match camera_type {
            "usb" => {
                let device_index = config
                    .get("device_index")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| {
                        anyhow::anyhow!("USB camera config must include 'device_index'")
                    })?;
                Box::new(streaming::capture_source::VideoCaptureSource::new(
                    device_index as usize,
                ))
            }
            other => anyhow::bail!("unsupported camera type: {other}"),
        };

        // ── 4. Set up pipeline ──────────────────────────────────────────
        let (stop_tx, stop_rx) = watch::channel(false);
        let rtsp_base = format!("rtsp://{}:{}", self.advertised_host, RTSP_PORT);

        let (run_handle, rtsp_url, rtmp_url, hub_handle) = {
            let mut hub = StreamHub::new(source, self.resource_controller.clone());
            let stream_url: Option<String>;

            // Register with RTSP server if provided.
            if let Some(server) = rtsp_server {
                let stream_path = format!("live/{}", camera_id);
                let ssrc = Uuid::new_v4().as_u128() as u32;

                // Oneshot channel to receive SPS/PPS from RtspOutput and update the SDP.
                let (sps_pps_tx, sps_pps_rx) = oneshot::channel::<(Vec<u8>, Vec<u8>)>();
                let server_clone = server.clone();
                let path_clone = stream_path.clone();

                tokio::spawn(async move {
                    if let Ok((sps, pps)) = sps_pps_rx.await {
                        server_clone.update_sps_pps(&path_clone, sps, pps);
                        info!(path = %path_clone, "SDP updated with sprop-parameter-sets");
                    }
                });

                let frame_tx =
                    server.register_live_stream(stream_path.clone(), SDP_BODY.to_string(), ssrc);

                let mut output = RtspOutput::with_channel(
                    stream_path.clone(),
                    SDP_BODY.to_string(),
                    ssrc,
                    frame_tx,
                );
                output.set_sps_pps_tx(sps_pps_tx);

                hub.add_output(Box::new(output)).await;
                stream_url = Some(format!("{}/{}", rtsp_base, stream_path));
                info!(%camera_id, rtsp_url = %stream_url.as_ref().unwrap(), "RTSP output registered");
            } else {
                stream_url = None;
            }

            // ── 4b. Optional RTMP push output ───────────────────────────
            //
            // If the DB-backed `rtmp_push` config has `enabled: true`,
            // construct an RtmpOutput from push_url + stream_name and
            // attach it to the hub. Frames will be pushed to the external
            // RTMP ingest point in parallel with RTSP serving.
            //
            // Read happens at stream creation time; toggling RTMP via
            // Web UI requires stopping and restarting the stream.
            let mut rtmp_url: Option<String> = None;
            if let Some(db) = &self.db {
                match crate::db::get_protocol_config(db, "rtmp_push").await {
                    Ok(Some(cfg)) => {
                        let enabled = cfg
                            .get("enabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if enabled {
                            let push_url =
                                cfg.get("push_url").and_then(|v| v.as_str()).unwrap_or("");
                            let stream_name = cfg
                                .get("stream_name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("stream");
                            if !push_url.is_empty() {
                                let full_url = if push_url.ends_with(stream_name) {
                                    push_url.to_string()
                                } else {
                                    format!("{}/{}", push_url.trim_end_matches('/'), stream_name)
                                };
                                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    RtmpOutput::new(&full_url)
                                })) {
                                    Ok(mut output) => {
                                        if let Err(e) = output.start().await {
                                            warn!(%camera_id, error = %e, "RTMP output failed to start; stream will continue without RTMP push");
                                        } else {
                                            info!(%camera_id, rtmp_url = %full_url, "RTMP push output attached");
                                            hub.add_output(Box::new(output)).await;
                                            rtmp_url = Some(full_url);
                                        }
                                    }
                                    Err(_) => {
                                        warn!(%camera_id, "RtmpOutput::new panicked; skipping RTMP push");
                                    }
                                }
                            } else {
                                warn!(%camera_id, "RTMP push enabled but push_url is empty; skipping");
                            }
                        }
                    }
                    Ok(None) => {
                        // No rtmp_push config in DB — silently skip.
                    }
                    Err(e) => {
                        warn!(%camera_id, error = %e, "failed to read rtmp_push config from DB; skipping RTMP push");
                    }
                }
            }

            // ── 4c. Optional local recording output ──────────────────────
            //
            // If the DB-backed `recording` config has `enabled: true`,
            // construct a FileOutput that muxes H.264 into rolling MP4
            // segments. ffmpeg handles segmentation; we run a periodic
            // pruning pass to enforce the capacity limit.
            if let Some(db) = &self.db {
                match crate::db::get_protocol_config(db, "recording").await {
                    Ok(Some(cfg)) => {
                        let enabled = cfg
                            .get("enabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if enabled {
                            let path = cfg
                                .get("path")
                                .and_then(|v| v.as_str())
                                .unwrap_or("./recordings");
                            let seg = cfg
                                .get("segment_duration_secs")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(900);
                            let cap = cfg
                                .get("max_capacity_mb")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(10_240);
                            let mut output = FileOutput::new(path, &camera_id, seg, cap);
                            match output.start().await {
                                Ok(()) => {
                                    info!(%camera_id, path, segment_secs = seg, "FileOutput attached");
                                    hub.add_output(Box::new(output)).await;
                                }
                                Err(e) => {
                                    warn!(%camera_id, error = %e, "FileOutput failed to start; stream will continue without local recording");
                                }
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!(%camera_id, error = %e, "failed to read recording config; skipping local recording");
                    }
                }
            }

            // Start the pipeline.
            let handle = hub.run().await;

            // Capture a runtime handle BEFORE moving hub into the monitor task.
            // This lets external callers (e.g., GB28181 INVITE handler) attach
            // additional outputs to this stream after it has started.
            let hub_handle = hub.handle();

            // Spawn a monitor task that calls hub.stop() when the external
            // stop signal is received.
            let cam_id = camera_id.clone();
            tokio::spawn(async move {
                let mut rx = stop_rx;
                let _ = rx.changed().await;
                info!(%cam_id, "stream stop signal received, stopping pipeline");
                hub.stop();
            });

            (handle, stream_url, rtmp_url, Some(hub_handle))
        };

        // ── 5. Store the stream handle ──────────────────────────────────
        let handle = StreamHandle {
            join_handle: Some(run_handle),
            stop_tx: Some(stop_tx),
            rtsp_url: rtsp_url.clone(),
            rtmp_url: rtmp_url.clone(),
            hub_handle,
            status: StreamStatus::Running,
        };

        {
            let mut streams = self.streams.write().await;
            streams.insert(camera_id.clone(), handle);
        }

        info!(%camera_id, %camera_type, rtsp_url = ?rtsp_url, "stream created");
        let status = StreamStatus::Running;

        Ok(StreamInfo {
            camera_id,
            rtsp_url,
            rtmp_url,
            status,
        })
    }

    /// Stop a running stream.
    ///
    /// Sends the stop signal, awaits the pipeline task with a 5-second
    /// timeout, and removes the stream from tracking.
    ///
    /// Returns an error if the camera ID is not found.
    #[tracing::instrument(skip_all, fields(camera_id))]
    pub async fn stop_stream(&self, camera_id: &str) -> Result<StreamInfo> {
        // Remove the handle from tracking first so concurrent calls see it gone.
        let mut handle = {
            let mut streams = self.streams.write().await;
            streams
                .remove(camera_id)
                .ok_or_else(|| anyhow::anyhow!("no active stream for camera {camera_id}"))?
        };

        // Signal stop.
        if let Some(tx) = handle.stop_tx.take() {
            let _ = tx.send(true);
        }

        // Await the pipeline task with timeout.
        if let Some(jh) = handle.join_handle.take() {
            match tokio::time::timeout(std::time::Duration::from_secs(5), jh).await {
                Ok(Ok(())) => {
                    info!(%camera_id, "stream stopped gracefully");
                }
                Ok(Err(e)) => {
                    warn!(%camera_id, error = %e, "pipeline task panicked");
                }
                Err(_) => {
                    warn!(%camera_id, "stream stop timed out after 5s, abandoning handle");
                }
            }
        }

        handle.status = StreamStatus::Stopped;

        info!(%camera_id, "stream stopped");
        Ok(StreamInfo {
            camera_id: camera_id.to_string(),
            rtsp_url: handle.rtsp_url,
            rtmp_url: handle.rtmp_url,
            status: StreamStatus::Stopped,
        })
    }

    /// Stop all active streams during graceful shutdown.
    #[tracing::instrument(skip_all)]
    pub async fn shutdown_all(&self) {
        let camera_ids: Vec<String> = {
            let streams = self.streams.read().await;
            streams.keys().cloned().collect()
        };
        for id in camera_ids {
            if let Err(e) = self.stop_stream(&id).await {
                warn!(%id, error = %e, "failed to stop stream during shutdown");
            }
        }
    }

    /// Return information about all currently tracked streams.
    #[tracing::instrument(skip_all)]
    pub async fn list_active_streams(&self) -> Vec<StreamInfo> {
        let streams = self.streams.read().await;
        streams
            .iter()
            .map(|(camera_id, handle)| StreamInfo {
                camera_id: camera_id.clone(),
                rtsp_url: handle.rtsp_url.clone(),
                rtmp_url: handle.rtmp_url.clone(),
                status: handle.status.clone(),
            })
            .collect()
    }

    /// Return the number of currently active (tracked) streams.
    #[tracing::instrument(skip_all)]
    pub async fn active_stream_count(&self) -> usize {
        self.streams.read().await.len()
    }

    /// Check whether a stream for the given camera ID is currently tracked
    /// (active).
    pub async fn has_stream(&self, camera_id: &str) -> bool {
        self.streams.read().await.contains_key(camera_id)
    }

    /// Attach a new output to an already-running stream.
    ///
    /// This is the runtime entry point used by external protocol handlers
    /// (e.g., GB28181 INVITE) to push frames from an active camera pipeline
    /// to a newly-connected consumer.
    ///
    /// Returns `Ok(OutputId)` if the output was attached; `Err` if the camera is
    /// not running or the runtime hub handle is no longer available
    /// (e.g., the stream is shutting down).
    #[tracing::instrument(skip_all, fields(camera_id))]
    pub async fn add_output_to_stream(
        &self,
        camera_id: &str,
        output: Box<dyn Output>,
    ) -> Result<OutputId> {
        let streams = self.streams.read().await;
        let handle = streams
            .get(camera_id)
            .ok_or_else(|| anyhow::anyhow!("no active stream for camera {camera_id}"))?;
        let hub_handle = handle
            .hub_handle
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no hub handle for camera {camera_id}"))?;
        let id = hub_handle.add_output_at_runtime(output).await;
        Ok(id)
    }

    /// Remove a runtime-attached output from a running stream.
    ///
    /// Detaches the output by its [`OutputId`] and signals its task to stop.
    /// Called from protocol handlers (e.g., GB28181 BYE) when an external
    /// consumer disconnects.
    #[tracing::instrument(skip_all, fields(camera_id))]
    pub async fn remove_output_from_stream(
        &self,
        camera_id: &str,
        output_id: OutputId,
    ) -> Result<()> {
        let streams = self.streams.read().await;
        let handle = streams
            .get(camera_id)
            .ok_or_else(|| anyhow::anyhow!("no active stream for camera {camera_id}"))?;
        let hub_handle = handle
            .hub_handle
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no hub handle for camera {camera_id}"))?;
        hub_handle.remove_output(output_id).await;
        Ok(())
    }
}

impl Default for StreamManager {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_stream_manager_new() {
        let manager = StreamManager::new();
        assert_eq!(manager.max_streams(), DEFAULT_MAX_STREAMS);
        assert_eq!(manager.available_permits(), DEFAULT_MAX_STREAMS);
        assert_eq!(manager.active_stream_count().await, 0);
    }

    #[tokio::test]
    async fn test_stream_manager_with_max_streams() {
        let manager = StreamManager::with_max_streams(4);
        assert_eq!(manager.max_streams(), 4);
        assert_eq!(manager.available_permits(), 4);
    }

    #[tokio::test]
    async fn test_list_active_streams_empty() {
        let manager = StreamManager::new();
        let streams = manager.list_active_streams().await;
        assert!(streams.is_empty());
    }

    #[tokio::test]
    async fn test_has_stream_returns_false_for_unknown() {
        let manager = StreamManager::new();
        assert!(!manager.has_stream("nonexistent").await);
    }

    #[tokio::test]
    async fn test_stop_stream_returns_error_for_unknown() {
        let manager = StreamManager::new();
        let err = manager.stop_stream("nonexistent").await.unwrap_err();
        assert!(err.to_string().contains("no active stream"));
    }

    #[tokio::test]
    async fn test_create_stream_rejects_unsupported_type() {
        let manager = StreamManager::new();
        let err = manager
            .create_stream("cam-1".into(), "unknown-type", &serde_json::json!({}), None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("unsupported camera type"),
            "expected unsupported type error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_create_stream_usb_missing_device_index() {
        let manager = StreamManager::new();
        let err = manager
            .create_stream("cam-1".into(), "usb", &serde_json::json!({}), None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("device_index"),
            "expected missing device_index error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_create_stream_rejects_duplicate() {
        // We can't actually create a real source in tests (no hardware), but
        // we can verify the duplicate guard fires.  The create call will fail
        // because VideoCaptureSource::new(9999) tries to open a device that
        // doesn't exist when start() is called inside hub.run().  We rely on
        // the fact that the guard check happens *before* any source creation
        // that could panic.
        //
        // Actually the guard only checks the HashMap.  The source is created
        // before insertion.  So the second call will also try to create a
        // source but fail.  Let's test the duplicate guard directly:
        let manager = StreamManager::new();

        // Pre-insert a dummy handle to test the guard.
        {
            let (_tx, _rx) = watch::channel(false);
            let handle = StreamHandle {
                join_handle: None,
                stop_tx: None,
                rtsp_url: None,
                rtmp_url: None,
                hub_handle: None,
                status: StreamStatus::Running,
            };
            manager.streams.write().await.insert("cam-1".into(), handle);
        }

        let err = manager
            .create_stream(
                "cam-1".into(),
                "usb",
                &serde_json::json!({"device_index": 0}),
                None,
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "expected duplicate error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_list_active_streams_after_insert() {
        let manager = StreamManager::new();

        // Pre-insert a dummy handle.
        {
            let handle = StreamHandle {
                join_handle: None,
                stop_tx: None,
                rtsp_url: Some("rtsp://localhost:8554/live/test".into()),
                rtmp_url: None,
                hub_handle: None,
                status: StreamStatus::Running,
            };
            manager
                .streams
                .write()
                .await
                .insert("test-cam".into(), handle);
        }

        let streams = manager.list_active_streams().await;
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].camera_id, "test-cam");
        assert_eq!(
            streams[0].rtsp_url.as_deref(),
            Some("rtsp://localhost:8554/live/test")
        );
        assert_eq!(streams[0].status, StreamStatus::Running);
        assert!(streams[0].rtmp_url.is_none());
    }

    #[tokio::test]
    async fn test_stop_stream_removes_tracking() {
        let manager = StreamManager::new();

        // Pre-insert a dummy handle with a stop channel so stop_stream works.
        let (stop_tx, _stop_rx) = watch::channel(false);
        {
            let handle = StreamHandle {
                join_handle: None,
                stop_tx: Some(stop_tx),
                rtsp_url: None,
                rtmp_url: None,
                hub_handle: None,
                status: StreamStatus::Running,
            };
            manager
                .streams
                .write()
                .await
                .insert("stop-me".into(), handle);
        }

        // Stop the stream (no join handle to await = immediate).
        let info = manager.stop_stream("stop-me").await.unwrap();
        assert_eq!(info.camera_id, "stop-me");
        assert_eq!(info.status, StreamStatus::Stopped);

        // Should no longer be in the tracked set.
        assert!(!manager.has_stream("stop-me").await);
    }

    #[tokio::test]
    async fn test_active_stream_count() {
        let manager = StreamManager::new();
        assert_eq!(manager.active_stream_count().await, 0);

        // Insert a dummy.
        {
            let handle = StreamHandle {
                join_handle: None,
                stop_tx: None,
                rtsp_url: None,
                rtmp_url: None,
                hub_handle: None,
                status: StreamStatus::Running,
            };
            manager.streams.write().await.insert("a".into(), handle);
        }
        assert_eq!(manager.active_stream_count().await, 1);

        // Insert another.
        {
            let handle = StreamHandle {
                join_handle: None,
                stop_tx: None,
                rtsp_url: None,
                rtmp_url: None,
                hub_handle: None,
                status: StreamStatus::Running,
            };
            manager.streams.write().await.insert("b".into(), handle);
        }
        assert_eq!(manager.active_stream_count().await, 2);

        // Remove one.
        manager.streams.write().await.remove("a");
        assert_eq!(manager.active_stream_count().await, 1);
    }

    #[test]
    fn test_stream_status_serialization() {
        let running = serde_json::to_value(StreamStatus::Running).unwrap();
        assert_eq!(running, serde_json::json!("Running"));

        let stopped = serde_json::to_value(StreamStatus::Stopped).unwrap();
        assert_eq!(stopped, serde_json::json!("Stopped"));

        let err = serde_json::to_value(StreamStatus::Error("oops".into())).unwrap();
        assert_eq!(err, serde_json::json!({"error": "oops"}));
    }

    #[test]
    fn test_stream_info_serialization() {
        let info = StreamInfo {
            camera_id: "cam-x".into(),
            rtsp_url: Some("rtsp://localhost:8554/live/cam-x".into()),
            rtmp_url: None,
            status: StreamStatus::Running,
        };
        let val = serde_json::to_value(&info).unwrap();
        assert_eq!(val["camera_id"], "cam-x");
        assert_eq!(val["rtsp_url"], "rtsp://localhost:8554/live/cam-x");
        assert!(val.get("rtmp_url").unwrap().is_null());
        assert_eq!(val["status"], "Running");
    }

    #[test]
    fn test_stream_info_serialization_roundtrip() {
        let info = StreamInfo {
            camera_id: "cam-y".into(),
            rtsp_url: None,
            rtmp_url: None,
            status: StreamStatus::Error("device disconnected".into()),
        };
        let json = serde_json::to_string(&info).unwrap();
        // We can't deserialize back because StreamInfo doesn't derive
        // Deserialize (it's an output-only type). Just verify the JSON shape.
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["camera_id"], "cam-y");
        assert_eq!(val["status"]["error"], "device disconnected");
    }

    /// Verify that DEFAULT_MAX_STREAMS is consistent.
    #[test]
    fn test_default_max_streams_constant() {
        assert_eq!(DEFAULT_MAX_STREAMS, 16);
    }

    /// Verify that SDP_BODY is a valid-looking SDP.
    #[test]
    fn test_sdp_body_format() {
        assert!(SDP_BODY.starts_with("v=0\r\n"));
        assert!(SDP_BODY.contains("H264/90000"));
        assert!(SDP_BODY.ends_with("\r\n"));
    }
}
