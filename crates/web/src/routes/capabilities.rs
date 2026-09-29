//! Hardware capability introspection endpoints.
//!
//! `GET /api/capabilities` returns the probed host hardware snapshot plus the
//! recommended encoder profiles, letting the Web UI present adaptive
//! resolution / quality options to the user.

use axum::Json;
use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;
use std::sync::Arc;

use security::middleware::AuthenticatedUser;
use streaming::ai::AiEngine;
use streaming::capability::{EncoderProfile, SystemCapabilities, probe, recommended_profiles};
use tokio::sync::Mutex;

use crate::protocol_runtime::ProtocolRuntime;

// ---------------------------------------------------------------------------
// Response model
// ---------------------------------------------------------------------------

/// Response body for `GET /api/capabilities`.
#[derive(Debug, Serialize)]
pub struct CapabilitiesResponse {
    /// Probed host hardware + available encoder backends.
    pub system: SystemCapabilities,
    /// Suggested encoder profiles (one per resolution tier the host can drive).
    pub recommended_profiles: Vec<EncoderProfile>,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// GET /api/capabilities — the SPEC v1 capability superset (§3.1) plus the
/// host hardware probe as extension fields (`system`, `recommended_profiles`).
///
/// Probing is cheap (a handful of sysfs/`/proc` reads) but not free, so the
/// result is cached for the process lifetime via a [`std::sync::OnceLock`].
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(skip_all)]
pub async fn get_capabilities(
    Extension(ai): Extension<Arc<AiEngine>>,
    Extension(audio_ai): Extension<Arc<streaming::audio_ai::AudioAiEngine>>,
    Extension(ocr): Extension<Arc<streaming::ocr::OcrEngine>>,
    Extension(voice): Extension<Arc<streaming::voice::VoiceEngine>>,
    Extension(chat): Extension<Arc<streaming::llm::ChatEngine>>,
    Extension(decision): Extension<Arc<streaming::decision::DecisionEngine>>,
    Extension(meeting): Extension<Arc<streaming::meeting::MeetingEngine>>,
    Extension(vlm): Extension<Arc<streaming::vlm::VlmEngine>>,
    Extension(stream_manager): Extension<Arc<crate::stream_manager::StreamManager>>,
    Extension(_protocol_runtime): Extension<Arc<Mutex<ProtocolRuntime>>>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> impl IntoResponse {
    static CACHE: std::sync::OnceLock<CapabilitiesResponse> = std::sync::OnceLock::new();
    let cached = CACHE.get_or_init(|| {
        let system = probe();
        let recommended = recommended_profiles(&system);
        CapabilitiesResponse {
            system,
            recommended_profiles: recommended,
        }
    });
    let mut events = vec!["camera_added", "camera_offlined"];
    let ai_hot_swap = ai.is_active() && ai_has_factory(&ai);
    if ai.is_active() {
        events.push("ai_detection");
    }
    if ai_hot_swap {
        events.push("ai_model_changed");
    }
    // The alarm bridge fires on AI rising edges (visual) or voted sound
    // classes (audio) and fans out to the SSE hub, the GB Alarm NOTIFY
    // and the ONVIF MotionAlarm — each channel no-ops until its consumer
    // is up/subscribed, so the event itself only requires either engine.
    if ai.is_active() || audio_ai.is_active() {
        events.push("alarm");
    }
    // Zone events ride the tracker inside the AI worker.
    if ai.is_active() {
        events.push("zone_event");
    }
    if voice.is_active() {
        events.push("voice_transcript");
    }
    // Meeting lifecycle (SPEC appendix A #27).
    if meeting.is_active() {
        events.push("meeting_state");
    }
    // VLM alarm-frame descriptions (SPEC appendix A #23) ride the visual
    // alarm pipeline asynchronously.
    if vlm.is_active() {
        events.push("alarm_description");
    }
    let superset = serde_json::json!({
        "spec_version": "1",
        "device": {
            "name": "mibee-eye",
            "model": "notebook",
            "vendor": "MiBee Studio",
        },
        "auth": {"model": "session", "setup": true},
        "multi_camera": true,
        "camera_management": true,
        "camera_control": true,
        "imaging": false,
        "ai": ai.is_active(),
        // Always-on sound-event detection + voice-presence signal
        // (SPEC appendix A notebook dialect; fail-open like `ai`).
        "audio_ai": audio_ai.is_active(),
        // Persistent hearing records (`GET/DELETE /api/audio/records`,
        // SPEC appendix A #24): anything the audio engines recognize
        // lands as a queryable text record.
        "audio_records": audio_records_capable(audio_ai.is_active(), voice.is_active()),
        // User-drawn intrusion/tripwire zones (`GET/PUT
        // /api/cameras/{id}/zones`); events require AI tracking.
        "zones": ai.is_active(),
        // On-device text recognition (PP-OCRv5, `POST /api/ocr`).
        "ocr": ocr.is_active(),
        // Wake word + offline transcription (SSE `voice_transcript`).
        "voice": voice.is_active(),
        // Voiceprint speaker profiles (`/api/voice/speakers`, SPEC
        // appendix A #25): enroll, verify-gate wake words, tag records.
        "voice_speakers": voice_speakers_capable(voice.is_active(), voice.speaker_capable()),
        // Local LLM dialogue (`POST /api/chat`).
        "chat": chat.is_active(),
        // Laya typed-decision triage over voice transcripts (SSE
        // `voice_decision`, SPEC appendix A #26).
        "decision": decision.is_active(),
        // On-demand meeting mode (record → diarize → transcribe, SPEC
        // appendix A #27).
        "meeting": meeting.is_active(),
        // VLM alarm-frame descriptions (SSE `alarm_description`).
        "vlm": vlm.is_active(),
        "ai_models": ai_hot_swap,
        "ai_upload": ai_hot_swap && ai.config().allow_upload,
        "ptz": false,
        "hls": false,
        // Recording is config-only on this device (protocols.recording);
        // there is no per-camera record endpoint yet.
        "recording": false,
        // SPEC v1 §5.2: burned into every video output pre-encode; config
        // lives in protocols.watermark (applies at next stream start).
        "watermark": true,
        "devices": true,
        "mjpeg": true,
        "mse": true,
        // Low-resolution substream (SPEC appendix A #20): advertised
        // while any active stream runs with a substream.
        "substream": stream_manager.any_substream_active().await,
        "webrtc": false,
        "events": events,
        "config_apply": {"default": "immediate", "sections": {}},
        "observability": {"metrics": true, "logs": true, "requests": true},
        // Device-specific extension: the host hardware probe.
        "system": cached.system,
        "recommended_profiles": cached.recommended_profiles,
    });
    (StatusCode::OK, Json(superset)).into_response()
}

/// Whether the engine can load models at runtime (hot-switch + upload).
fn ai_has_factory(ai: &AiEngine) -> bool {
    ai.can_load_models()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_response_is_serialisable() {
        let system = probe();
        let resp = CapabilitiesResponse {
            system,
            recommended_profiles: Vec::new(),
        };
        let json = serde_json::to_value(&resp).expect("must serialise");
        assert!(json.get("system").is_some());
    }

    /// The capability superset is hand-built JSON — a check that the
    /// watermark key (SPEC v1 §5.2) stays present and true.
    #[tokio::test]
    async fn capabilities_advertise_watermark() {
        let ai = Arc::new(AiEngine::from_parts(
            streaming::ai::AiConfig::default(),
            None,
        ));
        let audio_ai = Arc::new(streaming::audio_ai::AudioAiEngine::from_config(
            &streaming::audio_ai::AudioAiConfig::default(),
        ));
        let res = get_capabilities(
            Extension(ai),
            Extension(audio_ai),
            Extension(Arc::new(streaming::ocr::OcrEngine::from_config(
                &streaming::ocr::OcrConfig::default(),
            ))),
            Extension(Arc::new(streaming::voice::VoiceEngine::from_config(
                &streaming::voice::VoiceConfig::default(),
            ))),
            Extension(Arc::new(streaming::llm::ChatEngine::from_config(
                &streaming::llm::LlmConfig::default(),
            ))),
            Extension(Arc::new(streaming::decision::DecisionEngine::from_config(
                &streaming::decision::DecisionConfig::default(),
            ))),
            Extension(Arc::new(streaming::meeting::MeetingEngine::from_config(
                &streaming::meeting::MeetingConfig::default(),
                &streaming::voice::VoiceConfig::default(),
            ))),
            Extension(Arc::new(streaming::vlm::VlmEngine::from_config(
                &streaming::vlm::VlmConfig::default(),
            ))),
            Extension(Arc::new(crate::stream_manager::StreamManager::new())),
            Extension(Arc::new(tokio::sync::Mutex::new(
                crate::protocol_runtime::ProtocolRuntime::new(),
            ))),
            Extension(security::middleware::AuthenticatedUser("admin".to_string())),
        )
        .await
        .into_response();
        let body = axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["watermark"], serde_json::json!(true));
        // No active streams → substream capability false (SPEC appendix
        // A #20: it follows an active substream pipeline, not the config).
        assert_eq!(json["substream"], serde_json::json!(false));
        // Neither audio engine active (default engines: no models in
        // tests, fail-open) → no hearing-records capability.
        assert_eq!(json["audio_records"], serde_json::json!(false));
    }
}

