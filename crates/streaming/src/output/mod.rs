//! Output trait and concrete output adapters.
//!
//! [`Output`] is the consumer side of the stream pipeline — it receives
//! [`MediaFrame`]s  and delivers them to a downstream destination
//! (RTSP clients, RTMP push target, etc.).
//!
//! Concrete adapters:
//! - [`RtspOutput`] — feeds frames into an RTSP server for client distribution
//! - [`RtmpOutput`] — pushes frames via RTMP to an ingest point (e.g. MiBee NVR)

pub mod file;
pub mod gb28181;
#[cfg(test)]
pub mod mock;
pub mod rtsp;
pub mod rtmp;

pub use file::FileOutput;
pub use gb28181::Gb28181Output;
#[cfg(test)]
pub use mock::MockOutput;
pub use rtsp::RtspOutput;
pub use rtmp::RtmpOutput;

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;

use crate::source::MediaFrame;
use protocols::h264;

// ── Output trait ───────────────────────────────────────────────────────────────

/// Async consumer of media frames.
///
/// Implementors take [`MediaFrame`] values and deliver them somewhere
/// (network stream, file, another process, …).
///
/// # Lifetimes
///
/// Like [`Source`](crate::source::Source), each method returns a pinned
/// boxed future tied to `&mut self` for trait-object safety.
pub trait Output: Send + 'static {
    /// Start the output (open connection / bind listener).
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    /// Deliver a frame to the output.
    ///
    /// Blocks (asynchronously) until the frame has been handed off.
    /// Returns [`Err`] on permanent failure (output should be stopped).
    fn send_frame(
        &mut self,
        frame: &MediaFrame,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    /// Stop the output and release resources.
    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}

// ── Shared helpers ─────────────────────────────────────────────────────────────

/// Parse H.264 data into individual NAL units, handling both Annex B and
/// AVCC formats.
pub(super) fn parse_h264_nal_units(data: &[u8]) -> Vec<Vec<u8>> {
    if data.len() < 4 {
        return vec![data.to_vec()];
    }

    // Detect format: check for Annex B start code (0x00 0x00 0x01 or
    // 0x00 0x00 0x00 0x01) anywhere in the data.
    let is_annex_b = data.windows(3).any(|w| w == [0x00, 0x00, 0x01]);

    if is_annex_b {
        h264::split_nal_units(data)
            .iter()
            .map(|n| n.to_vec())
            .collect()
    } else {
        // Assume AVCC format (4-byte length prefix)
        h264::split_nal_units_avcc(data)
            .iter()
            .map(|n| n.to_vec())
            .collect()
    }
}

// Preserve the `crate::output::tests::MockOutput` path used by hub.rs tests.
#[cfg(test)]
pub(crate) mod tests {
    pub(crate) use super::mock::MockOutput;
}
