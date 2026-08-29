//! MSE / fMP4 streaming endpoint.
//!
//! `GET /api/cameras/{id}/stream.mse` returns a chunked HTTP response whose
//! body is a fragmented-MP4 byte-stream consumable by a browser
//! `MediaSource`. This is the H.264 delivery path (vs the legacy MJPEG
//! `<img>` preview) and carries a ~5-10× bandwidth saving plus client-side
//! hardware decoding.
//!
//! Each browser connection gets its own [`Fmp4Remuxer`] which joins the
//! stream at the next keyframe boundary, emits an init segment, then forwards
//! rolling media segments.

use std::convert::Infallible;

use axum::body::Body;
use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use tokio_stream::StreamExt;
use tracing::{debug, warn};

use security::middleware::AuthenticatedUser;
use streaming::fmp4::Fmp4Remuxer;
use streaming::source::MediaFrame;

use crate::errors::ApiError;
use crate::stream_manager::StreamManager;

/// GET /api/cameras/{id}/stream.mse — fragmented-MP4 stream for MSE playback.
///
/// Content-Type is `video/mp4` with a `media` brand, matching the MSE byte-stream
/// spec. The response body is chunked: an init segment first, then rolling
/// media segments as they are produced by the [`Fmp4Remuxer`].
#[tracing::instrument(skip_all)]
pub async fn stream_mse(
    Extension(stream_manager): Extension<std::sync::Arc<StreamManager>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(camera_id): Path<String>,
) -> Response {
    let mut rx = match stream_manager.subscribe_frames(&camera_id).await {
        Some(rx) => rx,
        None => {
            return ApiError::conflict("stream not active or no frames available").into_response();
        }
    };

    // Drive the remuxer on a background task and forward chunks over a
    // channel; the HTTP body reads from the channel so backpressure flows
    // naturally back to the encoder.
    let (chunk_tx, chunk_rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, Infallible>>(16);
    let mut remuxer = Fmp4Remuxer::new();
    // Bootstrap the init segment with cached SPS/PPS so the browser can start
    // decoding the very first media segment, rather than stalling until the
    // next IDR (which may be a full GOP away) re-emits the parameter sets.
    if let Some((sps, pps)) = stream_manager.sps_pps(&camera_id).await {
        remuxer.seed_sps_pps(sps, pps);
    }

    tokio::spawn(async move {
        loop {
            let frame = match rx.recv().await {
                Ok(f) => f,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    debug!("mse: frame broadcast closed, ending stream");
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    // Subscriber fell behind; skip ahead. The remuxer only
                    // emits output after a keyframe, so a lag simply delays the
                    // next media segment until the next IDR realigns the stream.
                    warn!("mse: client lagged by {n} frames, resync at next keyframe");
                    continue;
                }
            };
            let MediaFrame::Video { .. } = &*frame else {
                continue;
            };
            let chunks = match remuxer.push(&frame) {
                Ok(c) => c,
                Err(e) => {
                    warn!(error = %e, "mse: remux push failed, dropping client");
                    break;
                }
            };
            for chunk in chunks {
                if chunk_tx.send(Ok(chunk.into_bytes())).await.is_err() {
                    debug!("mse: client disconnected");
                    return;
                }
            }
        }
        // Flush any tail segment on graceful end.
        if let Some(tail) = remuxer.flush() {
            let _ = chunk_tx.send(Ok(tail)).await;
        }
    });

    let body_stream = tokio_stream::wrappers::ReceiverStream::new(chunk_rx).map(|res| match res {
        Ok(bytes) => Ok::<axum::body::Bytes, std::io::Error>(axum::body::Bytes::from(bytes)),
        // Infallible — never reached.
        Err(_) => unreachable!(),
    });

    let body = Body::from_stream(body_stream);

    (
        StatusCode::OK,
        [
            ("content-type", "video/mp4"),
            ("cache-control", "no-cache, no-store, must-revalidate"),
            // Hint that this is a live stream.
            ("x-content-type-options", "nosniff"),
        ],
        body,
    )
        .into_response()
}
