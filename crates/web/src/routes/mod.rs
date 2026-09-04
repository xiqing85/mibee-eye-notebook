use axum::Json;
use axum::extract::{Extension, Request};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use parking_lot::Mutex as ParkingLotMutex;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Mutex;

pub mod cameras;
pub mod capabilities;
pub mod config_api;
pub mod detections;
pub mod devices;
pub mod events;
pub mod mse;
pub mod protocols;
pub mod settings;
pub mod streams;
pub mod webrtc;

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

/// In-memory tracking of login failures for per-user exponential-backoff lockout.
struct LoginFailures {
    map: HashMap<String, (u32, Instant)>, // username -> (failure_count, last_attempt_time)
}

static LOGIN_FAILURES: std::sync::LazyLock<ParkingLotMutex<LoginFailures>> =
    std::sync::LazyLock::new(|| {
        ParkingLotMutex::new(LoginFailures {
            map: HashMap::new(),
        })
    });

/// Check if `username` is currently locked out.
/// Returns `Some(seconds_remaining)` if locked, `None` otherwise.
fn check_lockout(username: &str) -> Option<u64> {
    let mut failures = LOGIN_FAILURES.lock();
    cleanup_old_entries(&mut failures);

    if let Some(&(count, last_attempt)) = failures.map.get(username)
        && count >= 5
    {
        let elapsed = last_attempt.elapsed().as_secs();
        // Lockout duration doubles with each failure beyond 5: 60s, 120s, 240s, 480s, ...
        let lockout_duration = 60u64 * 2u64.pow(count - 5);
        if elapsed < lockout_duration {
            return Some(lockout_duration - elapsed);
        }
    }
    None
}

/// Record a failed login attempt for `username`.
fn record_failure(username: &str) {
    let mut failures = LOGIN_FAILURES.lock();
    let entry = failures
        .map
        .entry(username.to_string())
        .or_insert((0, Instant::now()));
    entry.0 += 1;
    entry.1 = Instant::now();
}

/// Reset the failure counter for `username` after a successful login.
fn reset_failures(username: &str) {
    let mut failures = LOGIN_FAILURES.lock();
    failures.map.remove(username);
}

/// Prune entries that have not been touched in the last 30 minutes.
fn cleanup_old_entries(failures: &mut LoginFailures) {
    let cutoff = Instant::now()
        .checked_sub(Duration::from_secs(1800))
        .unwrap_or(Instant::now());
    failures.map.retain(|_, v| v.1 > cutoff);
}

/// GET /health — returns `{"status": "ok", "uptime": <seconds since start>}`.
#[tracing::instrument(skip_all)]
pub async fn health_handler() -> Json<Value> {
    let uptime = START_TIME.elapsed().as_secs();
    Json(serde_json::json!({
        "status": "ok",
        "uptime": uptime
    }))
}

/// GET /metrics — returns Prometheus text-0.0.4 exposition format.
#[tracing::instrument(skip_all)]
pub async fn metrics_handler() -> impl IntoResponse {
    let body = observability::render_metrics();
    let headers = [("content-type", "text/plain; version=0.0.4; charset=utf-8")];
    (StatusCode::OK, headers, body)
}

/// GET /api/auth/me — return the authenticated user's identity.
///
/// Allows the SPA to query "who am I?" on load to decide whether to render
/// the app or redirect to login. The project is single-admin by design, so
/// the role is always `"admin"`.
#[tracing::instrument(skip_all)]
pub async fn me_handler(
    Extension(user): Extension<security::middleware::AuthenticatedUser>,
) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "username": user.0,
            "role": "admin",
        })),
    )
}

/// Placeholder handler for not-yet-implemented routes (501).
#[tracing::instrument(skip_all)]
pub async fn not_implemented_handler() -> impl IntoResponse {
    ApiError::not_implemented("not implemented").into_response()
}

