//! Unified structured HTTP error responses.
//!
//! Provides [`ApiError`] — an error type that implements [`IntoResponse`]
//! and produces JSON responses in the format:
//!
//! ```json
//! {"error": "code", "message": "...", "status": N}
//! ```
//!
//! with the appropriate `Content-Type: application/json` header.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Machine-readable error codes mapped to HTTP status codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorKind {
    /// 400 Bad Request — validation error.
    BadRequest,
    /// 401 Unauthorized — authentication failed.
    Unauthorized,
    /// 404 Not Found — resource does not exist.
    NotFound,
    /// 409 Conflict — resource state conflict.
    Conflict,
    /// 429 Too Many Requests — resource limit reached.
    TooManyRequests,
    /// 501 Not Implemented — endpoint not yet implemented.
    NotImplemented,
    /// 504 Gateway Timeout — upstream request timed out.
    GatewayTimeout,
    /// 500 Internal Server Error — unexpected error.
    InternalServerError,
}

impl ApiErrorKind {
    /// Map this error kind to an HTTP status code.
    fn status_code(self) -> StatusCode {
        match self {
            Self::BadRequest => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict => StatusCode::CONFLICT,
            Self::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            Self::NotImplemented => StatusCode::NOT_IMPLEMENTED,
            Self::GatewayTimeout => StatusCode::GATEWAY_TIMEOUT,
            Self::InternalServerError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Return a short machine-readable error code string.
    fn error_code(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::Unauthorized => "unauthorized",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::TooManyRequests => "too_many_requests",
            Self::NotImplemented => "not_implemented",
            Self::GatewayTimeout => "gateway_timeout",
            Self::InternalServerError => "internal_error",
        }
    }
}

/// Unified structured HTTP error response.
///
/// Renders as JSON with `error` (machine code), `message` (human text),
/// and `status` (HTTP status number).
#[derive(Debug)]
pub struct ApiError {
    kind: ApiErrorKind,
    message: String,
}

impl ApiError {
    /// Create a new `ApiError` with the given kind and message.
    pub fn new(kind: ApiErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    // ------------------------------------------------------------------
    // Convenience constructors — one per supported status code.
    // ------------------------------------------------------------------

    /// 400 Bad Request — validation error.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::BadRequest, message)
    }

    /// 401 Unauthorized — authentication failed.
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::Unauthorized, message)
    }

    /// 404 Not Found — resource does not exist.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::NotFound, message)
    }

    /// 409 Conflict — resource state conflict (e.g. stream already running).
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::Conflict, message)
    }

    /// 429 Too Many Requests — resource limit or rate limit reached.
    pub fn too_many_requests(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::TooManyRequests, message)
    }

    /// 501 Not Implemented — endpoint not yet implemented.
    pub fn not_implemented(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::NotImplemented, message)
    }

    /// 504 Gateway Timeout — upstream request timed out.
    pub fn gateway_timeout(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::GatewayTimeout, message)
    }

    /// 500 Internal Server Error — unexpected error (catch-all).
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::InternalServerError, message)
    }

    /// Return the HTTP status code for this error.
    pub fn status_code(&self) -> StatusCode {
        self.kind.status_code()
    }

    /// Return the machine-readable error code string.
    pub fn error_code(&self) -> &'static str {
        self.kind.error_code()
    }

    /// Return the human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({
            "error": self.kind.error_code(),
            "message": self.message,
            "status": self.kind.status_code().as_u16(),
        });
        (self.kind.status_code(), Json(body)).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        tracing::error!(error = %err, "internal server error");
        Self::internal("internal server error")
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::bad_request(format!("invalid JSON: {}", e))
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        match e {
            rusqlite::Error::QueryReturnedNoRows => ApiError::not_found("resource not found"),
            _ => ApiError::internal(format!("database error: {}", e)),
        }
    }
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.error_code(), self.message)
    }
}

