//! WebRTC WHIP/WHEP signalling endpoints.
//!
//! WHEP (WebRTC-HTTP Egress Protocol) lets a browser *pull* a camera stream
//! via RTCPeerConnection for sub-second latency. WHIP does the same for
//! ingest. Both exchange SDP offers/answers over plain HTTP — no WebSocket
//! signalling server needed.
//!
//! # Status
//!
//! The endpoint scaffolding, config gating, and request/response shapes are
//! implemented here. The actual SRTP/ICE/RTP plumbing is provided by the
//! `str0m` crate (sans-IO WebRTC). Integration of str0m's `Rtc` instance with
//! the hub's H.264 NAL stream (RTP packetisation + DTLS handshake driving) is
//! the remaining work — until then these endpoints return 501 to indicate the
//! WebRTC transport is not yet wired up, even when enabled in config.
//!
//! When `str0m` integration lands, the flow will be:
//!   1. POST /api/webrtc/whep/{camera_id} with SDP offer body
//!   2. Create an `Rtc`, add an H.264 video track, drive ICE/DTLS to connected
//!   3. Subscribe to the camera's frame broadcast, packetise NALs into RTP
//!   4. Return the SDP answer; subsequent RTP/SRTP flows over a UDP task

use axum::extract::{Extension, Path};
use axum::response::IntoResponse;
use tracing::debug;

use crate::errors::ApiError;
use crate::stream_manager::StreamManager;
use security::middleware::AuthenticatedUser;

/// POST /api/webrtc/whep/{camera_id} — WHEP egress (browser pulls a stream).
///
/// Accepts an SDP offer (`Content-Type: application/sdp`) and returns an SDP
/// answer. The browser then opens RTCPeerConnection and receives H.264 video
/// over SRTP at sub-second latency.
///
/// Returns 501 until the str0m RTP bridge is implemented (see module docs).
#[tracing::instrument(skip_all)]
pub async fn whep(
    Extension(stream_manager): Extension<std::sync::Arc<StreamManager>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(camera_id): Path<String>,
    body: String,
) -> axum::response::Response {
    // Confirm the camera has an active stream — WHEP can only egress what's
    // already being captured/encoded.
    if !stream_manager.has_stream(&camera_id).await {
        return ApiError::conflict("stream not active — start the camera first").into_response();
    }

    debug!(
        camera_id = %camera_id,
        offer_len = body.len(),
        "WHEP SDP offer received"
    );

    // TODO(str0m): implement the full WHEP flow:
    //   1. Parse the SDP offer.
    //   2. Create a str0m `Rtc`, add an H.264 (H.264/90000) video track with
    //      the camera's SPS/PPS as a/sprop-parameter-sets.
    //   3. Accept the offer, produce an SDP answer.
    //   4. Spawn a UDP task that drives `Rtc::handle_input`/`poll_output` and
    //      packetises NAL units from `stream_manager.subscribe_frames()` into
    //      RTP on the video track.
    //   5. Return the answer with Content-Type: application/sdp.
    //
    // Until then, signal that the transport is not wired up so the browser
    // shows a clear error rather than hanging on a partial handshake.
    ApiError::not_implemented(
        "WebRTC transport is enabled in config but the str0m RTP bridge is not yet implemented. \
         Use the MSE (stream.mse) or MJPEG (/live) transport for now.",
    )
    .into_response()
}

/// POST /api/webrtc/whip/{camera_id} — WHIP ingest (browser pushes a stream).
///
/// Reserved for future camera-ingest use (e.g. a remote browser acting as a
/// camera source). The local-USB-camera product never needs WHIP, but the
/// endpoint is registered for protocol completeness. Returns 501.
#[tracing::instrument(skip_all)]
pub async fn whip(
    Extension(_user): Extension<AuthenticatedUser>,
    Path(_camera_id): Path<String>,
    _body: String,
) -> axum::response::Response {
    ApiError::not_implemented("WHIP ingest is not implemented").into_response()
}

// Allow unused `StatusCode` import on configurations where it's not directly
// referenced after the scaffold compiles.
#[allow(unused_imports)]
use axum::http::StatusCode as _StatusCode;
