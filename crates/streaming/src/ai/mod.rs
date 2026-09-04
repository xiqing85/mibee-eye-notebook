//! On-device AI object detection (NanoDet-Plus ONNX via ONNX Runtime).
//!
//! Mirrors the `mibee-eye-raspi-rs` AI pipeline so all MiBee cameras share
//! one detection semantics: the same `nanodet-plus-m_320` model, the same
//! preprocessing constants, and the same GFL post-processing. Results are
//! exposed through the unified Web API (SPEC v1 §4.6 `GET /api/detections`,
//! `ai_detection` SSE events) with bboxes in **video pixel coordinates**.
//!
//! # Frame source
//!
//! This device's pipeline encodes frames to H.264 before they reach the
//! [`StreamHub`](crate::hub::StreamHub), so the AI worker taps the JPEG
//! preview broadcast that [`VideoCaptureSource`](crate::capture_source::VideoCaptureSource)
//! already maintains for the web UI. Inference therefore never touches the
//! capture/encode path.
//!
//! # Lifecycle
//!
//! [`AiEngine::from_config`] loads the model once (fail-open: a missing
//! model or runtime library leaves the engine inactive, never fake data).
//! [`StreamManager`](../../web/struct.StreamManager.html) spawns one worker
//! per camera stream; the worker exits when the JPEG broadcast closes
//! (stream stopped).

#[cfg(feature = "ai")]
pub mod ort;
pub mod postprocess;
pub mod preprocess;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{info, warn};

/// A single object detection in video pixel coordinates (SPEC v1 §4.6):
/// `bbox` is `[x, y, w, h]` with the origin at the top-left corner of the
/// camera's native stream resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    pub label: String,
    pub confidence: f32,
    pub bbox: [u32; 4],
}

/// Per-camera detection snapshot served by `GET /api/detections`.
#[derive(Debug, Clone, Serialize)]
pub struct CameraDetections {
    pub camera_id: String,
    pub detections: Vec<Detection>,
    pub frame_number: u64,
    /// Unix timestamp (seconds) of the inference that produced these
    /// detections.
    pub timestamp: u64,
}

/// Event published on every completed inference; bridged to the SSE
/// `ai_detection` event (SPEC v1 §6) by the web layer.
#[derive(Debug, Clone, Serialize)]
pub struct DetectionEvent {
    pub camera_id: String,
    pub detections: Vec<Detection>,
    pub frame_number: u64,
}

/// `[ai]` configuration section (SPEC v1 §5 device-specific config).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiConfig {
    /// Master switch. Off by default — inference is opt-in like the
    /// outbound protocols.
    #[serde(default)]
    pub enabled: bool,
    /// Path to the NanoDet-Plus ONNX model, relative to the working
    /// directory or absolute.
    #[serde(default = "default_model_path")]
    pub model_path: String,
    /// Minimum confidence for a detection to be reported (0..=1).
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f32,
    /// Minimum spacing between inferences per camera in milliseconds.
    #[serde(default = "default_interval_ms")]
    pub interval_ms: u64,
}

fn default_model_path() -> String {
    "models/nanodet-m.onnx".to_string()
}
fn default_confidence_threshold() -> f32 {
    0.5
}
fn default_interval_ms() -> u64 {
    1000
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model_path: default_model_path(),
            confidence_threshold: default_confidence_threshold(),
            interval_ms: default_interval_ms(),
        }
    }
}

/// Pluggable inference backend (ONNX Runtime in production; fakes in tests).
pub trait AiDetector: Send + Sync {
    /// Run detection on one JPEG frame. Returns bboxes already scaled to
    /// the frame's native resolution.
    fn detect(&self, jpeg: &[u8]) -> anyhow::Result<Vec<Detection>>;
    /// Human-readable identifier of the active model.
    fn model_name(&self) -> &str;
}

/// Shared, lock-protected detection state read by the web API.
#[derive(Default)]
pub struct AiState {
    inner: RwLock<HashMap<String, CameraDetections>>,
}

impl AiState {
    /// Latest detections for one camera.
    pub fn get(&self, camera_id: &str) -> Option<CameraDetections> {
        self.inner.read().get(camera_id).cloned()
    }

    /// The most recently updated camera's detections (single-camera devices
    /// have exactly one entry). Used by the camera-less SPEC §4.6 endpoint.
    pub fn latest(&self) -> Option<CameraDetections> {
        self.inner
            .read()
            .values()
            .max_by_key(|cd| cd.timestamp)
            .cloned()
    }

    /// Record a detection snapshot for a camera (called by the engine's
    /// workers; public so the web layer's tests can seed state).
    pub fn update(&self, entry: CameraDetections) {
        self.inner.write().insert(entry.camera_id.clone(), entry);
    }

    fn clear_detections(&self, camera_id: &str) {
        if let Some(entry) = self.inner.write().get_mut(camera_id) {
            entry.detections.clear();
        }
    }

    fn remove(&self, camera_id: &str) {
        self.inner.write().remove(camera_id);
    }
}

