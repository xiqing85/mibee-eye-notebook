//! SPEC v1 §0 response envelope middleware.
//!
//! Wraps every successful JSON `/api/*` response into
//! `{"ok":true,"data":…}`. Failure responses already carry
//! `{"ok":false,"error","message"}` via [`crate::errors::ApiError`].
//! Binary and streaming endpoints (SSE, MSE, JPEG, metrics, static) pass
//! through untouched, as does the legacy unwrapped `/health`.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

/// Upper bound for envelope buffering (config dumps are small).
const MAX_BODY: usize = 4 << 20;

/// Streaming paths that must never be wrapped (their non-JSON content types
/// would skip them anyway; listed for clarity).
const SKIP_EXACT: &[&str] = &["/api/events"];

pub async fn envelope(req: Request<Body>, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    if !path.starts_with("/api/") || SKIP_EXACT.contains(&path.as_str()) {
        return next.run(req).await;
    }

    let resp = next.run(req).await;
    let status = resp.status();
    if !status.is_success() || status == StatusCode::NO_CONTENT {
        return resp;
    }
    let is_json = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/json"));
    if !is_json {
        return resp;
    }

    let (mut parts, body) = resp.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let inner: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return (parts.status, bytes).into_response(),
    };
    let wrapped = serde_json::json!({ "ok": true, "data": inner });
    let out = serde_json::to_vec(&wrapped).unwrap_or_default();
    if let Ok(len) = out.len().to_string().parse() {
        parts.headers.insert(header::CONTENT_LENGTH, len);
    }
    Response::from_parts(parts, Body::from(out))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::{Json, Router};
    use tower::ServiceExt;

    async fn json_ok() -> Json<Value> {
        Json(serde_json::json!({"status": "ok"}))
    }
    async fn plain() -> &'static str {
        "binary"
    }
    async fn fail() -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "x"})),
        )
            .into_response()
    }

    fn app() -> Router {
        Router::new()
            .route("/api/thing", get(json_ok))
            .route("/api/bin", get(plain))
            .route("/api/fail", get(fail))
            .route("/metrics", get(plain))
            .layer(axum::middleware::from_fn(envelope))
    }

    #[tokio::test]
    async fn wraps_json_success() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/api/thing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), MAX_BODY)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["data"]["status"], "ok");
    }

    #[tokio::test]
    async fn passes_through_non_json_and_errors() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/api/bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            !resp
                .headers()
                .get(header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("json")
        );

        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/api/fail")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), MAX_BODY)
            .await
            .unwrap();
        assert!(serde_json::from_slice::<Value>(&body).unwrap()["ok"].is_null());
    }

    #[tokio::test]
    async fn skips_non_api_paths() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), MAX_BODY)
            .await
            .unwrap();
        assert_eq!(body, "binary");
    }
}
