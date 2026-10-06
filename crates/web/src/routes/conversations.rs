//! `GET /api/conversations` — the dialogue turn record list (SPEC v1
//! §3.4, capability `conversations`). Reads straight from the SQLite
//! store; newest first.

use axum::Json;
use axum::extract::{Extension, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;

use crate::db;
use crate::errors::ApiError;
use security::middleware::AuthenticatedUser;

#[derive(Debug, Deserialize)]
pub struct ListParams {
    /// Max turns to return (SPEC §3.4: default 50, cap 200).
    pub limit: Option<i64>,
}

/// `DELETE /api/conversations` — clear every turn (SPEC §3.4 clear-all,
/// `hearing_records` precedent).
#[tracing::instrument(skip_all)]
pub async fn clear_conversations(
    Extension(pool): Extension<SqlitePool>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> Result<impl IntoResponse, ApiError> {
    let removed = db::clear_conversation_turns(&pool)
        .await
        .map_err(|e| ApiError::internal(format!("conversations: {e}")))?;
    Ok((
        StatusCode::OK,
        Json(json!({ "applied": "immediate", "removed": removed })),
    ))
}

#[tracing::instrument(skip_all)]
pub async fn list_conversations(
    Extension(pool): Extension<SqlitePool>,
    Query(params): Query<ListParams>,
) -> Json<serde_json::Value> {
    let limit = params.limit.unwrap_or(50);
    let turns = db::list_conversation_turns(&pool, limit)
        .await
        .unwrap_or_default();
    Json(json!({ "conversations": turns }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::TurnDraft;
    use crate::conversations::TurnOrigin;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn conversations_endpoint_lists_newest_first() {
        let (pool, _auth) = crate::db::create_test_dbs().await;
        crate::db::run_migrations(&pool)
            .await
            .expect("migrations for test pool");
        for i in 0..3 {
            crate::db::insert_conversation_turn(
                &pool,
                TurnDraft::new(TurnOrigin::Voice, format!("c{i}"))
                    .user_text(format!("turn {i}"))
                    .think("llm", "qwen3", "本地应答", 10 + i),
            )
            .await
            .unwrap();
        }

        let app = axum::Router::new()
            .route(
                "/api/conversations",
                axum::routing::get(list_conversations).delete(clear_conversations),
            )
            .layer(Extension(pool))
            .layer(Extension(AuthenticatedUser("tester".to_string())));
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/conversations?limit=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let turns = json["conversations"].as_array().expect("turns array");
        assert_eq!(turns.len(), 2, "limit respected");
        assert_eq!(turns[0]["conversation_id"], "c2", "newest first");
        assert_eq!(turns[0]["thinking"].as_array().map(Vec::len), Some(1));
        assert_eq!(turns[0]["reply_text"], serde_json::Value::Null);

        // Clear-all removes every row (SPEC §3.4 DELETE).
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/conversations")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["applied"], serde_json::json!("immediate"));
        assert_eq!(json["removed"], serde_json::json!(3));
        // Re-list through the route: empty.
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/conversations")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["conversations"].as_array().map(Vec::len), Some(0));
    }
}
