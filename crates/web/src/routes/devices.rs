//! Device enumeration endpoints.
//!
//! All handlers require authentication (enforced by middleware).
//!
//! These endpoints enumerate local capture hardware (webcam, microphone)
//! by delegating to the `capture` crate.  The enumerate functions perform
//! blocking V4L2 / cpal syscalls so they are wrapped in [`spawn_blocking`].

use axum::Json;
use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;

use crate::errors::ApiError;
use security::middleware::AuthenticatedUser;

// ---------------------------------------------------------------------------
// Response types (wrapper types with Serialize; capture crate types don't
// derive Serialize themselves).
// ---------------------------------------------------------------------------

/// Response shape for a single video capture device.
#[derive(Debug, Serialize)]
pub struct VideoDeviceResponse {
    pub index: usize,
    pub name: String,
    pub formats: Vec<String>,
}

/// Structured single-format entry — one per `(width, height, format, fps)`
/// tuple the camera exposes.
#[derive(Debug, Serialize)]
pub struct FormatCapabilityResponse {
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub fps: u32,
}

/// Response shape for a single audio input device.
#[derive(Debug, Serialize)]
pub struct AudioDeviceResponse {
    pub name: String,
    pub supported_configs: Vec<AudioConfigResponse>,
}

/// Response shape for an audio configuration range.
#[derive(Debug, Serialize)]
pub struct AudioConfigResponse {
    pub channels: u16,
    pub min_sample_rate: f64,
    pub max_sample_rate: f64,
    pub sample_format: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/devices/video — enumerate all available video capture devices.
///
/// Returns a JSON array of [`VideoDeviceResponse`].  On headless / CI systems
/// the array may be empty.
///
/// The enumeration is offloaded to a blocking thread because the underlying
/// V4L2 ioctl calls are synchronous.
#[tracing::instrument(skip_all)]
pub async fn list_video_devices(
    Extension(_user): Extension<AuthenticatedUser>,
) -> impl IntoResponse {
    let devices = match tokio::task::spawn_blocking(capture::video::enumerate_devices).await {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => {
            tracing::error!(error = %e, "failed to enumerate video devices");
            return ApiError::internal("failed to enumerate video devices").into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "spawn_blocking join error for video enumeration");
            return ApiError::internal("failed to enumerate video devices").into_response();
        }
    };

    let response: Vec<VideoDeviceResponse> = devices
        .into_iter()
        .map(|d| VideoDeviceResponse {
            index: d.index,
            name: d.name,
            formats: d.formats,
        })
        .collect();

    (StatusCode::OK, Json(response)).into_response()
}

/// GET /api/devices/video/{index}/formats — list the structured
/// `(width, height, format, fps)` capabilities of a single camera.
///
/// Unlike [`list_video_devices`], which returns format strings, this returns
/// discrete fields so the Web UI can render a resolution / fps picker and the
/// streaming layer can request a specific format when the stream is created.
/// Entries are sorted highest-resolution-first.
#[tracing::instrument(skip_all)]
pub async fn list_video_device_formats(
    Extension(_user): Extension<AuthenticatedUser>,
    Path(index): Path<usize>,
) -> impl IntoResponse {
    let result = tokio::task::spawn_blocking(move || {
        capture::video::enumerate_device_formats_detailed(index)
    })
    .await;

    let formats = match result {
        Ok(Ok(f)) => f,
        Ok(Err(e)) => {
            tracing::warn!(device_index = index, error = %e, "failed to enumerate formats");
            return ApiError::not_found(format!(
                "could not enumerate formats for device {index}: {e}"
            ))
            .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "spawn_blocking join error for format enumeration");
            return ApiError::internal("failed to enumerate device formats").into_response();
        }
    };

    let response: Vec<FormatCapabilityResponse> = formats
        .into_iter()
        .map(|f| FormatCapabilityResponse {
            width: f.width,
            height: f.height,
            format: f.format,
            fps: f.fps,
        })
        .collect();

    (StatusCode::OK, Json(response)).into_response()
}

/// GET /api/devices/audio — enumerate all available audio input devices.
///
/// Returns a JSON array of [`AudioDeviceResponse`].  On headless / CI systems
/// the array may be empty.
///
/// The enumeration is offloaded to a blocking thread because the underlying
/// cpal host queries are synchronous.
#[tracing::instrument(skip_all)]
pub async fn list_audio_devices(
    Extension(_user): Extension<AuthenticatedUser>,
) -> impl IntoResponse {
    list_audio_devices_with(capture::audio::enumerate_devices).await
}

