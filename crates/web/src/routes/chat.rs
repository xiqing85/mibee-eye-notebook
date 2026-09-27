//! `POST /api/chat` — one local-LLM dialogue turn (SPEC appendix A
//! notebook dialect). Body: `{"text": "...", "history": [{"role","content"}]}`;
//! response: `{"reply": "..."}`. Fail-open: 501-family error when the
//! engine is inactive.

use std::sync::Arc;

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;
use streaming::llm::{ChatEngine, ChatTurn};

use crate::errors::ApiError;
use security::middleware::AuthenticatedUser;

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub text: String,
    #[serde(default)]
    pub history: Vec<ChatTurn>,
}

#[tracing::instrument(skip_all)]
pub async fn chat(
    Extension(engine): Extension<Arc<ChatEngine>>,
    Extension(_user): Extension<AuthenticatedUser>,
    body: axum::extract::Json<ChatRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if body.text.trim().is_empty() {
        return Err(ApiError::bad_request("text must not be empty"));
    }
    if !engine.is_active() {
        return Err(ApiError::not_implemented(format!(
            "llm inactive: {}",
            engine.inactive_reason()
        )));
    }
    let mut turns: Vec<ChatTurn> = body.history.clone().into_iter().take(8).collect();
    turns.push(ChatTurn {
        role: "user".into(),
        content: body.text.clone(),
    });
    let engine = Arc::clone(&engine);
    let reply = tokio::task::spawn_blocking(move || engine.complete(&turns))
        .await
        .map_err(|e| ApiError::internal(format!("llm task: {e}")))?
        .map_err(|e| ApiError::internal(format!("llm: {e}")))?;
    Ok((
        StatusCode::OK,
        axum::Json(json!({ "reply": reply, "applied": "immediate" })),
    ))
}