/// Process-wide AI engine: owns the model, the shared detection state, and
/// the detection event bus.
pub struct AiEngine {
    config: AiConfig,
    detector: Option<Arc<dyn AiDetector>>,
    /// `None` when inactive; human-readable reason surfaced in logs.
    inactive_reason: Option<String>,
    state: Arc<AiState>,
    event_tx: broadcast::Sender<DetectionEvent>,
}

impl AiEngine {
    /// Build the engine from config, loading the ONNX model when enabled.
    ///
    /// Fail-open: any load failure (feature not compiled in, model file
    /// missing, ONNX Runtime library missing) yields an inactive engine —
    /// the rest of the service keeps running without AI.
    #[must_use]
    pub fn from_config(config: &AiConfig) -> Self {
        let detector = Self::load_detector(config);
        Self::from_parts(config.clone(), detector)
    }

    fn load_detector(config: &AiConfig) -> Option<Arc<dyn AiDetector>> {
        if !config.enabled {
            return None;
        }
        #[cfg(feature = "ai")]
        {
            match ort::OrtDetector::new(&config.model_path) {
                Ok(detector) => {
                    info!(model = %detector.model_name(), "ai: ONNX detector loaded");
                    Some(Arc::new(detector))
                }
                Err(e) => {
                    warn!(error = %e, model_path = %config.model_path, "ai: detector unavailable, AI stays disabled (fail-open)");
                    None
                }
            }
        }
        #[cfg(not(feature = "ai"))]
        {
            warn!("ai: enabled in config but this binary was built without the `ai` feature");
            None
        }
    }

    /// Assemble an engine from explicit parts (test seam: inject a fake
    /// detector or force an inactive engine).
    #[must_use]
    pub fn from_parts(config: AiConfig, detector: Option<Arc<dyn AiDetector>>) -> Self {
        let inactive_reason = if detector.is_none() {
            Some(if config.enabled {
                "detector unavailable (see startup log)".to_string()
            } else {
                "disabled by configuration".to_string()
            })
        } else {
            None
        };
        let (event_tx, _) = broadcast::channel(64);
        Self {
            config,
            detector,
            inactive_reason,
            state: Arc::new(AiState::default()),
            event_tx,
        }
    }

