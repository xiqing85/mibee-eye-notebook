//! AI detection endpoints (SPEC v1 §4.6 + multi-camera dialect).
//!
//! - `GET /api/detections` — the SPEC §4.6 shape: the most recently
//!   inferred camera's detections. Single-camera deployments (the common
//!   case) have exactly one worker, so this is that camera's result.
//! - `GET /api/cameras/{id}/detections` — per-camera dialect for this
//!   multi-camera device (SPEC appendix A notebook dialect).
//!
//! Bboxes are in video pixel coordinates of the camera's native stream
//! resolution (SPEC §4.6). When AI is disabled or unavailable the endpoints
//! return `{"enabled": false}` — never fabricated detections.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path};
use serde_json::json;
use streaming::ai::AiEngine;

use security::middleware::AuthenticatedUser;

/// `GET /api/detections` — latest detections of the most recently inferred
/// camera (SPEC v1 §4.6).
#[tracing::instrument(skip_all)]
pub async fn get_detections(
    Extension(ai): Extension<Arc<AiEngine>>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> Json<serde_json::Value> {
    if !ai.is_active() {
        return Json(json!({ "enabled": false }));
    }
    let snapshot = ai.state().latest();
    let (detections, timestamp) = snapshot
        .map(|s| (s.detections, s.timestamp))
        .unwrap_or_default();
    Json(json!({
        "detections": detections,
        "model": ai.model_name(),
        "timestamp": timestamp,
    }))
}

/// `GET /api/cameras/{id}/detections` — per-camera detections (multi-camera
/// dialect). Unknown cameras report an empty list, mirroring the
/// camera-less endpoint's "enabled but nothing detected yet" shape.
#[tracing::instrument(skip_all, fields(camera_id))]
pub async fn get_camera_detections(
    Path(camera_id): Path<String>,
    Extension(ai): Extension<Arc<AiEngine>>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> Json<serde_json::Value> {
    if !ai.is_active() {
        return Json(json!({ "enabled": false }));
    }
    let snapshot = ai.state().get(&camera_id);
    let (detections, timestamp) = snapshot
        .map(|s| (s.detections, s.timestamp))
        .unwrap_or_default();
    Json(json!({
        "detections": detections,
        "model": ai.model_name(),
        "timestamp": timestamp,
    }))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use streaming::ai::{AiConfig, AiDetector, Detection};
    use tower::ServiceExt;

    struct FixedDetector;
    impl AiDetector for FixedDetector {
        fn detect(&self, _jpeg: &[u8]) -> anyhow::Result<Vec<Detection>> {
            Ok(vec![Detection {
                label: "laptop".to_string(),
                confidence: 0.8,
                bbox: [5, 6, 7, 8],
            }])
        }
        fn model_name(&self) -> &str {
            "test-model.onnx"
        }
    }

    async fn app_with(engine: Arc<AiEngine>) -> Router {
        Router::new()
            .route("/api/detections", get(get_detections))
            .route("/api/cameras/{id}/detections", get(get_camera_detections))
            .layer(Extension(engine))
            .layer(Extension(AuthenticatedUser("tester".to_string())))
    }

    fn active_engine() -> Arc<AiEngine> {
        Arc::new(AiEngine::from_parts(
            AiConfig::default(),
            Some(Arc::new(FixedDetector)),
        ))
    }

    #[tokio::test]
    async fn test_disabled_engine_reports_enabled_false() {
        let engine = Arc::new(AiEngine::from_config(&AiConfig::default()));
        let app = app_with(engine).await;
        let res = app
            .oneshot(Request::get("/api/detections").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["enabled"], false, "disabled engine: {json}");
        assert!(json.get("detections").is_none(), "no fabricated data");
    }

    #[tokio::test]
    async fn test_active_engine_serves_detections() {
        let engine = active_engine();
        // Seed the state the way the worker would.
        engine.state().update(streaming::ai::CameraDetections {
            camera_id: "cam-0".into(),
            detections: vec![Detection {
                label: "laptop".to_string(),
                confidence: 0.8,
                bbox: [5, 6, 7, 8],
            }],
            frame_number: 3,
            timestamp: 1_700_000_000,
        });
        let app = app_with(engine).await;
        let res = app
            .oneshot(Request::get("/api/detections").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["model"], "test-model.onnx");
        assert_eq!(json["timestamp"], 1_700_000_000);
        assert_eq!(json["detections"][0]["label"], "laptop");
        assert_eq!(
            json["detections"][0]["bbox"],
            serde_json::json!([5, 6, 7, 8])
        );
    }

    #[tokio::test]
    async fn test_per_camera_endpoint() {
        let engine = active_engine();
        engine.state().update(streaming::ai::CameraDetections {
            camera_id: "cam-9".into(),
            detections: vec![],
            frame_number: 1,
            timestamp: 42,
        });
        let app = app_with(engine).await;
        let res = app
            .oneshot(
                Request::get("/api/cameras/cam-9/detections")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["timestamp"], 42);
        assert_eq!(json["detections"], serde_json::json!([]));
    }
}
