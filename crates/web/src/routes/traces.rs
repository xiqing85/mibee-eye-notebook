//! `GET /api/traces/conversations…` — conversation model-call chain
//! queries (SPEC v1 §3.3, capability `observability.traces`).

use axum::Json;
use axum::extract::{Path, Query};
use serde::Deserialize;
use serde_json::json;

use crate::convtrace::convtrace;
use crate::errors::ApiError;

#[derive(Debug, Deserialize)]
pub struct ListParams {
    /// Max conversations to return (SPEC §3.3: default 50, cap 200).
    pub limit: Option<usize>,
}

#[tracing::instrument(skip_all)]
pub async fn list_conversations(Query(params): Query<ListParams>) -> Json<serde_json::Value> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    Json(json!({ "conversations": convtrace().list(limit) }))
}

#[tracing::instrument(skip_all)]
pub async fn get_conversation(Path(id): Path<String>) -> Result<Json<serde_json::Value>, ApiError> {
    match convtrace().get(&id) {
        Some(detail) => Ok(Json(json!(detail))),
        None => Err(ApiError::not_found(format!(
            "unknown conversation trace: {id}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn traces_endpoints_serve_hub_state() {
        convtrace().clear();
        let conv = convtrace().start("chat");
        conv.span("vlm", "qwen3-vl-2b", "看图直答", vec![])
            .finish_ok();
        conv.span("llm", "qwen3-4b", "本地应答", vec![])
            .tokens(Some(64), Some(32))
            .finish_ok();
        conv.close();

        let app = axum::Router::new()
            .route(
                "/api/traces/conversations",
                axum::routing::get(list_conversations),
            )
            .route(
                "/api/traces/conversations/{id}",
                axum::routing::get(get_conversation),
            );

        // List: bare data (envelope wraps in the real server).
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/traces/conversations")
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
        assert!(json.get("ok").is_none(), "handler returns bare data");
        let list = &json["conversations"];
        assert_eq!(list.as_array().map(Vec::len), Some(1));
        assert_eq!(list[0]["origin"], "chat");
        assert_eq!(list[0]["status"], "ok");
        assert_eq!(list[0]["models"], serde_json::json!(["llm", "vlm"]));

        // Detail: spans sorted by start, tokens round-trip.
        let id = list[0]["id"].as_str().unwrap().to_string();
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/traces/conversations/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let detail: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(detail["spans"].as_array().map(Vec::len), Some(2));
        assert_eq!(detail["spans"][0]["model"], "vlm");
        assert_eq!(detail["spans"][1]["tokens_prompt"], serde_json::json!(64));

        // Unknown id → 404 error envelope shape via ApiError.
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/traces/conversations/c-nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        convtrace().clear();
    }
}
