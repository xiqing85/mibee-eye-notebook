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

/// The injected context blocks of one dialogue turn (#29 + #30-A).
/// All optional/fail-open: missing blocks are simply omitted.
#[derive(Debug, Default, Clone)]
pub struct TurnContext {
    /// 【画面】 from [`GroundingState::scene_summary`].
    pub scene: Option<String>,
    /// 【本机】 — local clock/system/camera facts (time questions must
    /// never be answered from model memory).
    pub local: Option<String>,
    /// 【联网】 — intent-gated lookup result (weather).
    pub web: Option<String>,
}

fn language_hint(user_text: &str) -> Option<&'static str> {
    match streaming::lang::detect(user_text) {
        streaming::lang::SpokenLang::Cantonese => Some(
            "用户使用粤语——请用地道的口语粤语（广东话）回答：用粤语惯用字（而家、嘅、唔、\
             係、咗、乜嘢、冇），不要用普通话书面语（避免写「现在」「的」「不」「什么」）。",
        ),
        streaming::lang::SpokenLang::English => Some("The user speaks English — reply in English."),
        streaming::lang::SpokenLang::Mandarin => None,
    }
}

/// The always-on grounding system turn assembled from a
/// [`TurnContext`]. `user_text` drives the reply-language hint
/// (Cantonese/English are nudged explicitly — a small model does not
/// follow a generic instruction; Mandarin needs no hint).
pub fn build_system_turn(ctx: &TurnContext, user_text: &str) -> ChatTurn {
    let mut content = String::from(
        "你是一台家庭安防摄像头上的语音助手。用用户所用的语言回复（普通话、粤语或英语），\
         回答简洁。涉及时间、天气、画面等问题时，只依据下面给出的【】资料回答；没有资料就\
         如实说不知道。\n",
    );
    if let Some(hint) = language_hint(user_text) {
        content.push_str(hint);
        content.push('\n');
    }
    for (tag, block) in [
        ("【画面】", &ctx.scene),
        ("【本机】", &ctx.local),
        ("【联网】", &ctx.web),
    ] {
        if let Some(block) = block {
            content.push_str(tag);
            content.push_str(block);
            content.push('\n');
        }
    }
    ChatTurn {
        role: "system".into(),
        content,
    }
}

/// Chinese weekday name for an ISO weekday number (1=Monday…7=Sunday).
#[must_use]
pub fn weekday_name(iso_u: usize) -> &'static str {
    const NAMES: [&str; 7] = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];
    NAMES[(iso_u.clamp(1, 7) - 1) % 7]
}

/// Render the 【本机】 block from gathered facts (pure — testable).
#[must_use]
pub fn format_local_block(
    now_local: &str,
    weekday: &str,
    uptime_human: &str,
    load1: f64,
    avail_mib: u64,
    cameras: &str,
) -> String {
    format!(
        "当前时间 {now_local} {weekday}；本机已运行 {uptime_human}；负载 {load1:.2}；\
         可用内存 {avail_mib} MiB；相机：{cameras}。回答时间/日期问题必须以此为准。"
    )
}

/// Gather and render the 【本机】 block (reads /proc + camera rows).
pub async fn local_block(pool: &SqlitePool) -> String {
    let now = chrono::Local::now();
    let uptime_human = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|t| {
            t.split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
        })
        .map(|secs| {
            let d = (secs / 86_400.0) as u64;
            let h = ((secs % 86_400.0) / 3600.0) as u64;
            let m = ((secs % 3600.0) / 60.0) as u64;
            if d > 0 {
                format!("{d}天{h}小时")
            } else {
                format!("{h}小时{m}分")
            }
        })
        .unwrap_or_else(|| "未知".into());
    let load1 = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|t| {
            t.split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
        })
        .unwrap_or(0.0);
    let avail_mib = streaming::tools::available_mem_mib().unwrap_or(0);
    let cameras = db::list_cameras(pool)
        .await
        .map(|cams| {
            if cams.is_empty() {
                "无".to_string()
            } else {
                cams.iter()
                    .map(|c| format!("{}({})", c.name, c.status))
                    .collect::<Vec<_>>()
                    .join("、")
            }
        })
        .unwrap_or_else(|_| "未知".into());
    let weekday_idx: usize = now
        .format("%u")
        .to_string()
        .parse()
        .unwrap_or(1)
        .clamp(1, 7);
    format_local_block(
        &now.format("%Y-%m-%d %H:%M:%S").to_string(),
        weekday_name(weekday_idx),
        &uptime_human,
        load1,
        avail_mib,
        &cameras,
    )
}

