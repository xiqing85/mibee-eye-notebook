//! `POST /api/chat` — one local-LLM dialogue turn (SPEC appendix A
//! notebook dialect #22 + #29). Body: `{"text": "...", "history":
//! [{"role","content"}], "vision": bool?}`; response: `{"reply": "...",
//! "grounded": "vlm"|"scene"|"none"}`. Fail-open: 501-family error when
//! the engine is inactive.
//!
//! Grounding (#29): every turn is prepended with a system turn that
//! carries the persona, a language-following instruction (the user may
//! speak Mandarin, Cantonese or English) and — when the grounding state
//! knows something fresh about the camera — a 【画面】 context block
//! built from live detection labels and the last VLM alarm description.
//! `vision: true` additionally routes the question itself through the
//! VLM against a fresh frame (slow, CPU); any failure falls back to
//! the grounded LLM path with an honest `grounded` value.

use std::sync::Arc;

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;
use streaming::llm::{ChatEngine, ChatTurn};
use streaming::vlm::VlmEngine;

use crate::db;
use crate::errors::ApiError;
use crate::grounding::GroundingState;
use crate::stream_manager::StreamManager;
use security::middleware::AuthenticatedUser;

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub text: String,
    #[serde(default)]
    pub history: Vec<ChatTurn>,
    /// Explicit VLM Q&A on a fresh frame (SPEC #29; slow on CPU).
    #[serde(default)]
    pub vision: bool,
}

/// Distinctive Cantonese characters (rare in written Mandarin) — enough
/// signal to nudge a small model explicitly.
const CANTONESE_HINT_CHARS: &[char] = &[
    '咁', '嘅', '唔', '係', '喺', '嗰', '啲', '乜', '嘢', '佢', '嚟', '噉', '咧', '嚿', '掂', '冇',
];

/// Cheap spoken-language detection for the reply-language nudge. A 0.6 B
/// model does not reliably follow a generic "reply in the user's
/// language" instruction — an explicit per-turn hint does much better.
fn language_hint(user_text: &str) -> Option<&'static str> {
    let canto = user_text.chars().any(|c| CANTONESE_HINT_CHARS.contains(&c));
    if canto {
        return Some("用户使用粤语——请用粤语（广东话）回答。");
    }
    // Mostly ASCII letters and spaces → English.
    let mut letters = 0usize;
    let mut cjk = 0usize;
    for c in user_text.chars() {
        if c.is_ascii_alphabetic() {
            letters += 1;
        } else if ('\u{4e00}'..='\u{9fff}').contains(&c) {
            cjk += 1;
        }
    }
    if letters >= 2 && cjk == 0 {
        return Some("The user speaks English — reply in English.");
    }
    None
}

/// The always-on grounding system turn. `scene` is the 【画面】 block
/// from [`GroundingState::scene_summary`] (None → omitted, the reply
/// is then `grounded: "none"`). `user_text` drives the reply-language
/// hint (Cantonese/English are nudged explicitly — see
/// [`language_hint`]; Mandarin needs no hint).
pub fn build_system_turn(scene: Option<&str>, user_text: &str) -> ChatTurn {
    let mut content = String::from(
        "你是一台家庭安防摄像头上的语音助手。用用户所用的语言回复（普通话、粤语或英语），\
         回答简洁。\n",
    );
    if let Some(hint) = language_hint(user_text) {
        content.push_str(hint);
        content.push('\n');
    }
    if let Some(scene) = scene {
        content.push_str("【画面】");
        content.push_str(scene);
        content.push('\n');
    }
    ChatTurn {
        role: "system".into(),
        content,
    }
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Resolve the camera whose grounding/feed a device-level chat turn
/// should use: the first registered camera (single-camera deployments
/// are the norm; deterministic for multi-camera ones).
async fn primary_camera_id(pool: &SqlitePool) -> Option<String> {
    db::list_cameras(pool)
        .await
        .ok()
        .and_then(|cams| cams.first().map(|c| c.id.clone()))
}

#[tracing::instrument(skip_all)]
pub async fn chat(
    Extension(engine): Extension<Arc<ChatEngine>>,
    Extension(vlm): Extension<Arc<VlmEngine>>,
    Extension(streams): Extension<Arc<StreamManager>>,
    Extension(grounding): Extension<Arc<GroundingState>>,
    Extension(pool): Extension<SqlitePool>,
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
    let camera_id = primary_camera_id(&pool).await;
    let scene = camera_id
        .as_deref()
        .and_then(|id| grounding.scene_summary(id, unix_now_ms()));

    // Explicit VLM Q&A (#29): the question itself goes to the vision
    // model with a fresh frame. Any failure falls through to the
    // grounded LLM path — never a hard error for an optional mode.
    if body.vision
        && vlm.is_active()
        && let Some(id) = camera_id.as_deref()
        && let Some(jpeg) = streams.latest_jpeg(id).await
    {
        let vlm = Arc::clone(&vlm);
        let question = body.text.clone();
        let answered = tokio::task::spawn_blocking(move || vlm.answer_jpeg(&jpeg, &question))
            .await
            .map_err(|e| ApiError::internal(format!("vlm task: {e}")))?;
        match answered {
            Ok(reply) => {
                return Ok((
                    StatusCode::OK,
                    axum::Json(json!({
                        "reply": reply,
                        "grounded": "vlm",
                        "applied": "immediate",
                    })),
                ));
            }
            Err(e) => {
                tracing::warn!(error = %e, "chat: vlm Q&A failed — falling back to llm");
            }
        }
    }

    let mut turns: Vec<ChatTurn> = vec![build_system_turn(scene.as_deref(), &body.text)];
    turns.extend(body.history.iter().take(8).cloned());
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
        axum::Json(json!({
            "reply": reply,
            "grounded": if scene.is_some() { "scene" } else { "none" },
            "applied": "immediate",
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_turn_carries_language_and_scene() {
        let t = build_system_turn(Some("实时检测：2×person"), "你看到几个人");
        assert_eq!(t.role, "system");
        assert!(t.content.contains("粤语"), "{}", t.content);
        assert!(
            t.content.contains("【画面】实时检测：2×person"),
            "{}",
            t.content
        );
        // Mandarin text gets no explicit nudge beyond the generic line.
        assert!(!t.content.contains("用户使用粤语"), "{}", t.content);
    }

    #[test]
    fn system_turn_without_scene_omits_block() {
        let t = build_system_turn(None, "你好");
        assert!(!t.content.contains("【画面】"), "{}", t.content);
    }

    #[test]
    fn cantonese_text_gets_explicit_nudge() {
        // Only *distinctive* written Cantonese is detectable — sentences
        // written entirely in shared characters are genuinely ambiguous
        // and fall back to the generic language instruction.
        let t = build_system_turn(None, "我唔知道啊");
        assert!(t.content.contains("用户使用粤语"), "{}", t.content);
        let t = build_system_turn(None, "佢哋喺边度？");
        assert!(t.content.contains("用户使用粤语"), "{}", t.content);
    }

    #[test]
    fn english_text_gets_explicit_nudge() {
        let t = build_system_turn(None, "how many people do you see?");
        assert!(t.content.contains("reply in English"), "{}", t.content);
        // Mixed CJK text never triggers the English hint.
        let t = build_system_turn(None, "这个 hello 什么意思");
        assert!(!t.content.contains("reply in English"), "{}", t.content);
    }
}
