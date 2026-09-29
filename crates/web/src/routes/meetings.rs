//! `/api/meetings` — on-demand meeting mode (SPEC appendix A notebook
//! dialect #27): start/stop a recording session, list meetings and their
//! diarized segments, delete a meeting.
//!
//! `stop` returns as soon as the WAV is finalized (`status:"processing"`);
//! the pipeline (diarization → per-segment ASR → punctuation → voiceprint
//! voting → persistence) runs on a spawned task and reports through the
//! `meeting_state` SSE event. Auto-stopped sessions (max duration) ride
//! the same [`process_finished`] helper via the host bridge in main.

use std::sync::Arc;

use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::json;
use sqlx::SqlitePool;
use streaming::meeting::{MeetingEngine, RecordedMeeting};

use crate::db;
use crate::errors::ApiError;
use crate::routes::events::{CameraEvent, EventBus};
use security::middleware::AuthenticatedUser;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[tracing::instrument(skip_all)]
pub async fn start(
    Extension(pool): Extension<SqlitePool>,
    Extension(meeting): Extension<Arc<MeetingEngine>>,
    Extension(events): Extension<Arc<EventBus>>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> Result<impl IntoResponse, ApiError> {
    if !meeting.is_active() {
        return Err(ApiError::not_implemented(format!(
            "meeting inactive: {}",
            meeting.inactive_reason()
        )));
    }
    let started = now_ms();
    let id = db::insert_meeting_started(&pool, started as i64)
        .await
        .map_err(|e| ApiError::internal(format!("meeting: {e}")))?;
    if let Err(e) = meeting.begin_recording(id) {
        // The row exists but no session does — mark it failed immediately
        // so the list never shows a phantom recording.
        let _ = db::update_meeting_status(&pool, id, "failed", &e).await;
        if e == "already recording" {
            return Err(ApiError::conflict(format!(
                "a meeting is already recording (id {})",
                meeting.recording_id().unwrap_or_default()
            )));
        }
        return Err(ApiError::internal(format!("meeting: {e}")));
    }
    tracing::info!(meeting = id, "meeting: recording started");
    let _ = events.send(CameraEvent::MeetingState {
        camera_id: "all".into(),
        meeting_id: id,
        status: "recording".into(),
        timestamp_ms: now_ms(),
    });
    Ok((
        StatusCode::CREATED,
        axum::Json(json!({ "id": id, "started_at_ms": started })),
    ))
}

#[tracing::instrument(skip_all)]
pub async fn stop(
    Extension(pool): Extension<SqlitePool>,
    Extension(meeting): Extension<Arc<MeetingEngine>>,
    Extension(events): Extension<Arc<EventBus>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    if !meeting.is_active() {
        return Err(ApiError::not_implemented(format!(
            "meeting inactive: {}",
            meeting.inactive_reason()
        )));
    }
    let recorded = meeting.finish_recording(id).map_err(|e| {
        if e == "not recording" {
            ApiError::conflict("not recording")
        } else {
            ApiError::conflict(e)
        }
    })?;
    db::update_meeting_status(&pool, id, "processing", "")
        .await
        .map_err(|e| ApiError::internal(format!("meeting: {e}")))?;
    tracing::info!(meeting = id, "meeting: recording stopped, processing");
    // The pipeline runs detached; completion arrives via meeting_state SSE.
    tokio::spawn(process_finished(
        pool,
        Arc::clone(&meeting),
        events,
        recorded,
    ));
    Ok((
        StatusCode::OK,
        axum::Json(json!({ "id": id, "status": "processing" })),
    ))
}

#[tracing::instrument(skip_all)]
pub async fn list(
    Extension(pool): Extension<SqlitePool>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> Result<impl IntoResponse, ApiError> {
    let meetings = db::list_meetings(&pool)
        .await
        .map_err(|e| ApiError::internal(format!("meeting: {e}")))?;
    Ok((StatusCode::OK, axum::Json(json!({ "meetings": meetings }))))
}

#[tracing::instrument(skip_all)]
pub async fn get_meeting(
    Extension(pool): Extension<SqlitePool>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let Some(row) = db::get_meeting(&pool, id)
        .await
        .map_err(|e| ApiError::internal(format!("meeting: {e}")))?
    else {
        return Err(ApiError::not_found(format!("meeting {id} not found")));
    };
    let segments = db::list_meeting_segments(&pool, id)
        .await
        .map_err(|e| ApiError::internal(format!("meeting: {e}")))?;
    Ok((
        StatusCode::OK,
        axum::Json(json!({
            "meeting": row,
            "segments": segments,
        })),
    ))
}

#[tracing::instrument(skip_all)]
pub async fn delete_meeting(
    Extension(pool): Extension<SqlitePool>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    // A running session must be stopped first (its WAV is still open).
    let existed = db::delete_meeting(&pool, id)
        .await
        .map_err(|e| ApiError::internal(format!("meeting: {e}")))?;
    if !existed {
        return Err(ApiError::not_found(format!("meeting {id} not found")));
    }
    Ok((
        StatusCode::OK,
        axum::Json(json!({ "applied": "immediate", "deleted": id })),
    ))
}

/// Shared post-recording pipeline: process the WAV, persist the result (or
/// the failure), emit `meeting_state`, and clean the audio file per
/// `keep_audio`. Used by the `stop` route and by the auto-stop bridge in
/// main (which owns a cloned event bus for exactly this purpose).
pub async fn process_finished(
    pool: SqlitePool,
    meeting: Arc<MeetingEngine>,
    events: Arc<EventBus>,
    rec: RecordedMeeting,
) {
    let id = rec.id;
    let emit = |status: &str| {
        let _ = events.send(CameraEvent::MeetingState {
            camera_id: "all".into(),
            meeting_id: id,
            status: status.into(),
            timestamp_ms: now_ms(),
        });
    };
    let engine = Arc::clone(&meeting);
    let rec_for_task = rec.clone();
    let result = tokio::task::spawn_blocking(move || engine.process_meeting(&rec_for_task))
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("pipeline task panicked: {e}")));
    match result {
        Ok(transcript) => {
            let audio_path = meeting.cleanup_audio(&rec);
            let path_str = audio_path
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            if let Err(e) = db::finalize_meeting_done(
                &pool,
                id,
                rec.ended_at_ms as i64,
                transcript.duration_ms as i64,
                i64::from(transcript.num_speakers),
                &path_str,
                &transcript.segments,
            )
            .await
            {
                tracing::error!(error = %e, meeting = id, "meeting: persist failed");
                let _ = db::update_meeting_status(&pool, id, "failed", &format!("{e}")).await;
                emit("failed");
                return;
            }
            tracing::info!(
                meeting = id,
                speakers = transcript.num_speakers,
                segments = transcript.segments.len(),
                "meeting: minute ready"
            );
            emit("done");
        }
        Err(e) => {
            let _ = meeting.cleanup_audio(&rec);
            tracing::error!(
                error = format!("{e:#}"),
                meeting = id,
                "meeting: pipeline failed"
            );
            let _ = db::update_meeting_status(&pool, id, "failed", &format!("{e:#}")).await;
            emit("failed");
        }
    }
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
        let meeting = Arc::new(MeetingEngine::from_config(
            &streaming::meeting::MeetingConfig::default(),
            &streaming::voice::VoiceConfig::default(),
        ));
        let events = std::sync::Arc::new(crate::routes::events::new_event_bus());
        Router::new()
            .route("/api/meetings/start", post(start))
            .route("/api/meetings/{id}/stop", post(stop))
            .route("/api/meetings", get(list))
            .route(
                "/api/meetings/{id}",
                get(get_meeting).delete(delete_meeting),
            )
            .layer(Extension(pool))
            .layer(Extension(meeting))
            .layer(Extension(events))
            .layer(Extension(AuthenticatedUser("tester".to_string())))
    }

    #[tokio::test]
    async fn inactive_engine_refuses_start_but_lists_and_deletes() {
        let (pool, _auth) = crate::db::create_test_dbs().await;
        crate::db::run_migrations(&pool).await.expect("migrations");
        let app = app_with(pool.clone());

        // Inactive engine → 501 with the reason.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/meetings/start")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 501);

        // A stopped session on an idle engine: 409.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/meetings/1/stop")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 501);

        // Seed a done meeting directly and exercise list/get/delete.
        let id = db::insert_meeting_started(&pool, 1_000).await.unwrap();
        db::update_meeting_status(&pool, id, "processing", "")
            .await
            .unwrap();
        db::finalize_meeting_done(
            &pool,
            id,
            61_000,
            60_000,
            2,
            "",
            &[
                streaming::meeting::TranscriptSegment {
                    start_ms: 0,
                    end_ms: 5_000,
                    speaker_index: 0,
                    speaker: "mickey".into(),
                    text: "今天开会讨论发布。".into(),
                },
                streaming::meeting::TranscriptSegment {
                    start_ms: 6_000,
                    end_ms: 9_000,
                    speaker_index: 1,
                    speaker: String::new(),
                    text: "好的。".into(),
                },
            ],
        )
        .await
        .unwrap();

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/meetings")
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
        assert_eq!(json["meetings"].as_array().unwrap().len(), 1);
        assert_eq!(json["meetings"][0]["status"], "done");
        assert_eq!(json["meetings"][0]["num_speakers"], 2);

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/meetings/{id}"))
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
        let segs = json["segments"].as_array().unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0]["speaker"], "mickey");
        assert_eq!(segs[1]["speaker_index"], 1);

        // Unknown id: 404 on get and delete.
        for uri in ["/api/meetings/999", "/api/meetings/999"] {
            let res = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), 404);
        }

        // Delete removes row + segments.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/meetings/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        assert!(db::list_meetings(&pool).await.unwrap().is_empty());
        assert!(
            db::list_meeting_segments(&pool, id)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
