//! `GET/DELETE /api/audio/records` — hearing records (SPEC appendix A
//! notebook dialect #24): the persistent text record of what the audio
//! engines recognized (sound-event classes and voice transcripts).
//! Reads are newest-first; DELETE clears everything. The records themselves
//! are written by the audio bridges in `main.rs` (fail-open).

use axum::extract::{Extension, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;

use crate::db;
use crate::errors::ApiError;
use security::middleware::AuthenticatedUser;
use sqlx::SqlitePool;

#[derive(Debug, Deserialize)]
pub struct RecordsQuery {
    limit: Option<i64>,
    kind: Option<String>,
}

#[tracing::instrument(skip_all)]
pub async fn list_records(
    Extension(pool): Extension<SqlitePool>,
    Extension(_user): Extension<AuthenticatedUser>,
    Query(q): Query<RecordsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let limit = q.limit.unwrap_or(100);
    let kind = q.kind.as_deref();
    let records = db::list_hearing_records(&pool, limit, kind)
        .await
        .map_err(|e| ApiError::internal(format!("hearing records: {e}")))?;
    Ok((
        StatusCode::OK,
        axum::Json(json!({ "records": records, "applied": "immediate" })),
    ))
}

#[tracing::instrument(skip_all)]
pub async fn clear_records(
    Extension(pool): Extension<SqlitePool>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> Result<impl IntoResponse, ApiError> {
    let removed = db::clear_hearing_records(&pool)
        .await
        .map_err(|e| ApiError::internal(format!("hearing records: {e}")))?;
    Ok((
        StatusCode::OK,
        axum::Json(json!({ "applied": "immediate", "removed": removed })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use tower::ServiceExt;

    async fn app_with(pool: sqlx::SqlitePool) -> Router {
        Router::new()
            .route(
                "/api/audio/records",
                get(list_records).delete(clear_records),
            )
            .layer(Extension(pool))
            .layer(Extension(AuthenticatedUser("tester".to_string())))
    }

    #[tokio::test]
    async fn records_roundtrip_filter_and_clear() {
        let (pool, _auth) = crate::db::create_test_dbs().await;
        crate::db::run_migrations(&pool).await.expect("migrations");
        db::insert_hearing_record(&pool, "sound", "Dog", Some(0.7), "", "", "", "", 1_000)
            .await
            .unwrap();
        db::insert_hearing_record(
            &pool,
            "voice",
            "开灯",
            None,
            "小蜜蜂",
            "mickey",
            "",
            "",
            2_000,
        )
        .await
        .unwrap();
        let app = app_with(pool.clone()).await;

        // Default list: newest first, both kinds, full record shape.
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/audio/records")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["records"].as_array().unwrap().len(), 2);
        assert_eq!(json["records"][0]["kind"], "voice");
        assert_eq!(json["records"][0]["text"], "开灯");
        assert_eq!(json["records"][0]["keyword"], "小蜜蜂");
        assert_eq!(json["records"][1]["score"], 0.7);
        assert_eq!(json["applied"], "immediate");

        // Kind filter.
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/audio/records?kind=sound")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let arr = json["records"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["kind"], "sound");

        // Clear.
        let res = app
            .clone()
            .oneshot(
                Request::delete("/api/audio/records")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["removed"], 2);

        // Empty afterwards.
        let res = app
            .oneshot(
                Request::get("/api/audio/records")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["records"].as_array().unwrap().len(), 0);
    }
}
