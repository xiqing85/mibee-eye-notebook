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

use security::middleware::AuthenticatedUser;
use streaming::capability::{EncoderProfile, SystemCapabilities, probe, recommended_profiles};

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
#[tracing::instrument(skip_all)]
pub async fn get_capabilities(Extension(_user): Extension<AuthenticatedUser>) -> impl IntoResponse {
    static CACHE: std::sync::OnceLock<CapabilitiesResponse> = std::sync::OnceLock::new();
    let cached = CACHE.get_or_init(|| {
        let system = probe();
        let recommended = recommended_profiles(&system);
        CapabilitiesResponse {
            system,
            recommended_profiles: recommended,
        }
    });
    let superset = serde_json::json!({
        "spec_version": "1",
        "device": {
            "name": "mibee-rec",
            "model": "notebook",
            "vendor": "MiBee Studio",
        },
        "auth": {"model": "session", "setup": true},
        "multi_camera": true,
        "camera_management": true,
        "camera_control": true,
        "imaging": false,
        "ai": false,
        "ptz": false,
        "hls": false,
        // Recording is config-only on this device (protocols.recording);
        // there is no per-camera record endpoint yet.
        "recording": false,
        "devices": true,
        "mjpeg": true,
        "mse": true,
        "webrtc": false,
        "events": ["camera_added", "camera_offlined"],
        "config_apply": {"default": "immediate", "sections": {}},
        "observability": {"metrics": true, "logs": true, "requests": true},
        // Device-specific extension: the host hardware probe.
        "system": cached.system,
        "recommended_profiles": cached.recommended_profiles,
    });
    (StatusCode::OK, Json(superset)).into_response()
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
}