/// SPEC appendix A #24: the hearing-records surface (`GET/DELETE
/// /api/audio/records`) exists when either audio engine is active — the
/// device can hear through sound events or the voice loop alike.
fn audio_records_capable(audio_ai_active: bool, voice_active: bool) -> bool {
    audio_ai_active || voice_active
}

/// Voiceprint features need BOTH the wake-word loop and the speaker
/// embedding model (a missing CAM++ file keeps the capability off while
/// the rest of voice keeps working).
fn voice_speakers_capable(voice_active: bool, speaker_model_loaded: bool) -> bool {
    voice_active && speaker_model_loaded
}

#[cfg(test)]
mod voice_speakers_tests {
    use super::voice_speakers_capable;

    #[test]
    fn truth_table() {
        assert!(!voice_speakers_capable(false, false));
        assert!(!voice_speakers_capable(true, false), "model file required");
        assert!(!voice_speakers_capable(false, true), "voice loop required");
        assert!(voice_speakers_capable(true, true));
    }
}

#[cfg(test)]
mod audio_records_tests {
    use super::audio_records_capable;

    #[test]
    fn audio_records_capability_follows_any_active_audio_engine() {
        assert!(!audio_records_capable(false, false));
        assert!(audio_records_capable(true, false));
        assert!(audio_records_capable(false, true));
        assert!(audio_records_capable(true, true));
    }
}