impl std::error::Error for ApiError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    /// Helper: extract JSON body from a response.
    async fn body_json(res: Response) -> serde_json::Value {
        let body = axum::body::to_bytes(res.into_body(), 1024 * 16)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn test_bad_request_format() {
        let err = ApiError::bad_request("invalid input");
        let res = err.into_response();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let json = body_json(res).await;
        assert_eq!(json["error"], "bad_request");
        assert_eq!(json["message"], "invalid input");
        assert_eq!(json["status"], 400);
    }

    #[tokio::test]
    async fn test_unauthorized_format() {
        let err = ApiError::unauthorized("invalid credentials");
        let res = err.into_response();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let json = body_json(res).await;
        assert_eq!(json["error"], "unauthorized");
        assert_eq!(json["message"], "invalid credentials");
        assert_eq!(json["status"], 401);
    }

    #[tokio::test]
    async fn test_not_found_format() {
        let err = ApiError::not_found("camera not found");
        let res = err.into_response();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let json = body_json(res).await;
        assert_eq!(json["error"], "not_found");
        assert_eq!(json["message"], "camera not found");
        assert_eq!(json["status"], 404);
    }

    #[tokio::test]
    async fn test_conflict_format() {
        let err = ApiError::conflict("stream already running");
        let res = err.into_response();
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let json = body_json(res).await;
        assert_eq!(json["error"], "conflict");
        assert_eq!(json["message"], "stream already running");
        assert_eq!(json["status"], 409);
    }

    #[tokio::test]
    async fn test_too_many_requests_format() {
        let err = ApiError::too_many_requests("resource limit reached");
        let res = err.into_response();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        let json = body_json(res).await;
        assert_eq!(json["error"], "too_many_requests");
        assert_eq!(json["message"], "resource limit reached");
        assert_eq!(json["status"], 429);
    }

    #[tokio::test]
    async fn test_not_implemented_format() {
        let err = ApiError::not_implemented("not implemented");
        let res = err.into_response();
        assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
        let json = body_json(res).await;
        assert_eq!(json["error"], "not_implemented");
        assert_eq!(json["message"], "not implemented");
        assert_eq!(json["status"], 501);
    }

    #[tokio::test]
    async fn test_gateway_timeout_format() {
        let err = ApiError::gateway_timeout("upstream timeout");
        let res = err.into_response();
        assert_eq!(res.status(), StatusCode::GATEWAY_TIMEOUT);
        let json = body_json(res).await;
        assert_eq!(json["error"], "gateway_timeout");
        assert_eq!(json["message"], "upstream timeout");
        assert_eq!(json["status"], 504);
    }

    #[tokio::test]
    async fn test_internal_error_format() {
        let err = ApiError::internal("something went wrong");
        let res = err.into_response();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = body_json(res).await;
        assert_eq!(json["error"], "internal_error");
        assert_eq!(json["message"], "something went wrong");
        assert_eq!(json["status"], 500);
    }

    #[tokio::test]
    async fn test_from_anyhow() {
        let underlying = anyhow::anyhow!("disk full");
        let err = ApiError::from(underlying);
        assert_eq!(err.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.error_code(), "internal_error");
        assert_eq!(err.message(), "internal server error");
    }

    #[test]
    fn test_display() {
        let err = ApiError::not_found("camera not found");
        assert_eq!(err.to_string(), "not_found: camera not found");
    }

    #[test]
    fn test_error_impl() {
        let err = ApiError::bad_request("nope");
        let err_ref: &dyn std::error::Error = &err;
        assert_eq!(err_ref.to_string(), "bad_request: nope");
    }

    #[test]
    fn test_kind_status_code_mapping() {
        assert_eq!(
            ApiErrorKind::BadRequest.status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ApiErrorKind::Unauthorized.status_code(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(ApiErrorKind::NotFound.status_code(), StatusCode::NOT_FOUND);
        assert_eq!(ApiErrorKind::Conflict.status_code(), StatusCode::CONFLICT);
        assert_eq!(
            ApiErrorKind::TooManyRequests.status_code(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            ApiErrorKind::NotImplemented.status_code(),
            StatusCode::NOT_IMPLEMENTED
        );
        assert_eq!(
            ApiErrorKind::GatewayTimeout.status_code(),
            StatusCode::GATEWAY_TIMEOUT
        );
        assert_eq!(
            ApiErrorKind::InternalServerError.status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn test_kind_error_code_strings() {
        assert_eq!(ApiErrorKind::BadRequest.error_code(), "bad_request");
        assert_eq!(ApiErrorKind::Unauthorized.error_code(), "unauthorized");
        assert_eq!(ApiErrorKind::NotFound.error_code(), "not_found");
        assert_eq!(ApiErrorKind::Conflict.error_code(), "conflict");
        assert_eq!(
            ApiErrorKind::TooManyRequests.error_code(),
            "too_many_requests"
        );
        assert_eq!(ApiErrorKind::NotImplemented.error_code(), "not_implemented");
        assert_eq!(ApiErrorKind::GatewayTimeout.error_code(), "gateway_timeout");
        assert_eq!(
            ApiErrorKind::InternalServerError.error_code(),
            "internal_error"
        );
    }

    #[tokio::test]
    async fn test_content_type_is_json() {
        let err = ApiError::not_found("test");
        let res = err.into_response();
        let content_type = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.contains("application/json"),
            "expected application/json, got {content_type}"
        );
    }
}
