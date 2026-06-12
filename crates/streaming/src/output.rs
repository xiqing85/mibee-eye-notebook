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
use tokio::sync::mpsc;

use crate::source::MediaFrame;
use anyhow::Result;
use protocols::h264;
use protocols::rtmp::{RtmpPushClient, build_video_nalus, build_video_sequence_header};

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
#[allow(dead_code)]
pub struct RtspOutput {
    /// Stream path (used as the RTSP mount point, e.g. "webcam").
    stream_path: String,
    /// SDP body describing the stream (codec, payload type, etc.).
    sdp_body: String,
    /// SSRC for RTP packets.
    ssrc: u32,
    /// Channel sender for pushing H.264 NAL data to the RTSP server.
    frame_tx: Option<mpsc::Sender<Vec<u8>>>,
    /// Whether the output has been started.
    started: bool,
}

impl RtspOutput {
    /// Create a new RTSP output without a channel (you must call
    /// [`with_channel`](RtspOutput::with_channel) to enable frame delivery).
    pub fn new(stream_path: &str, sdp_body: &str, ssrc: u32) -> Self {
        Self {
            stream_path: stream_path.to_string(),
            sdp_body: sdp_body.to_string(),
            ssrc,
            frame_tx: None,
            started: false,
        }
    }

    /// Create an RTSP output with a pre-registered channel sender.
    ///
    /// The sender is typically obtained from
    /// [`RtspServer::register_live_stream`](protocols::rtsp_server::RtspServer::register_live_stream).
    pub fn with_channel(
        stream_path: String,
        sdp_body: String,
        ssrc: u32,
        frame_tx: mpsc::Sender<Vec<u8>>,
    ) -> Self {
        Self {
            stream_path,
            sdp_body,
            ssrc,
            frame_tx: Some(frame_tx),
            started: false,
        }
    }
}