/// Weather lookup for the 【联网】 block (#30-A): intent-gated,
/// config-enabled, fail-open (errors → None and the model honestly
/// says it does not know).
pub async fn web_block(tools: &streaming::tools::ToolsConfig, user_text: &str) -> Option<String> {
    if !tools.weather_enabled || tools.weather_city.is_empty() {
        return None;
    }
    if !streaming::tools::weather_intent(user_text) {
        return None;
    }
    match streaming::tools::fetch_weather(&tools.weather_city, tools.timeout_secs).await {
        Ok(report) => Some(report),
        Err(e) => {
            tracing::warn!(error = %e, "tools: weather lookup failed (fail-open)");
            None
        }
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
#[allow(clippy::too_many_arguments)]
pub async fn chat(
    Extension(engine): Extension<Arc<ChatEngine>>,
    Extension(vlm): Extension<Arc<VlmEngine>>,
    Extension(streams): Extension<Arc<StreamManager>>,
    Extension(grounding): Extension<Arc<GroundingState>>,
    Extension(tools): Extension<crate::server::SharedTools>,
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
    let tools_now = tools.read().expect("tools config lock poisoned").clone();
    let ctx = TurnContext {
        scene: camera_id
            .as_deref()
            .and_then(|id| grounding.scene_summary(id, unix_now_ms())),
        local: Some(local_block(&pool).await),
        web: web_block(&tools_now, &body.text).await,
    };
    let scene = ctx.scene.clone();

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

    let mut turns: Vec<ChatTurn> = vec![build_system_turn(&ctx, &body.text)];
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

    fn ctx(scene: Option<&str>) -> TurnContext {
        TurnContext {
            scene: scene.map(String::from),
            local: Some("当前时间 2026-10-01 21:00:00 周四；本机已运行 3小时5分".into()),
            web: None,
        }
    }

    #[test]
    fn system_turn_carries_language_scene_and_local() {
        let t = build_system_turn(&ctx(Some("实时检测：2×person")), "你看到几个人");
        assert_eq!(t.role, "system");
        assert!(t.content.contains("粤语"), "{}", t.content);
        assert!(
            t.content.contains("【画面】实时检测：2×person"),
            "{}",
            t.content
        );
        assert!(
            t.content.contains("【本机】当前时间 2026-10-01"),
            "{}",
            t.content
        );
        assert!(!t.content.contains("用户使用粤语"), "{}", t.content);
    }

    #[test]
    fn system_turn_without_scene_omits_that_block_only() {
        let t = build_system_turn(&ctx(None), "你好");
        assert!(!t.content.contains("【画面】"), "{}", t.content);
        assert!(t.content.contains("【本机】"), "{}", t.content);
    }

    #[test]
    fn system_turn_cantonese_gets_vernacular_hint() {
        // 而家-only questions reach the Cantonese arm via the bigram and
        // must carry the vernacular-writing nudge — a small model turns
        // a bare "用粤语回答" into standard written Chinese.
        let t = build_system_turn(&ctx(None), "而家广州天气点呀");
        assert!(t.content.contains("口语粤语"), "{}", t.content);
        assert!(t.content.contains("而家、嘅"), "{}", t.content);
        assert!(t.content.contains("不要用普通话书面语"), "{}", t.content);
    }

    #[test]
    fn web_block_appears_when_fetched() {
        let mut c = ctx(None);
        c.web = Some("广州 当前天气：Partly cloudy，气温 26°C".into());
        let t = build_system_turn(&c, "今天天气如何");
        assert!(t.content.contains("【联网】广州"), "{}", t.content);
    }

    #[test]
    fn cantonese_text_gets_explicit_nudge() {
        let t = build_system_turn(&ctx(None), "我唔知道啊");
        assert!(t.content.contains("用户使用粤语"), "{}", t.content);
    }

    #[test]
    fn english_text_gets_explicit_nudge() {
        let t = build_system_turn(&ctx(None), "how many people do you see?");
        assert!(t.content.contains("reply in English"), "{}", t.content);
    }

    #[test]
    fn weekday_names_map_iso_monday_first() {
        // 2026-10-01 is a Thursday → ISO weekday 4 → 周四 (the Sunday-first
        // off-by-one this replaces said 周三).
        assert_eq!(weekday_name(4), "周四");
        assert_eq!(weekday_name(1), "周一");
        assert_eq!(weekday_name(7), "周日");
        // Out-of-range values clamp instead of panicking.
        assert_eq!(weekday_name(0), "周一");
        assert_eq!(weekday_name(9), "周日");
    }

    #[test]
    fn local_block_formats_the_clock_authoritatively() {
        let b = format_local_block(
            "2026-10-01 21:00:00",
            "周四",
            "3小时5分",
            0.42,
            8192,
            "客厅(running)",
        );
        assert!(b.contains("当前时间 2026-10-01 21:00:00 周四"), "{b}");
        assert!(b.contains("可用内存 8192 MiB"), "{b}");
        assert!(b.contains("客厅(running)"), "{b}");
        assert!(b.contains("必须以此为准"), "{b}");
    }
}