async fn list_audio_devices_with(
    enumerate: fn() -> anyhow::Result<Vec<capture::audio::AudioDeviceInfo>>,
) -> impl IntoResponse {
    let devices = match tokio::task::spawn_blocking(enumerate).await {
        Ok(Ok(d)) => d,
        // Fail-open: a host that cannot enumerate audio inputs (user
        // service without device access, headless CI) reports an empty
        // list — the endpoint contract already says the array may be
        // empty, and the 500 broke the devices view everywhere audio
        // access was missing (found by the browser walkthrough on
        // :8443, 2026-09-27).
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "audio enumeration failed — reporting no devices (fail-open)");
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(error = %e, "audio enumeration join error — reporting no devices");
            Vec::new()
        }
    };

    // cpal surfaces every ALSA PCM plugin as a device with tens of
    // thousands of rate/format rows (rate-converter plugins alone list
    // ~15k configs each) — a multi-megabyte payload that blew the
    // envelope middleware's body limit and 500'd the devices view. The
    // UI only lists device names; keep a few config rows per device as
    // a representative hint.
    const MAX_CONFIGS_PER_DEVICE: usize = 8;
    let response: Vec<AudioDeviceResponse> = devices
        .into_iter()
        .map(|d| AudioDeviceResponse {
            name: d.name,
            supported_configs: d
                .supported_configs
                .into_iter()
                .take(MAX_CONFIGS_PER_DEVICE)
                .map(|c| AudioConfigResponse {
                    channels: c.channels,
                    min_sample_rate: c.min_sample_rate,
                    max_sample_rate: c.max_sample_rate,
                    sample_format: c.sample_format,
                })
                .collect(),
        })
        .collect();

    (StatusCode::OK, Json(response)).into_response()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    /// Verify that `list_video_devices` compiles and returns an acceptable
    /// status code (200 on most systems, 500 only if V4L2 is genuinely broken).
    #[tokio::test]
    async fn test_list_video_devices_returns_ok() {
        let user = AuthenticatedUser("test".into());
        let ext = Extension(user);

        let resp = list_video_devices(ext).await.into_response();
        let status = resp.status();
        assert!(
            status == StatusCode::OK || status == StatusCode::INTERNAL_SERVER_ERROR,
            "expected 200 or 500, got {status}"
        );
    }

    /// `list_audio_devices` is always 200 — enumeration failure degrades
    /// to an empty list (fail-open), never a 500.
    #[tokio::test]
    async fn test_list_audio_devices_returns_ok() {
        let user = AuthenticatedUser("test".into());
        let ext = Extension(user);

        let resp = list_audio_devices(ext).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// ALSA plugin "devices" report tens of thousands of config rows —
    /// the response must stay bounded (envelope middleware buffers at
    /// most 4MB; a raw dump 500'd the devices view on :8443).
    #[tokio::test]
    async fn test_list_audio_devices_caps_configs() {
        fn many_configs() -> anyhow::Result<Vec<capture::audio::AudioDeviceInfo>> {
            Ok(vec![capture::audio::AudioDeviceInfo {
                name: "rate converter plugin".into(),
                supported_configs: (0..20_000)
                    .map(|_| capture::audio::AudioConfigInfo {
                        channels: 2,
                        min_sample_rate: 44_100.0,
                        max_sample_rate: 48_000.0,
                        sample_format: "F32".into(),
                    })
                    .collect(),
            }])
        }
        let resp = list_audio_devices_with(many_configs).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("read body");
        let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(v.as_array().map(Vec::len), Some(1));
        assert_eq!(
            v[0]["supported_configs"].as_array().map(Vec::len),
            Some(8),
            "configs must be capped to 8 per device"
        );
    }

    /// An environment with no enumerable audio inputs (user service
    /// without device access) must answer 200 with an empty array, not
    /// 500 — regression for the :8443 devices-view breakage.
    #[tokio::test]
    async fn test_list_audio_devices_fail_open_empty() {
        fn no_audio() -> anyhow::Result<Vec<capture::audio::AudioDeviceInfo>> {
            Err(anyhow::anyhow!("no audio host"))
        }
        let resp = list_audio_devices_with(no_audio).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        assert_eq!(body.as_ref(), b"[]");
    }
}