impl Output for RtspOutput {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if self.stream_path.is_empty() {
                anyhow::bail!("RtspOutput stream path must not be empty");
            }
            if self.frame_tx.is_none() {
                anyhow::bail!(
                    "RtspOutput has no channel -- use with_channel() or register a live stream"
                );
            }
            self.started = true;
            tracing::info!("RtspOutput started: /{}", self.stream_path);
            Ok(())
        })
    }

    fn send_frame(
        &mut self,
        frame: &MediaFrame,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        match frame {
            MediaFrame::Video { data, .. } => {
                let data = data.clone();
                Box::pin(async move {
                    if !self.started {
                        anyhow::bail!("RtspOutput not started");
                    }
                    match &self.frame_tx {
                        Some(tx) => {
                            tx.send(data)
                                .await
                                .map_err(|e| anyhow::anyhow!("RtspOutput send failed: {e}"))?;
                            Ok(())
                        }
                        None => {
                            anyhow::bail!("RtspOutput has no channel -- call with_channel()");
                        }
                    }
                })
            }
            MediaFrame::Audio { .. } => {
                // Audio frames not yet supported via RTSP; silently drop.
                Box::pin(async move { Ok(()) })
            }
        }
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.frame_tx = None;
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
/// Uses the native [RtmpPushClient] for protocol-level push without
/// external dependencies on ffmpeg.
///
/// Audio frames are silently dropped for now.
pub struct RtmpOutput {
    /// RTMP URL (e.g. `rtmp://localhost:1935/live/stream`).
    url: String,
    /// Application name extracted from URL.
    app_name: String,
    /// Stream key extracted from URL.
    stream_key: String,
    /// RTMP host extracted from URL.
    host: String,
    /// RTMP port extracted from URL.
    port: u16,
    /// Push client (connected on start).
    client: Option<RtmpPushClient>,
    /// Whether the output has been started.
    started: bool,
    /// Whether the AVC sequence header has been sent.
    seq_header_sent: bool,
}

impl RtmpOutput {
    /// Create a new RTMP output from a full RTMP URL.
    ///
    /// The URL format is: `rtmp://host:port/app/streamKey`
    pub fn new(url: &str) -> Self {
        let (app_name, stream_key) = Self::parse_rtmp_url(url);
        let (host, port) = Self::parse_rtmp_host_port(url);
        Self {
            url: url.to_string(),
            app_name,
            stream_key,
            host,
            port,
            client: None,
            started: false,
            seq_header_sent: false,
        }
    }

    /// Create an RTMP output with explicit host, port, app, and stream key.
    pub fn new_with_parts(host: &str, port: u16, app: &str, stream: &str) -> Self {
        let url = format!("rtmp://{host}:{port}/{app}/{stream}");
        Self {
            url,
            app_name: app.to_string(),
            stream_key: stream.to_string(),
            host: host.to_string(),
            port,
            client: None,
            started: false,
            seq_header_sent: false,
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

    fn parse_rtmp_host_port(url: &str) -> (String, u16) {
        let rest = url.trim_start_matches("rtmp://");
        let host_part = if let Some(slash_pos) = rest.find('/') {
            &rest[..slash_pos]
        } else {
            rest
        };
        if let Some(colon_pos) = host_part.find(':') {
            (
                host_part[..colon_pos].to_string(),
                host_part[colon_pos + 1..].parse::<u16>().unwrap_or(1935),
            )
        } else {
            (host_part.to_string(), 1935)
        }
    }
}

impl Output for RtmpOutput {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if self.url.is_empty() {
                anyhow::bail!("RtmpOutput URL must not be empty");
            }

            let mut client =
                RtmpPushClient::new(&self.host, self.port, &self.app_name, &self.stream_key);
            client.connect().await?;

            self.client = Some(client);
            self.started = true;
            self.seq_header_sent = false;
            tracing::info!("RtmpOutput started: {}", self.url);
            Ok(())
        })
    }

    fn send_frame(
        &mut self,
        frame: &MediaFrame,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        match frame {
            MediaFrame::Video {
                data,
                keyframe,
                timestamp,
            } => {
                let data = data.clone();
                let kf = *keyframe;
                let ts = *timestamp as u32;
                Box::pin(async move {
                    if !self.started {
                        anyhow::bail!("RtmpOutput not started");
                    }
                    let client = self
                        .client
                        .as_mut()
                        .ok_or_else(|| anyhow::anyhow!("RtmpOutput client not connected"))?;

                    // Parse NAL units from the frame data (handle Annex B or AVCC)
                    let nal_units = parse_h264_nal_units(&data);
                    if nal_units.is_empty() {
                        return Ok(());
                    }

                    // Send AVC sequence header on first keyframe
                    let need_seq_header = kf && !self.seq_header_sent;
                    if need_seq_header {
                        let sps = nal_units
                            .iter()
                            .find(|n| n.first().map(|b| b & 0x1F) == Some(7));
                        let pps = nal_units
                            .iter()
                            .find(|n| n.first().map(|b| b & 0x1F) == Some(8));
                        if let (Some(sps), Some(pps)) = (sps, pps) {
                            let seq_header = build_video_sequence_header(sps, pps);
                            client.send_video(&seq_header, ts).await?;
                        }
                        self.seq_header_sent = true;
                    }

                    // Re-encode as AVCC (4-byte length prefix per NAL unit)
                    let mut avcc_data = Vec::new();
                    for nal in &nal_units {
                        avcc_data.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                        avcc_data.extend_from_slice(nal);
                    }

                    // Build RTMP video payload and send
                    let rtmp_payload = build_video_nalus(&avcc_data, kf, 0);
                    client.send_video(&rtmp_payload, ts).await?;

                    Ok(())
                })
            }
            MediaFrame::Audio { .. } => Box::pin(async move {
                // Audio frames not yet supported via RTMP push;
                // silently drop for now.
                Ok(())
            }),
        }
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if let Some(mut client) = self.client.take() {
                let _ = client.close().await;
            }
            self.started = false;
            tracing::info!("RtmpOutput stopped");
            Ok(())
        })
    }
}