/// Marks which listener a request arrived on (SPEC appendix A): session
/// cookies are issued with `Secure` only on the TLS listener, because
/// browsers refuse `Secure` cookies over plain http:// — the optional
/// HTTP port would otherwise be unusable. Requests that never pass
/// through a marker layer (unit tests) default to `Secure`.
#[derive(Debug, Clone, Copy)]
pub struct ListenerScheme(pub bool);

/// Tag requests served by the TLS listener.
pub async fn mark_secure_listener(mut req: Request, next: Next) -> Response {
    req.extensions_mut().insert(ListenerScheme(true));
    next.run(req).await
}

/// Tag requests served by the optional plain-HTTP listener.
pub async fn mark_insecure_listener(mut req: Request, next: Next) -> Response {
    req.extensions_mut().insert(ListenerScheme(false));
    next.run(req).await
}

/// `Secure` attribute for a Set-Cookie issued to this request.
fn secure_flag(secure: bool) -> &'static str {
    if secure { " Secure;" } else { "" }
}

/// Session cookie value shared by login/setup (max-age 24h).
fn session_cookie(token: &str, secure: bool) -> String {
    format!(
        "session={token}; HttpOnly;{} SameSite=Strict; Path=/; Max-Age=86400",
        secure_flag(secure)
    )
}

/// Cleared session cookie for logout.
fn session_cookie_cleared(secure: bool) -> String {
    format!(
        "session=; HttpOnly;{} SameSite=Strict; Path=/; Max-Age=0",
        secure_flag(secure)
    )
}

/// Request body for POST /api/auth/login
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    /// SPEC §2: may be empty/omitted — defaults to "admin" (single-admin form).
    #[serde(default)]
    pub username: String,
    pub password: String,
}

/// POST /api/auth/login — authenticate and create a session.
///
/// Validates credentials against the stored bcrypt hash.
/// On success, returns a `Set-Cookie: session=<token>; HttpOnly; Secure; SameSite=Strict; Path=/` header
/// and a JSON body `{"status":"ok"}`.
/// Returns 401 on invalid credentials.
/// Returns 429 with retry delay when account is locked due to too many failures.
#[tracing::instrument(skip_all, fields(username = %body.username))]
pub async fn login_handler(
    Extension(_db): Extension<SqlitePool>,
    Extension(auth_db): Extension<Arc<Mutex<Connection>>>,
    insecure: Option<Extension<ListenerScheme>>,
    Json(mut body): Json<LoginRequest>,
) -> impl IntoResponse {
    let secure = insecure.map(|Extension(s)| s.0).unwrap_or(true);
    let conn = auth_db.lock().await;

    // SPEC §2: empty/omitted username defaults to "admin" (single-admin login form).
    if body.username.trim().is_empty() {
        body.username = "admin".to_owned();
    }

    // Check login lockout (exponential backoff after 5 failures)
    if let Some(retry_in) = check_lockout(&body.username) {
        observability::increment_auth_failures("locked_out");
        return ApiError::too_many_requests(format!(
            "account locked, try again in {} seconds",
            retry_in
        ))
        .into_response();
    }

    // Look up stored password hash, then release the DB lock before the
    // (expensive, blocking) bcrypt verify — holding it would stall every
    // other DB-backed request for the whole verification.
    let stored_hash = match security::auth::get_user_password(&conn, &body.username) {
        Ok(Some(h)) => h,
        Ok(None) => {
            record_failure(&body.username);
            observability::increment_auth_failures("bad_password");
            return ApiError::unauthorized("invalid credentials").into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "login_handler: failed to query user");
            return ApiError::internal("internal error").into_response();
        }
    };
    drop(conn);

    // Verify password on the blocking pool: bcrypt (cost 12) is pure CPU and
    // must not run on an async worker.
    let pw = body.password.clone();
    let verified =
        tokio::task::spawn_blocking(move || security::password::verify_password(&pw, &stored_hash))
            .await;

    match verified {
        Ok(Ok(true)) => {
            reset_failures(&body.username);
        }
        Ok(Ok(false)) => {
            record_failure(&body.username);
            observability::increment_auth_failures("bad_password");
            return ApiError::unauthorized("invalid credentials").into_response();
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "login_handler: password verification failed");
            return ApiError::internal("internal error").into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "login_handler: verify task panicked");
            return ApiError::internal("internal error").into_response();
        }
    }

    // Create session
    let conn = auth_db.lock().await;
    let token = match security::auth::create_session(&conn, &body.username) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "login_handler: failed to create session");
            return ApiError::internal("internal error").into_response();
        }
    };

    drop(conn);

    tracing::info!(username = %body.username, "User logged in");

    // Set HttpOnly + Secure + SameSite session cookie
    let cookie = session_cookie(&token, secure);

    // Generate CSRF token for double-submit pattern.
    // The token is NOT HttpOnly so the frontend JS can read it and include
    // it as X-CSRF-Token header on state-changing requests.
    let csrf_token = security::auth::generate_token();
    let csrf_cookie = format!("csrf-token={csrf_token}; SameSite=Strict; Path=/; Max-Age=86400");

    // Build response manually — Axum's tuple header syntax uses `insert` (overwrite),
    // so two `set-cookie` headers must be appended via `headers_mut().append()`.
    let mut response = (
        StatusCode::OK,
        Json(serde_json::json!({"status": "ok", "csrf_token": csrf_token})),
    )
        .into_response();
    let headers = response.headers_mut();
    let session_header: axum::http::HeaderValue = match cookie.parse() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "failed to serialize session cookie");
            return ApiError::internal("internal error").into_response();
        }
    };
    headers.append(axum::http::header::SET_COOKIE, session_header);
    let csrf_header: axum::http::HeaderValue = match csrf_cookie.parse() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "failed to serialize csrf cookie");
            return ApiError::internal("internal error").into_response();
        }
    };
    headers.append(axum::http::header::SET_COOKIE, csrf_header);
    response
}

