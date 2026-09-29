//! `/api/voice/speakers` — voiceprint speaker profiles (SPEC appendix A
//! notebook dialect #25): list, enroll (wake-word samples collected by the
//! engine), commit a completed enrollment, cancel an in-flight one, and
//! delete a profile.
//!
//! Enrollment is a poll-driven flow: `POST` arms the engine for the next
//! `utterances` wake words (each becomes one embedding sample), the client
//! polls `GET` for progress, and `POST /commit` persists the completed
//! profile. All state mutations are explicit — `GET` has no side effects.

use std::sync::Arc;

use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;
use streaming::voice::VoiceEngine;

use crate::db;
use crate::errors::ApiError;
use security::middleware::AuthenticatedUser;

#[derive(Debug, Deserialize)]
pub struct EnrollRequest {
    pub name: String,
    pub utterances: Option<u32>,
}

#[tracing::instrument(skip_all)]
pub async fn list_speakers(
    Extension(pool): Extension<SqlitePool>,
    Extension(voice): Extension<Arc<VoiceEngine>>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> Result<impl IntoResponse, ApiError> {
    let speakers = db::list_voice_speakers(&pool)
        .await
        .map_err(|e| ApiError::internal(format!("voice speakers: {e}")))?;
    Ok((
        StatusCode::OK,
        axum::Json(json!({
            "speakers": speakers,
            "enrollment": voice.enrollment_status(),
            "capable": voice.speaker_capable(),
        })),
    ))
}

#[tracing::instrument(skip_all)]
pub async fn enroll(
    Extension(pool): Extension<SqlitePool>,
    Extension(voice): Extension<Arc<VoiceEngine>>,
    Extension(_user): Extension<AuthenticatedUser>,
    axum::Json(body): axum::Json<EnrollRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if !voice.is_active() {
        return Err(ApiError::not_implemented(format!(
            "voice inactive: {}",
            voice.inactive_reason()
        )));
    }
    // Refuse a name that is already enrolled — re-enrolling goes through
    // DELETE first (keeps the DB row and the in-memory profile in step).
    let existing = db::list_voice_speakers(&pool)
        .await
        .map_err(|e| ApiError::internal(format!("voice speakers: {e}")))?;
    if existing.iter().any(|s| s.name == body.name) {
        return Err(ApiError::bad_request(format!(
            "speaker {:?} already enrolled — delete it first",
            body.name
        )));
    }
    let needed = body.utterances.unwrap_or(3);
    voice
        .begin_enrollment(&body.name, needed)
        .map_err(ApiError::bad_request)?;
    Ok((
        StatusCode::OK,
        axum::Json(json!({
            "started": voice.enrollment_status(),
            "hint": "say the wake word 'needed' more times at the device",
        })),
    ))
}

#[tracing::instrument(skip_all)]
pub async fn commit_enrollment(
    Extension(pool): Extension<SqlitePool>,
    Extension(voice): Extension<Arc<VoiceEngine>>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> Result<impl IntoResponse, ApiError> {
    let Some((name, embeddings)) = voice.take_completed_enrollment() else {
        return Err(ApiError::bad_request(
            "no completed enrollment session (collect all samples first)",
        ));
    };
    let dim = voice.speaker_dim();
    db::insert_voice_speaker(&pool, &name, i64::from(dim), &embeddings)
        .await
        .map_err(|e| ApiError::internal(format!("voice speakers: {e}")))?;
    tracing::info!(speaker = %name, samples = embeddings.len(), dim, "voice: profile persisted");
    Ok((
        StatusCode::OK,
        axum::Json(json!({
            "enrolled": name,
            "samples": embeddings.len(),
            "dim": dim,
        })),
    ))
}

#[tracing::instrument(skip_all)]
pub async fn cancel_enrollment(
    Extension(voice): Extension<Arc<VoiceEngine>>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> impl IntoResponse {
    voice.cancel_enrollment();
    (
        StatusCode::OK,
        axum::Json(json!({ "applied": "immediate", "enrollment": voice.enrollment_status() })),
    )
}

#[tracing::instrument(skip_all)]
pub async fn delete_speaker(
    Extension(pool): Extension<SqlitePool>,
    Extension(voice): Extension<Arc<VoiceEngine>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let removed = db::delete_voice_speaker(&pool, &name)
        .await
        .map_err(|e| ApiError::internal(format!("voice speakers: {e}")))?;
    let in_memory = voice.remove_speaker(&name);
    if !removed && !in_memory {
        return Err(ApiError::not_found(format!("speaker {name:?} not found")));
    }
    Ok((
        StatusCode::OK,
        axum::Json(json!({ "applied": "immediate", "removed": name })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, post};
    use tower::ServiceExt;

    fn app_with(pool: sqlx::SqlitePool) -> Router {
        // Default-config engine: inactive → enrollment refused, listing
        // inert — exactly what a non-voice build or missing models report.
        let voice = Arc::new(VoiceEngine::from_config(
            &streaming::voice::VoiceConfig::default(),
        ));
        Router::new()
            .route("/api/voice/speakers", get(list_speakers).post(enroll))
            .route("/api/voice/speakers/commit", post(commit_enrollment))
            .route("/api/voice/speakers/cancel", post(cancel_enrollment))
            .route(
                "/api/voice/speakers/{name}",
                axum::routing::delete(delete_speaker),
            )
            .layer(Extension(pool))
            .layer(Extension(voice))
            .layer(Extension(AuthenticatedUser("tester".to_string())))
    }

    #[tokio::test]
    async fn inactive_engine_lists_empty_and_refuses_enrollment() {
        let (pool, _auth) = crate::db::create_test_dbs().await;
        crate::db::run_migrations(&pool).await.expect("migrations");
        let app = app_with(pool.clone());

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/voice/speakers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["speakers"], serde_json::json!([]));
        assert_eq!(json["enrollment"], serde_json::Value::Null);
        assert_eq!(json["capable"], false);

        // Enrollment on an inactive engine is a 501-family refusal.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/voice/speakers")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"name": "mickey", "utterances": 2}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 501);

        // Commit without a session: 400.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/voice/speakers/commit")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 400);

        // Delete of an unknown speaker: 404.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/voice/speakers/nobody")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 404);
    }

    #[tokio::test]
    async fn delete_removes_persisted_profile() {
        let (pool, _auth) = crate::db::create_test_dbs().await;
        crate::db::run_migrations(&pool).await.expect("migrations");
        db::insert_voice_speaker(&pool, "mickey", 4, &[vec![0.5; 4]])
            .await
            .unwrap();
        let app = app_with(pool.clone());

        let res = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/voice/speakers/mickey")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        assert!(db::list_voice_speakers(&pool).await.unwrap().is_empty());
    }
}
