//! Output trait and concrete output adapters.
//!
//! [`Output`] is the consumer side of the stream pipeline — it receives
//! [`MediaFrame`]s  and delivers them to a downstream destination
//! (RTSP clients, RTMP push target, etc.).
//!
//! Concrete adapters:
//! - [`RtspOutput`] — feeds frames into an RTSP server for client distribution
//! - [`RtmpOutput`] — pushes frames via RTMP to an ingest point (e.g. MiBee NVR)

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;

use crate::source::MediaFrame;

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

// ── Concrete outputs ───────────────────────────────────────────────────────────

// ---------------------------------------------------------------------------
// RtspOutput
// ---------------------------------------------------------------------------

/// RTSP server output.
///
/// Registers a stream with the RTSP server and feeds incoming frames to
/// all connected RTSP clients via interleaved RTP/TCP.
///
/// **Current status**: structural adapter — wires frame data to the RTSP
/// server's [`build_interleaved_frame`] helper.
#[allow(dead_code)]
pub struct RtspOutput {
    /// Stream path (used as the RTSP mount point, e.g. "webcam").
    stream_path: String,
    /// SDP body describing the stream (codec, payload type, etc.).
    sdp_body: String,
    /// SSRC for RTP packets.
    ssrc: u32,
    /// Whether the output has been started.
    started: bool,
}

impl RtspOutput {
    /// Create a new RTSP output.
    pub fn new(stream_path: &str, sdp_body: &str, ssrc: u32) -> Self {
        Self {
            stream_path: stream_path.to_string(),
            sdp_body: sdp_body.to_string(),
            ssrc,
            started: false,
        }
    }
}

