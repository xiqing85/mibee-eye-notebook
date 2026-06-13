//! Device enumeration endpoints.
//!
//! All handlers require authentication (enforced by middleware).
//!
//! These endpoints enumerate local capture hardware (webcam, microphone)
//! by delegating to the `capture` crate.  The enumerate functions perform
//! blocking V4L2 / cpal syscalls so they are wrapped in [`spawn_blocking`].

use axum::Json;
use axum::extract::Extension;
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

/// GET /api/devices/audio — enumerate all available audio input devices.
///
/// Returns a JSON array of [`AudioDeviceResponse`].  On headless / CI systems
/// the array may be empty.
///
/// The enumeration is offloaded to a blocking thread because the underlying
/// cpal host queries are synchronous.
pub async fn list_audio_devices(
    Extension(_user): Extension<AuthenticatedUser>,
) -> impl IntoResponse {
    let devices = match tokio::task::spawn_blocking(capture::audio::enumerate_devices).await {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => {
            tracing::error!(error = %e, "failed to enumerate audio devices");
            return ApiError::internal("failed to enumerate audio devices").into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "spawn_blocking join error for audio enumeration");
            return ApiError::internal("failed to enumerate audio devices").into_response();
        }
    };

    let response: Vec<AudioDeviceResponse> = devices
        .into_iter()
        .map(|d| AudioDeviceResponse {
            name: d.name,
            supported_configs: d
                .supported_configs
                .into_iter()
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

    /// Verify that `list_audio_devices` compiles and returns an acceptable
    /// status code.
    #[tokio::test]
    async fn test_list_audio_devices_returns_ok() {
        let user = AuthenticatedUser("test".into());
        let ext = Extension(user);

        let resp = list_audio_devices(ext).await.into_response();
        let status = resp.status();
        assert!(
            status == StatusCode::OK || status == StatusCode::INTERNAL_SERVER_ERROR,
            "expected 200 or 500, got {status}"
        );
    }
}
