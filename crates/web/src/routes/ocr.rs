//! `POST /api/ocr` — recognize text in a JPEG (SPEC appendix A notebook
//! dialect). Body is the raw JPEG bytes.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::json;
use streaming::ocr::OcrEngine;

use crate::errors::ApiError;
use security::middleware::AuthenticatedUser;

#[tracing::instrument(skip_all)]
pub async fn run_ocr(
    Extension(ocr): Extension<Arc<OcrEngine>>,
    Extension(_user): Extension<AuthenticatedUser>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    if !ocr.is_active() {
        return Err(ApiError::not_implemented(format!(
            "ocr inactive: {}",
            ocr.inactive_reason()
        )));
    }
    if body.is_empty() {
        return Err(ApiError::bad_request("empty body — POST a JPEG image"));
    }
    let engine = Arc::clone(&ocr);
    let items = tokio::task::spawn_blocking(move || engine.recognize_jpeg(&body))
        .await
        .map_err(|e| ApiError::internal(format!("ocr task: {e}")))?
        .map_err(|e| ApiError::internal(format!("ocr: {e}")))?;
    Ok((StatusCode::OK, axum::Json(json!({ "items": items }))))
}
