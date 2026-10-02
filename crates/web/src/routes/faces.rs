//! `/api/faces` — face recognition enrollments (SPEC appendix A #33),
//! mirroring the voiceprint speakers flow (#25): POST arms an enrollment
//! (frames are collected automatically by the AI detection loop while
//! the subject faces the camera), GET polls progress with no side
//! effects, commit persists, cancel aborts, DELETE removes.

use std::sync::Arc;

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;
use streaming::face::FaceEngine;

use crate::db;
use crate::errors::ApiError;
use security::middleware::AuthenticatedUser;

#[derive(Debug, Deserialize)]
pub struct EnrollRequest {
    pub name: String,
}

#[tracing::instrument(skip_all)]
pub async fn list_faces(
    Extension(pool): Extension<SqlitePool>,
    Extension(face): Extension<Arc<FaceEngine>>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> Result<impl IntoResponse, ApiError> {
    let faces = db::list_faces(&pool)
        .await
        .map_err(|e| ApiError::internal(format!("faces: {e}")))?;
    let needed = face.enroll_frames();
    let enrollment = face.with_registry(|r| r.enrollment_status(needed));
    Ok((
        StatusCode::OK,
        axum::Json(json!({
            "faces": faces,
            "enrollment": enrollment.map(|(name, collected, needed)| {
                json!({"name": name, "collected": collected, "needed": needed})
            }),
            "capable": face.is_active(),
        })),
    ))
}

#[tracing::instrument(skip_all)]
pub async fn begin_enroll(
    Extension(pool): Extension<SqlitePool>,
    Extension(face): Extension<Arc<FaceEngine>>,
    Extension(_user): Extension<AuthenticatedUser>,
    axum::Json(body): axum::Json<EnrollRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if !face.is_active() {
        return Err(ApiError::not_implemented(format!(
            "face inactive: {}",
            face.inactive_reason()
        )));
    }
    let name = body.name.trim();
    let bytes = name.len();
    if bytes == 0 || bytes > 32 {
        return Err(ApiError::bad_request("name must be 1..=32 bytes"));
    }
    let exists = db::list_faces(&pool)
        .await
        .map_err(|e| ApiError::internal(format!("faces: {e}")))?
        .iter()
        .any(|f| f.name == name);
    if exists {
        return Err(ApiError::bad_request("name already enrolled"));
    }
    face.with_registry(|r| r.begin_enroll(name));
    Ok((StatusCode::OK, axum::Json(json!({"enrolling": name}))))
}

#[tracing::instrument(skip_all)]
pub async fn commit_enroll(
    Extension(pool): Extension<SqlitePool>,
    Extension(face): Extension<Arc<FaceEngine>>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> Result<impl IntoResponse, ApiError> {
    let needed = face.enroll_frames();
    let committed = face.with_registry(|r| r.commit_enroll(needed));
    match committed {
        Ok(f) => {
            db::insert_face(&pool, &f.name, f.embedding.len() as i64, &f.embedding)
                .await
                .map_err(|e| ApiError::internal(format!("faces: {e}")))?;
            Ok((
                StatusCode::OK,
                axum::Json(json!({"enrolled": f.name, "dim": f.embedding.len()})),
            ))
        }
        Err(msg) => Err(ApiError::bad_request(&msg)),
    }
}

#[tracing::instrument(skip_all)]
pub async fn cancel_enroll(
    Extension(face): Extension<Arc<FaceEngine>>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> Result<impl IntoResponse, ApiError> {
    face.with_registry(|r| r.cancel_enroll());
    Ok((StatusCode::OK, axum::Json(json!({"cancelled": true}))))
}

#[tracing::instrument(skip_all)]
pub async fn delete_face(
    Extension(pool): Extension<SqlitePool>,
    Extension(face): Extension<Arc<FaceEngine>>,
    Extension(_user): Extension<AuthenticatedUser>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let removed = db::delete_face(&pool, &name)
        .await
        .map_err(|e| ApiError::internal(format!("faces: {e}")))?;
    if !removed {
        return Err(ApiError::not_found("no such face"));
    }
    face.with_registry(|r| {
        r.remove(&name);
    });
    Ok((StatusCode::OK, axum::Json(json!({"deleted": name}))))
}

#[cfg(test)]
mod tests {
    // Route-level behavior is covered by the streaming face-registry unit
    // tests (commit/cancel/match semantics) and the db face-CRUD tests;
    // the handlers are thin wrappers over both.
}
