use axum::Json;
use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

pub mod cameras;
pub mod devices;
pub mod protocols;
pub mod settings;
pub mod streams;

use crate::errors::ApiError;

/// Return a seconds-since-epoch timestamp string suitable for DB storage.
pub fn chrono_now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Server start instant, set once on first access.
static START_TIME: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

/// GET /health — returns `{"status": "ok", "uptime": <seconds since start>}`.
pub async fn health_handler() -> Json<Value> {
    let uptime = START_TIME.elapsed().as_secs();
    Json(serde_json::json!({
        "status": "ok",
        "uptime": uptime
    }))
}

/// GET /metrics — returns Prometheus text-0.0.4 exposition format.
pub async fn metrics_handler() -> impl IntoResponse {
    let body = observability::render_metrics();
    let headers = [("content-type", "text/plain; version=0.0.4; charset=utf-8")];
    (StatusCode::OK, headers, body)
}

/// Placeholder handler for not-yet-implemented routes (501).
pub async fn not_implemented_handler() -> impl IntoResponse {
    ApiError::not_implemented("not implemented").into_response()
}

/// Request body for POST /api/auth/login
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// POST /api/auth/login — authenticate and create a session.
///
/// Validates credentials against the stored bcrypt hash.
/// On success, returns a `Set-Cookie: session=<token>; HttpOnly; Secure; SameSite=Strict; Path=/` header
/// and a JSON body `{"status":"ok"}`.
/// Returns 401 on invalid credentials.
pub async fn login_handler(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    Json(body): Json<LoginRequest>,
) -> impl IntoResponse {
    let conn = db.lock().await;

    // Look up stored password hash
    let stored_hash = match security::auth::get_user_password(&conn, &body.username) {
        Ok(Some(h)) => h,
        Ok(None) => {
            return ApiError::unauthorized("invalid credentials").into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "login_handler: failed to query user");
            return ApiError::internal("internal error").into_response();
        }
    };

    // Verify password
    match security::password::verify_password(&body.password, &stored_hash) {
        Ok(true) => {}
        Ok(false) => {
            return ApiError::unauthorized("invalid credentials").into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "login_handler: password verification failed");
            return ApiError::internal("internal error").into_response();
        }
    }

    // Create session
    let token = match security::auth::create_session(&conn, &body.username) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "login_handler: failed to create session");
            return ApiError::internal("internal error").into_response();
        }
    };

    drop(conn);

    tracing::info!(username = %body.username, "User logged in");

    // Set HttpOnly + Secure + SameSite cookie
    let cookie =
        format!("session={token}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=86400");

    (
        StatusCode::OK,
        [("set-cookie", cookie)],
        Json(serde_json::json!({"status": "ok"})),
    )
        .into_response()
}

/// POST /api/auth/logout — invalidate the current session.
///
/// Reads the session cookie, deletes the session from the database,
/// and clears the cookie by setting Max-Age=0.
pub async fn logout_handler(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let token = req
        .headers()
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies
                .split(';')
                .find_map(|c| c.trim().strip_prefix("session="))
                .map(|s| s.to_owned())
        });

    if let Some(token) = token {
        let conn = db.lock().await;
        if let Err(e) = security::auth::invalidate_session(&conn, &token) {
            tracing::warn!(error = %e, "logout_handler: failed to invalidate session");
        }
    }

    // Clear the cookie regardless of whether we found a session
    let cookie = "session=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0";

    (
        StatusCode::OK,
        [("set-cookie", cookie.to_owned())],
        Json(serde_json::json!({"status": "ok"})),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Setup handler
// ---------------------------------------------------------------------------

/// Request body for POST /api/auth/setup
#[derive(Debug, Deserialize)]
pub struct SetupRequest {
    pub username: String,
    pub password: String,
}

/// POST /api/auth/setup — first-run setup (no auth required).
///
/// Creates the initial admin user. Only succeeds if no users exist yet.
/// Also generates a self-signed TLS certificate if not present (best-effort).
/// Returns 200 on success, 400 if already configured or validation fails.
pub async fn setup_handler(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    Json(body): Json<SetupRequest>,
) -> impl IntoResponse {
    // Validate
    if body.username.trim().is_empty() {
        return ApiError::bad_request("username cannot be empty").into_response();
    }
    if body.password.len() < 8 {
        return ApiError::bad_request("password must be at least 8 characters").into_response();
    }

    let conn = db.lock().await;

    // Check if already configured
    match security::auth::is_first_run(&conn) {
        Ok(true) => {} // proceed
        Ok(false) => {
            return ApiError::bad_request("already configured").into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "setup_handler: failed to check first-run status");
            return ApiError::internal("internal error").into_response();
        }
    }

    // Ensure users table exists
    if let Err(e) = security::auth::init_users_table(&conn) {
        tracing::error!(error = %e, "setup_handler: failed to init users table");
        return ApiError::internal("internal error").into_response();
    }

    // Hash the password
    let hash = match security::password::hash_password(&body.password) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "setup_handler: failed to hash password");
            return ApiError::internal("internal error").into_response();
        }
    };

    // Insert the admin user
    if let Err(e) = conn.execute(
        "INSERT INTO users (username, password_hash) VALUES (?1, ?2)",
        rusqlite::params![body.username, hash],
    ) {
        tracing::error!(error = %e, "setup_handler: failed to insert admin user");
        return ApiError::internal("internal error").into_response();
    }

    drop(conn);

    // Generate self-signed TLS cert if not present (best-effort)
    if let Err(e) = security::tls::build_tls_config("tls/cert.pem", "tls/key.pem") {
        tracing::warn!(error = %e, "setup_handler: failed to generate TLS cert");
    }

    tracing::info!(username = %body.username, "Initial admin user created (first-run setup)");

    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
}