/// Parse H.264 data into individual NAL units, handling both Annex B and
/// AVCC formats.
fn parse_h264_nal_units(data: &[u8]) -> Vec<Vec<u8>> {
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
        let (tx, _rx) = mpsc::channel(16);
        let mut out = RtspOutput::with_channel(
            "webcam".to_string(),
            "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns=Test\r\nt=0 0\r\n".to_string(),
            0x1234,
            tx,
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
    async fn test_rtsp_output_no_channel_fails() {
        let mut out = RtspOutput::new("test", "s=Test", 1);
        // Without a channel, start should fail
        assert!(out.start().await.is_err());
    }

    #[tokio::test]
    async fn test_rtsp_output_send_video_frame() {
        let (tx, mut rx) = mpsc::channel(16);
        let mut out = RtspOutput::with_channel("test".to_string(), "s=Test".to_string(), 1, tx);
        out.start().await.unwrap();

        let frame = MediaFrame::Video {
            keyframe: true,
            data: vec![0x67, 0x42, 0x80],
            timestamp: 42,
        };
        out.send_frame(&frame).await.unwrap();

        // Verify data arrives on the channel
        let received = rx.recv().await.expect("Should receive data on channel");
        assert_eq!(received, vec![0x67, 0x42, 0x80]);

        out.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_rtsp_output_ignore_audio() {
        let (tx, mut rx) = mpsc::channel(16);
        let mut out = RtspOutput::with_channel("test".to_string(), "s=Test".to_string(), 1, tx);
        out.start().await.unwrap();

        // Audio frames should be silently dropped (no error, no data)
        let frame = MediaFrame::Audio {
            data: vec![0xFF; 160],
            timestamp: 100,
        };
        out.send_frame(&frame).await.unwrap();

        // Nothing should arrive on the channel
        let result = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await;
        assert!(result.is_err(), "No data should arrive for audio frames");

        out.stop().await.unwrap();
    }

    // ── RtmpOutput tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_rtmp_output_start_stop() {
        let mut out = RtmpOutput::new("rtmp://localhost:1935/live/stream");
        let result = out.start().await;
        // RTMP server may or may not be running; if start succeeded, verify stop.
        if result.is_ok() {
            assert!(out.started);
            out.stop().await.unwrap();
            assert!(!out.started);
        }
    }

    #[tokio::test]
    async fn test_rtmp_output_empty_url_fails() {
        let mut out = RtmpOutput::new("");
        assert!(out.start().await.is_err());
    }

    #[tokio::test]
    async fn test_rtmp_output_send_frame() {
        let mut out = RtmpOutput::new("rtmp://localhost:1935/live/stream");
        if out.start().await.is_err() {
            return; // RTMP server not available
        }
        // Send a video frame with Annex B H.264 data
        let frame = MediaFrame::Video {
            keyframe: true,
            data: vec![
                // Annex B start code + SPS NAL
                0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x80, 0x1E,
                // Annex B start code + PPS NAL
                0x00, 0x00, 0x00, 0x01, 0x68, 0xCE, 0x3C, 0x80,
            ],
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
    fn test_rtmp_host_port_parsing() {
        let (host, port) = RtmpOutput::parse_rtmp_host_port("rtmp://example.com:1935/live/stream");
        assert_eq!(host, "example.com");
        assert_eq!(port, 1935);
    }

    #[test]
    fn test_rtmp_host_port_parsing_default_port() {
        let (host, port) = RtmpOutput::parse_rtmp_host_port("rtmp://example.com/live/stream");
        assert_eq!(host, "example.com");
        assert_eq!(port, 1935);
    }
    #[test]
    fn test_rtmp_output_new_with_parts() {
        let out = RtmpOutput::new_with_parts("localhost", 1935, "live", "test");
        assert_eq!(out.url, "rtmp://localhost:1935/live/test");
        assert_eq!(out.app_name, "live");
        assert_eq!(out.stream_key, "test");
        assert_eq!(out.host, "localhost");
        assert_eq!(out.port, 1935);
    }
}
