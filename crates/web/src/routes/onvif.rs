//! ONVIF device discovery endpoints.
//!
//! All handlers require authentication (enforced by middleware).

use axum::Json;
use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use std::time::Duration;

use crate::routes::error_response;
use security::middleware::AuthenticatedUser;

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/onvif/discover — probe the local network for ONVIF cameras.
///
/// Uses WS-Discovery (UDP multicast on port 3702) with a 5-second timeout.
pub async fn discover(Extension(_user): Extension<AuthenticatedUser>) -> impl IntoResponse {
    match protocols::onvif::discover_devices(Duration::from_secs(5)).await {
        Ok(devices) => {
            let result: Vec<serde_json::Value> = devices
                .into_iter()
                .map(|d| {
                    serde_json::json!({
                        "xaddrs": d.xaddrs,
                        "scopes": d.scopes,
                        "types": d.types,
                        "endpoint": d.endpoint,
                    })
                })
                .collect();
            (StatusCode::OK, Json(result)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "ONVIF discovery error");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "ONVIF discovery failed")
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_onvif_discover_requires_auth() {
        // Create a DB with a seeded user so setup is complete
        let conn = std::sync::Arc::new(tokio::sync::Mutex::new({
            let c = rusqlite::Connection::open_in_memory().unwrap();
            security::auth::init_users_table(&c).unwrap();
            let hash = security::password::hash_password("test_pass").unwrap();
            c.execute(
                "INSERT INTO users (username, password_hash) VALUES (?1, ?2)",
                rusqlite::params!["admin", hash],
            )
            .unwrap();
            c
        }));
        let state = crate::server::AppRouterState {
            db: conn,
            active: crate::server::ActiveStreams::default(),
            stream_manager: Arc::new(crate::stream_manager::StreamManager::new()),
            rtsp_server: Arc::new(protocols::rtsp_server::RtspServer::new(
                protocols::rtsp_server::RtspServerConfig::default(),
            )),
        };
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/onvif/discover")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
}