/// POST /api/auth/logout — invalidate the current session.
///
/// Reads the session cookie, deletes the session from the database,
/// and clears the cookie by setting Max-Age=0.
#[tracing::instrument(skip_all)]
pub async fn logout_handler(
    insecure: Option<Extension<ListenerScheme>>,
    Extension(_db): Extension<SqlitePool>,
    Extension(auth_db): Extension<Arc<Mutex<Connection>>>,
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
        let conn = auth_db.lock().await;
        if let Err(e) = security::auth::invalidate_session(&conn, &token) {
            tracing::warn!(error = %e, "logout_handler: failed to invalidate session");
        }
    }

    // Clear the cookie regardless of whether we found a session.
    // SPEC §2: logout responds 204 with only the clearing Set-Cookie.
    let cookie = session_cookie_cleared(insecure.map(|Extension(s)| s.0).unwrap_or(true));

    (StatusCode::NO_CONTENT, [("set-cookie", cookie.to_owned())]).into_response()
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
#[tracing::instrument(skip_all, fields(username = %body.username))]
pub async fn setup_handler(
    Extension(_db): Extension<SqlitePool>,
    Extension(auth_db): Extension<Arc<Mutex<Connection>>>,
    setup_insecure: Option<Extension<ListenerScheme>>,
    Json(body): Json<SetupRequest>,
) -> impl IntoResponse {
    // Validate
    if body.username.trim().is_empty() {
        return ApiError::bad_request("username cannot be empty").into_response();
    }
    if body.password.len() < 8 {
        return ApiError::bad_request("password must be at least 8 characters").into_response();
    }

    let conn = auth_db.lock().await;

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

    // Sign the new admin in immediately (SPEC v1 §2: setup establishes a
    // session). The connection is still held here.
    let token = match security::auth::create_session(&conn, &body.username) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "setup_handler: failed to create session");
            return ApiError::internal("internal error").into_response();
        }
    };

    drop(conn);

    // Generate self-signed TLS cert if not present (best-effort)
    if let Err(e) = security::tls::build_tls_config("tls/cert.pem", "tls/key.pem") {
        tracing::warn!(error = %e, "setup_handler: failed to generate TLS cert");
    }

    tracing::info!(username = %body.username, "Initial admin user created (first-run setup)");

    let csrf_token = security::auth::generate_token();
    let mut response = (
        StatusCode::OK,
        Json(serde_json::json!({"status": "ok", "csrf_token": csrf_token})),
    )
        .into_response();
    let headers = response.headers_mut();
    for cookie in [
        session_cookie(
            &token,
            setup_insecure.map(|Extension(s)| s.0).unwrap_or(true),
        ),
        format!("csrf-token={csrf_token}; SameSite=Strict; Path=/; Max-Age=86400"),
    ] {
        match cookie.parse::<axum::http::HeaderValue>() {
            Ok(v) => {
                headers.append(axum::http::header::SET_COOKIE, v);
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to serialize setup cookie");
                return ApiError::internal("internal error").into_response();
            }
        }
    }
    response
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
#[tracing::instrument(skip_all)]
pub async fn reset_password_handler(
    Extension(_db): Extension<SqlitePool>,
    Extension(auth_db): Extension<Arc<Mutex<Connection>>>,
    Extension(user): Extension<security::middleware::AuthenticatedUser>,
    Json(body): Json<ResetPasswordRequest>,
) -> impl IntoResponse {
    let conn = auth_db.lock().await;
    match security::auth::reset_password(&conn, &user.0, &body.old_password, &body.new_password) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response(),
        Err(security::auth::AuthError::WrongPassword) => {
            ApiError::unauthorized("Old password is incorrect").into_response()
        }
        Err(security::auth::AuthError::UserNotFound(u)) => {
            ApiError::not_found(format!("User '{}' not found", u)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "password reset failed");
            ApiError::internal("internal error").into_response()
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
        let resp = not_implemented_handler().await.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn test_login_rejects_wrong_credentials() {
        let (pool, auth_db) = crate::server::test_db_with_user().await;
        let app = crate::server::build_app(pool, auth_db);

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
        let (pool, auth_db) = crate::db::create_test_dbs().await;
        let app = crate::server::build_app(pool, auth_db);

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
        let _ = observability::register_metrics();
        let app = crate::server::test_app_with_user().await;

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
        let (pool, auth_db) = crate::server::test_db_with_user().await;
        let app = crate::server::build_app(pool, auth_db);
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
        let (pool, auth_db) = crate::db::create_test_dbs().await;
        let app = crate::server::build_app(pool, auth_db.clone());

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
        let conn = auth_db.lock().await;
        assert!(!security::auth::is_first_run(&conn).unwrap());
    }

    #[tokio::test]
    async fn test_setup_rejects_after_configured() {
        let (pool, auth_db) = crate::db::create_test_dbs().await;
        // Seed a user
        {
            let conn = auth_db.lock().await;
            security::auth::init_users_table(&conn).unwrap();
            let hash = security::password::hash_password("existing_pass").unwrap();
            conn.execute(
                "INSERT INTO users (username, password_hash) VALUES (?1, ?2)",
                rusqlite::params!["admin", hash],
            )
            .unwrap();
        }

        let app = crate::server::build_app(pool, auth_db);

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
        let (pool, auth_db) = crate::db::create_test_dbs().await;
        let app = crate::server::build_app(pool, auth_db.clone());

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
        let conn = auth_db.lock().await;
        assert!(security::auth::is_first_run(&conn).unwrap());
    }

    #[tokio::test]
    async fn test_setup_rejects_empty_username() {
        let (pool, auth_db) = crate::db::create_test_dbs().await;
        let app = crate::server::build_app(pool, auth_db);

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
        let (pool, auth_db) = crate::db::create_test_dbs().await;
        let app = crate::server::build_app(pool, auth_db);

        let req = Request::builder()
            .uri("/api/cameras")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_setup_allows_health_before_setup() {
        let (pool, auth_db) = crate::db::create_test_dbs().await;
        let app = crate::server::build_app(pool, auth_db);

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_setup_allows_setup_endpoint_before_setup() {
        let (pool, auth_db) = crate::db::create_test_dbs().await;
        let app = crate::server::build_app(pool, auth_db);

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

    /// Helper to set up a test app with an in-memory DB that has a seeded user.
    async fn test_app_with_user() -> (sqlx::SqlitePool, Arc<Mutex<Connection>>, String) {
        let (pool, auth_db) = crate::db::create_test_dbs().await;
        let token = {
            let conn = auth_db.lock().await;
            security::auth::init_users_table(&conn).unwrap();
            let hash = security::password::hash_password("current_pass").unwrap();
            conn.execute(
                "INSERT INTO users (username, password_hash) VALUES (?1, ?2)",
                rusqlite::params!["admin", hash],
            )
            .unwrap();
            security::auth::create_session(&conn, "admin").unwrap()
        };
        (pool, auth_db, token)
    }

    #[tokio::test]
    async fn test_login_success() {
        let (pool, auth_db, _token) = test_app_with_user().await;
        let app = crate::server::build_app(pool, auth_db);

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
    async fn test_login_insecure_listener_cookie_has_no_secure_flag() {
        // SPEC appendix A: session cookies issued over the optional plain
        // HTTP listener must omit `Secure` (browsers refuse Secure cookies
        // over http://, which would make login unusable there).
        let (pool, auth_db, _token) = test_app_with_user().await;
        let app = crate::server::build_app(pool, auth_db)
            .layer(axum::middleware::from_fn(mark_insecure_listener));

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

        let set_cookie = res
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .unwrap();
        assert!(
            set_cookie.starts_with("session="),
            "should set session cookie"
        );
        assert!(
            !set_cookie.contains("Secure"),
            "cookie issued on the insecure listener must not be Secure: {set_cookie}"
        );
        assert!(set_cookie.contains("HttpOnly"));
    }

    #[tokio::test]
    async fn test_logout_insecure_listener_clears_without_secure_flag() {
        let (pool, auth_db, _token) = test_app_with_user().await;
        let app = crate::server::build_app(pool, auth_db)
            .layer(axum::middleware::from_fn(mark_insecure_listener));

        // log in first to obtain a session cookie
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
        let res = app.clone().oneshot(req).await.unwrap();
        let cookie = res
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();

        let req = Request::builder()
            .uri("/api/auth/logout")
            .method("POST")
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let set_cookie = res
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .unwrap();
        assert!(!set_cookie.contains("Secure"));
    }

    #[tokio::test]
    async fn test_login_nonexistent_user() {
        let (pool, auth_db, _token) = test_app_with_user().await;
        let app = crate::server::build_app(pool, auth_db);

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
        let (pool, auth_db, _token) = test_app_with_user().await;
        let app = crate::server::build_app(pool, auth_db);

        let req = Request::builder()
            .uri("/api/auth/logout")
            .method("POST")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        // SPEC §2: logout responds 204 No Content with a clearing cookie.
        assert_eq!(res.status(), StatusCode::NO_CONTENT);

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

    #[tokio::test]
    async fn test_reset_password_requires_auth() {
        let (pool, auth_db, _token) = test_app_with_user().await;
        let app = crate::server::build_app(pool, auth_db);

        let req = Request::builder()
            .uri("/api/auth/reset")
            .method("POST")
            .header("content-type", "application/json")
            .header("cookie", "csrf-token=test-csrf")
            .header("x-csrf-token", "test-csrf")
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
        let (pool, auth_db, token) = test_app_with_user().await;
        let app = crate::server::build_app(pool, auth_db);

        let req = Request::builder()
            .uri("/api/auth/reset")
            .method("POST")
            .header("content-type", "application/json")
            .header("cookie", format!("session={token}; csrf-token=test-csrf"))
            .header("x-csrf-token", "test-csrf")
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
        let (pool, auth_db, token) = test_app_with_user().await;
        let app = crate::server::build_app(pool, auth_db);

        let req = Request::builder()
            .uri("/api/auth/reset")
            .method("POST")
            .header("content-type", "application/json")
            .header("cookie", format!("session={token}; csrf-token=test-csrf"))
            .header("x-csrf-token", "test-csrf")
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