impl Output for RtspOutput {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // TODO(T17): Register this stream with the global RtspServer via
            //   server.add_stream(StreamConfig::new(&self.stream_path, &self.sdp_body, self.ssrc));
            // For now, we validate the configuration and mark as started.
            if self.stream_path.is_empty() {
                anyhow::bail!("RtspOutput stream path must not be empty");
            }
            self.started = true;
            tracing::info!("RtspOutput started: /{}", self.stream_path);
            Ok(())
        })
    }

    fn send_frame(
        &mut self,
        _frame: &MediaFrame,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if !self.started {
                anyhow::bail!("RtspOutput not started");
            }
            // TODO(T17): Convert MediaFrame → RTP packet, then use
            //   protocols::rtsp_server::build_interleaved_frame(channel, &rtp_packet)
            // to construct the interleaved frame and write it to each connected
            // client's TCP stream.
            Ok(())
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.started = false;
            tracing::info!("RtspOutput stopped: /{}", self.stream_path);
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// RtmpOutput
// ---------------------------------------------------------------------------

/// RTMP push output.
///
/// Connects to an RTMP ingest point (such as MiBee NVR) and pushes
/// H.264/AAC frames as RTMP video/audio messages.
///
/// **Current status**: structural adapter — will send RTMP commands and
/// frames over a TCP socket.
#[allow(dead_code)]
pub struct RtmpOutput {
    /// RTMP URL (e.g. `rtmp://localhost:1935/live/stream`).
    url: String,
    /// Application name extracted from URL.
    app_name: String,
    /// Stream key extracted from URL.
    stream_key: String,
    /// Whether the output has been started.
    started: bool,
}

impl RtmpOutput {
    /// Create a new RTMP output from a full RTMP URL.
    ///
    /// The URL format is: `rtmp://host:port/app/streamKey`
    pub fn new(url: &str) -> Self {
        // Crude URL parsing for RTMP — enough for the structural adapter.
        let (app_name, stream_key) = Self::parse_rtmp_url(url);
        Self {
            url: url.to_string(),
            app_name,
            stream_key,
            started: false,
        }
    }

    /// Create an RTMP output with explicit app and stream key.
    pub fn new_with_parts(host: &str, port: u16, app: &str, stream: &str) -> Self {
        let url = format!("rtmp://{host}:{port}/{app}/{stream}");
        Self {
            url,
            app_name: app.to_string(),
            stream_key: stream.to_string(),
            started: false,
        }
    }

    fn parse_rtmp_url(url: &str) -> (String, String) {
        // Strip "rtmp://" prefix, then parse path
        let rest = url.trim_start_matches("rtmp://");
        if let Some(slash_pos) = rest.find('/') {
            let after_host = &rest[slash_pos + 1..];
            if let Some(second_slash) = after_host.find('/') {
                (
                    after_host[..second_slash].to_string(),
                    after_host[second_slash + 1..].to_string(),
                )
            } else {
                (after_host.to_string(), String::new())
            }
        } else {
            ("live".to_string(), "stream".to_string())
        }
    }
}

impl Output for RtmpOutput {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // TODO(T19): Connect to RTMP server, perform handshake, send
            // connect/createStream/publish commands. After that, frames can
            // be pushed via send_frame.
            if self.url.is_empty() {
                anyhow::bail!("RtmpOutput URL must not be empty");
            }
            self.started = true;
            tracing::info!("RtmpOutput started: {}", self.url);
            Ok(())
        })
    }

    fn send_frame(
        &mut self,
        _frame: &MediaFrame,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if !self.started {
                anyhow::bail!("RtmpOutput not started");
            }
            // TODO(T19): Convert MediaFrame → RTMP chunk and write to the
            // TCP socket. Video frames go as message type 9 (video), audio
            // as message type 8 (audio).
            Ok(())
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // TODO(T19): Send FCUnpublish / close connection.
            self.started = false;
            tracing::info!("RtmpOutput stopped");
            Ok(())
        })
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // Mock output that records received frames for verification.
    pub(crate) struct MockOutput {
        received: Arc<Mutex<Vec<MediaFrame>>>,
        started: bool,
        fail_on_send: bool,
    }

    impl MockOutput {
        pub fn new() -> Self {
            Self {
                received: Arc::new(Mutex::new(Vec::new())),
                started: false,
                fail_on_send: false,
            }
        }

        pub fn with_fail() -> Self {
            Self {
                received: Arc::new(Mutex::new(Vec::new())),
                started: false,
                fail_on_send: true,
            }
        }

        pub fn receiver(&self) -> Arc<Mutex<Vec<MediaFrame>>> {
            self.received.clone()
        }
    }

    impl Output for MockOutput {
        fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
            Box::pin(async move {
                self.started = true;
                Ok(())
            })
        }

        fn send_frame(
            &mut self,
            frame: &MediaFrame,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
            let f = frame.clone();
            Box::pin(async move {
                if !self.started {
                    anyhow::bail!("MockOutput not started");
                }
                if self.fail_on_send {
                    anyhow::bail!("MockOutput simulated failure");
                }
                self.received.lock().await.push(f);
                Ok(())
            })
        }

        fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
            Box::pin(async move {
                self.started = false;
                Ok(())
            })
        }
    }

    // ── Output trait tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_mock_output_lifecycle() {
        let mut out = MockOutput::new();
        out.start().await.unwrap();

        let frame = MediaFrame::Video {
            keyframe: true,
            data: vec![0x67],
            timestamp: 0,
        };
        out.send_frame(&frame).await.unwrap();

        let received = out.receiver();
        let frames = received.lock().await;
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], frame);

        out.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_mock_output_send_before_start() {
        let mut out = MockOutput::new();
        let frame = MediaFrame::Audio {
            data: vec![],
            timestamp: 0,
        };
        assert!(out.send_frame(&frame).await.is_err());
    }

    #[tokio::test]
    async fn test_mock_output_failure_mode() {
        let mut out = MockOutput::with_fail();
        out.start().await.unwrap();
        let frame = MediaFrame::Video {
            keyframe: false,
            data: vec![],
            timestamp: 0,
        };
        assert!(out.send_frame(&frame).await.is_err());
    }

    // ── RtspOutput tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_rtsp_output_start_stop() {
        let mut out = RtspOutput::new(
            "webcam",
            "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=Test\r\nt=0 0\r\n",
            0x1234,
        );
        out.start().await.unwrap();
        assert!(out.started);
        out.stop().await.unwrap();
        assert!(!out.started);
    }

    #[tokio::test]
    async fn test_rtsp_output_empty_path_fails() {
        let mut out = RtspOutput::new("", "", 0);
        assert!(out.start().await.is_err());
    }

    #[tokio::test]
    async fn test_rtsp_output_send_frame() {
        let mut out = RtspOutput::new("test", "s=Test", 1);
        out.start().await.unwrap();
        let frame = MediaFrame::Video {
            keyframe: true,
            data: vec![0x67, 0x42, 0x80],
            timestamp: 42,
        };
        // Should not error (it's a no-op stub for now)
        out.send_frame(&frame).await.unwrap();
        out.stop().await.unwrap();
    }

    // ── RtmpOutput tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_rtmp_output_start_stop() {
        let mut out = RtmpOutput::new("rtmp://localhost:1935/live/stream");
        out.start().await.unwrap();
        assert!(out.started);
        out.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_rtmp_output_empty_url_fails() {
        let mut out = RtmpOutput::new("");
        assert!(out.start().await.is_err());
    }

    #[tokio::test]
    async fn test_rtmp_output_send_frame() {
        let mut out = RtmpOutput::new("rtmp://localhost:1935/live/stream");
        out.start().await.unwrap();
        let frame = MediaFrame::Audio {
            data: vec![0xFF; 160],
            timestamp: 100,
        };
        out.send_frame(&frame).await.unwrap();
        out.stop().await.unwrap();
    }

    #[test]
    fn test_rtmp_url_parsing() {
        let (app, key) = RtmpOutput::parse_rtmp_url("rtmp://example.com:1935/live/mykey");
        assert_eq!(app, "live");
        assert_eq!(key, "mykey");
    }

    #[test]
    fn test_rtmp_url_parsing_no_key() {
        let (app, key) = RtmpOutput::parse_rtmp_url("rtmp://example.com/app");
        assert_eq!(app, "app");
        assert_eq!(key, "");
    }

    #[test]
    fn test_rtmp_output_new_with_parts() {
        let out = RtmpOutput::new_with_parts("localhost", 1935, "live", "test");
        assert_eq!(out.url, "rtmp://localhost:1935/live/test");
        assert_eq!(out.app_name, "live");
        assert_eq!(out.stream_key, "test");
    }
}