    /// Whether a real detector is loaded and workers may run.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.detector.is_some()
    }

    /// Why the engine is inactive (empty string when active).
    #[must_use]
    pub fn inactive_reason(&self) -> &str {
        self.inactive_reason.as_deref().unwrap_or("")
    }

    /// Identifier of the active model (empty string when inactive).
    #[must_use]
    pub fn model_name(&self) -> &str {
        self.detector.as_ref().map_or("", |d| d.model_name())
    }

    /// Shared detection state for the web API.
    #[must_use]
    pub fn state(&self) -> Arc<AiState> {
        Arc::clone(&self.state)
    }

    /// Subscribe to per-inference detection events.
    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<DetectionEvent> {
        self.event_tx.subscribe()
    }

    /// Spawn the per-camera inference worker.
    ///
    /// The worker consumes the camera's JPEG preview broadcast, runs one
    /// inference per `interval_ms` window, updates the shared state, and
    /// publishes a [`DetectionEvent`]. It exits (clearing the camera's
    /// state) once the broadcast closes, i.e. when the stream stops.
    pub fn spawn_worker(
        &self,
        camera_id: String,
        mut jpeg_rx: broadcast::Receiver<Arc<[u8]>>,
    ) -> tokio::task::JoinHandle<()> {
        let Some(detector) = self.detector.clone() else {
            tracing::debug!(%camera_id, "ai: engine inactive, no worker spawned");
            return tokio::spawn(async {});
        };
        let threshold = self.config.confidence_threshold;
        let interval = Duration::from_millis(self.config.interval_ms.max(1));
        let state = Arc::clone(&self.state);
        let event_tx = self.event_tx.clone();

        info!(%camera_id, model = %detector.model_name(), interval_ms = interval.as_millis() as u64, "ai: detection worker started");

        tokio::spawn(async move {
            let mut last_run = tokio::time::Instant::now()
                .checked_sub(interval)
                .unwrap_or_else(tokio::time::Instant::now);
            let mut frame_number: u64 = 0;
            loop {
                match jpeg_rx.recv().await {
                    Ok(jpeg) => {
                        if last_run.elapsed() < interval {
                            continue;
                        }
                        last_run = tokio::time::Instant::now();
                        frame_number += 1;

                        let detect = detector.clone();
                        let result = tokio::task::spawn_blocking(move || detect.detect(&jpeg))
                            .await
                            .unwrap_or_else(|e| Err(anyhow::anyhow!("inference task failed: {e}")));

                        match result {
                            Ok(mut detections) => {
                                detections.retain(|d| d.confidence >= threshold);
                                observability::increment_ai_inferences();
                                let timestamp = unix_now_secs();
                                state.update(CameraDetections {
                                    camera_id: camera_id.clone(),
                                    detections: detections.clone(),
                                    frame_number,
                                    timestamp,
                                });
                                let _ = event_tx.send(DetectionEvent {
                                    camera_id: camera_id.clone(),
                                    detections,
                                    frame_number,
                                });
                            }
                            Err(e) => {
                                warn!(camera_id = %camera_id, error = %e, "ai: inference error");
                                state.clear_detections(&camera_id);
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(camera_id = %camera_id, skipped, "ai: worker lagged behind JPEG tap");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            state.remove(&camera_id);
            info!(camera_id = %camera_id, "ai: detection worker stopped");
        })
    }
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Deterministic fake detector used across this crate and the web
    /// crate's route tests. Never used in production code paths — an
    /// inactive engine reports `enabled: false` instead of fake data.
    pub(crate) struct FakeDetector {
        pub model: String,
    }

    impl AiDetector for FakeDetector {
        fn detect(&self, _jpeg: &[u8]) -> anyhow::Result<Vec<Detection>> {
            Ok(vec![Detection {
                label: "person".to_string(),
                confidence: 0.9,
                bbox: [10, 20, 30, 40],
            }])
        }
        fn model_name(&self) -> &str {
            &self.model
        }
    }

    fn active_engine() -> AiEngine {
        AiEngine::from_parts(
            AiConfig::default(),
            Some(Arc::new(FakeDetector {
                model: "fake-model.onnx".to_string(),
            })),
        )
    }

    #[test]
    fn test_detection_roundtrip() {
        let det = Detection {
            label: "chair".to_string(),
            confidence: 0.65,
            bbox: [1, 2, 3, 4],
        };
        let json = serde_json::to_string(&det).expect("serialize");
        let back: Detection = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(det, back);
        assert!(json.contains("\"bbox\":[1,2,3,4]"), "bbox is a JSON array");
    }

    #[test]
    fn test_engine_disabled_by_default() {
        let engine = AiEngine::from_config(&AiConfig::default());
        assert!(!engine.is_active());
        assert_eq!(engine.model_name(), "");
        assert_eq!(engine.inactive_reason(), "disabled by configuration");
    }

    #[test]
    fn test_engine_enabled_but_model_missing_fails_open() {
        let cfg = AiConfig {
            enabled: true,
            model_path: "definitely-missing.onnx".into(),
            ..AiConfig::default()
        };
        let engine = AiEngine::from_config(&cfg);
        assert!(!engine.is_active(), "missing model must not fake activity");
    }

    #[test]
    fn test_config_defaults_and_parse() {
        let cfg: AiConfig = serde_json::from_str("{}").expect("empty JSON uses defaults");
        assert!(!cfg.enabled);
        assert_eq!(cfg.model_path, "models/nanodet-m.onnx");
        assert!((cfg.confidence_threshold - 0.5).abs() < f32::EPSILON);
        assert_eq!(cfg.interval_ms, 1000);

        let cfg: AiConfig = serde_json::from_str(
            "{\"enabled\":true,\"model_path\":\"/opt/m.onnx\",\"interval_ms\":250}",
        )
        .expect("parse overrides");
        assert!(cfg.enabled);
        assert_eq!(cfg.model_path, "/opt/m.onnx");
        assert_eq!(cfg.interval_ms, 250);
        assert!((cfg.confidence_threshold - 0.5).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn test_worker_updates_state_and_emits_event() {
        let engine = active_engine();
        let mut events = engine.subscribe_events();
        let (jpeg_tx, jpeg_rx) = broadcast::channel(4);

        engine.spawn_worker("cam-1".to_string(), jpeg_rx);

        let jpeg: Arc<[u8]> = Arc::from([0xFF, 0xD8, 0x00, 0xFF, 0xD9].as_slice());
        jpeg_tx.send(jpeg).expect("send jpeg");

        let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event within timeout")
            .expect("event channel open");
        assert_eq!(event.camera_id, "cam-1");
        assert_eq!(event.detections.len(), 1);
        assert_eq!(event.detections[0].label, "person");

        let snapshot = engine.state().get("cam-1").expect("state updated");
        assert_eq!(snapshot.detections.len(), 1);
        assert_eq!(snapshot.frame_number, 1);
    }

    #[tokio::test]
    async fn test_worker_clears_state_when_stream_stops() {
        let engine = active_engine();
        let (jpeg_tx, jpeg_rx) = broadcast::channel(4);
        engine.spawn_worker("cam-2".to_string(), jpeg_rx);
        let jpeg: Arc<[u8]> = Arc::from([0xFF, 0xD8, 0x00, 0xFF, 0xD9].as_slice());
        let _ = jpeg_tx.send(jpeg);

        // Wait until the worker recorded the frame, then drop all senders.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(engine.state().get("cam-2").is_some());
        drop(jpeg_tx);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            engine.state().get("cam-2").is_none(),
            "worker must clear state on stream stop"
        );
    }

    #[tokio::test]
    async fn test_state_latest_picks_most_recent() {
        let engine = active_engine();
        engine.state().update(CameraDetections {
            camera_id: "a".into(),
            detections: vec![],
            frame_number: 1,
            timestamp: 100,
        });
        engine.state().update(CameraDetections {
            camera_id: "b".into(),
            detections: vec![],
            frame_number: 2,
            timestamp: 200,
        });
        assert_eq!(engine.state().latest().expect("some").camera_id, "b");
    }
}