// ---------------------------------------------------------------------------
// Password reset handler
// ---------------------------------------------------------------------------

/// Request body for POST /api/auth/reset
#[derive(Debug, Deserialize)]
pub struct ResetPasswordRequest {
    pub old_password: String,
    pub new_password: String,
}

/// POST /api/auth/reset — change password (requires session auth).
///
/// Validates the old password, hashes the new password, updates the user
/// record, and invalidates ALL existing sessions for the user (forces re-login).
/// Returns 200 on success, 401 if the old password is wrong.
pub async fn reset_password_handler(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    Extension(user): Extension<security::middleware::AuthenticatedUser>,
    Json(body): Json<ResetPasswordRequest>,
) -> impl IntoResponse {
    let conn = db.lock().await;
    match security::auth::reset_password(&conn, &user.0, &body.old_password, &body.new_password) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response(),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("incorrect") || msg.contains("not found") {
                ApiError::unauthorized(&msg).into_response()
            } else {
                tracing::error!(error = %msg, "password reset failed");
                ApiError::internal("internal error").into_response()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_health_handler_returns_ok() {
        let resp = health_handler().await;
        assert_eq!(resp.0["status"], "ok");
        let _uptime = resp.0["uptime"].as_u64().expect("uptime should be a u64");
    }

    #[tokio::test]
    async fn test_metrics_handler_returns_text() {
        // register_metrics may already be called by another test — that's fine.
        let _ = observability::register_metrics();

        let resp = metrics_handler().await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.contains("text/plain"),
            "metrics should return text/plain, got: {content_type}"
        );
    }

    #[tokio::test]
    async fn test_not_implemented_called_directly() {
        // Directly test the handler returns 501
        let resp = not_implemented_handler().await.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn test_login_rejects_wrong_credentials() {
        // Use a DB with a seeded user so setup is complete
        let app = crate::server::build_app(crate::server::test_db_with_user());

        let req = Request::builder()
            .uri("/api/auth/login")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "username": "admin",
                    "password": "wrong_password"
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_auth_public_routes_do_not_require_auth() {
        let app = crate::server::build_app(std::sync::Arc::new(tokio::sync::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        )));

        // Auth routes should not return 401 (they are the login/setup endpoints)
        for path in &["/api/auth/login", "/api/auth/setup", "/api/auth/logout"] {
            let req = Request::builder()
                .uri(*path)
                .method("POST")
                .body(Body::empty())
                .unwrap();
            let res = app.clone().oneshot(req).await.unwrap();
            assert_ne!(
                res.status(),
                StatusCode::UNAUTHORIZED,
                "auth route {path} should not require authentication"
            );
        }
    }

    #[tokio::test]
    async fn test_health_and_metrics_are_public() {
        // register_metrics must be called before the handler runs.
        let _ = observability::register_metrics();
        let app = crate::server::build_app(std::sync::Arc::new(tokio::sync::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        )));

        for path in &["/health", "/metrics"] {
            let req = Request::builder().uri(*path).body(Body::empty()).unwrap();
            let res = app.clone().oneshot(req).await.unwrap();
            assert_ne!(
                res.status(),
                StatusCode::UNAUTHORIZED,
                "public route {path} should not require auth"
            );
        }
    }

    #[tokio::test]
    async fn test_unknown_route_returns_404() {
        // Use a seeded DB so setup is complete and the request reaches the router
        let app = crate::server::build_app(crate::server::test_db_with_user());
        let req = Request::builder()
            .uri("/nonexistent")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    // -----------------------------------------------------------------------
    // Setup endpoint tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_setup_creates_admin_user() {
        let db = std::sync::Arc::new(tokio::sync::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        ));
        let app = crate::server::build_app(db.clone());

        let req = Request::builder()
            .uri("/api/auth/setup")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "username": "admin",
                    "password": "securepass123"
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // Verify the user was actually created
        let conn = db.lock().await;
        assert!(!security::auth::is_first_run(&conn).unwrap());
    }

    #[tokio::test]
    async fn test_setup_rejects_after_configured() {
        let db = std::sync::Arc::new(tokio::sync::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        ));
        // Seed a user
        {
            let conn = db.lock().await;
            security::auth::init_users_table(&conn).unwrap();
            let hash = security::password::hash_password("existing_pass").unwrap();
            conn.execute(
                "INSERT INTO users (username, password_hash) VALUES (?1, ?2)",
                rusqlite::params!["admin", hash],
            )
            .unwrap();
        }

        let app = crate::server::build_app(db);

        let req = Request::builder()
            .uri("/api/auth/setup")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "username": "admin",
                    "password": "newpassword123"
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_setup_rejects_short_password() {
        let db = std::sync::Arc::new(tokio::sync::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        ));
        let app = crate::server::build_app(db.clone());

        let req = Request::builder()
            .uri("/api/auth/setup")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "username": "admin",
                    "password": "short"
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        // Verify no user was created
        let conn = db.lock().await;
        assert!(security::auth::is_first_run(&conn).unwrap());
    }

    #[tokio::test]
    async fn test_setup_rejects_empty_username() {
        let db = std::sync::Arc::new(tokio::sync::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        ));
        let app = crate::server::build_app(db);

        let req = Request::builder()
            .uri("/api/auth/setup")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "username": "",
                    "password": "securepass123"
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_setup_blocks_protected_routes_before_setup() {
        let app = crate::server::build_app(std::sync::Arc::new(tokio::sync::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        )));

        let req = Request::builder()
            .uri("/api/cameras")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_setup_allows_health_before_setup() {
        let app = crate::server::build_app(std::sync::Arc::new(tokio::sync::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        )));

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_setup_allows_setup_endpoint_before_setup() {
        let app = crate::server::build_app(std::sync::Arc::new(tokio::sync::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        )));

        // With empty body it returns 422 (deserialization failure), not 503
        let req = Request::builder()
            .uri("/api/auth/setup")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "username": "test",
                    "password": "testtest"
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_ne!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // -----------------------------------------------------------------------
    // Login tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_login_success() {
        let (db, _token) = test_app_with_user();
        let app = crate::server::build_app(db);

        let req = Request::builder()
            .uri("/api/auth/login")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "username": "admin",
                    "password": "current_pass"
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // Verify Set-Cookie header
        let set_cookie = res
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .unwrap();
        assert!(
            set_cookie.starts_with("session="),
            "should set session cookie"
        );
        assert!(set_cookie.contains("HttpOnly"), "cookie should be HttpOnly");
        assert!(set_cookie.contains("Secure"), "cookie should be Secure");
    }

    #[tokio::test]
    async fn test_login_nonexistent_user() {
        let (db, _token) = test_app_with_user();
        let app = crate::server::build_app(db);

        let req = Request::builder()
            .uri("/api/auth/login")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "username": "nobody",
                    "password": "whatever"
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_logout_clears_cookie() {
        let (db, _token) = test_app_with_user();
        let app = crate::server::build_app(db);

        let req = Request::builder()
            .uri("/api/auth/logout")
            .method("POST")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let set_cookie = res
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .unwrap();
        assert!(
            set_cookie.contains("Max-Age=0"),
            "logout should clear cookie"
        );
    }
    // -----------------------------------------------------------------------
    // Password reset tests
    // -----------------------------------------------------------------------

    /// Helper to set up a test app with an in-memory DB that has a seeded user.
    fn test_app_with_user() -> (std::sync::Arc<Mutex<Connection>>, String) {
        let conn = Connection::open_in_memory().unwrap();
        let hash = security::password::hash_password("current_pass").unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS users (
                username TEXT PRIMARY KEY,
                password_hash TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            INSERT INTO users (username, password_hash) VALUES ('admin', '{hash}');
            CREATE TABLE IF NOT EXISTS sessions (
                token TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL
            );"
        ))
        .unwrap();
        let token = security::auth::create_session(&conn, "admin").unwrap();
        let db = std::sync::Arc::new(Mutex::new(conn));
        (db, token)
    }

    #[tokio::test]
    async fn test_reset_password_requires_auth() {
        // Use a seeded DB so setup is complete
        let (db, _token) = test_app_with_user();
        let app = crate::server::build_app(db);

        let req = Request::builder()
            .uri("/api/auth/reset")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "old_password": "x",
                    "new_password": "y"
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_reset_password_success() {
        let (db, token) = test_app_with_user();
        let app = crate::server::build_app(db);

        let req = Request::builder()
            .uri("/api/auth/reset")
            .method("POST")
            .header("content-type", "application/json")
            .header("cookie", format!("session={token}"))
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "old_password": "current_pass",
                    "new_password": "new_secure_pass"
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_reset_password_wrong_old_password() {
        let (db, token) = test_app_with_user();
        let app = crate::server::build_app(db);

        let req = Request::builder()
            .uri("/api/auth/reset")
            .method("POST")
            .header("content-type", "application/json")
            .header("cookie", format!("session={token}"))
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "old_password": "wrong_password",
                    "new_password": "new_pass"
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
}
