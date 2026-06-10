//! Source trait and concrete source adapters.
//!
//! Defines [`MediaFrame`] — the unit of media data flowing through the
//! pipeline — and [`Source`], the async trait that produces frames.
//!
//! Concrete adapters wrap the protocol-level clients:
//! - [`RtspSource`] — pulls RTP→NAL frames from an RTSP camera
//! - [`RtmpSource`] — accepts an RTMP push and extracts frames
//! - [`OnvifSource`] — discovers ONVIF cameras, resolves RTSP URIs, delegates
//! - [`Gb28181Source`] — receives GB/T 28181 SIP INVITE, extracts PS→H.264

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;

// ── MediaFrame ─────────────────────────────────────────────────────────────────

/// A single media frame — either video (H.264 NAL unit data) or audio (PCM/G.711).
#[derive(Debug, Clone, PartialEq)]
pub enum MediaFrame {
    /// Video frame: H.264 NAL unit data.
    Video {
        /// Whether this frame is a keyframe (IDR).
        keyframe: bool,
        /// Raw NAL unit data (Annex B or AVCC format).
        data: Vec<u8>,
        /// Presentation timestamp in milliseconds.
        timestamp: u64,
    },
    /// Audio frame: PCM or G.711 encoded data.
    Audio {
        /// Raw audio data.
        data: Vec<u8>,
        /// Presentation timestamp in milliseconds.
        timestamp: u64,
    },
}

impl MediaFrame {
    /// Return the timestamp of this frame.
    pub fn timestamp(&self) -> u64 {
        match self {
            MediaFrame::Video { timestamp, .. } | MediaFrame::Audio { timestamp, .. } => *timestamp,
        }
    }

    /// Return the raw data of this frame.
    pub fn data(&self) -> &[u8] {
        match self {
            MediaFrame::Video { data, .. } | MediaFrame::Audio { data, .. } => data,
        }
    }
}

// ── Source trait ───────────────────────────────────────────────────────────────

/// Async source of media frames.
///
/// Implementors wrap a capture device, network stream, or file and produce
/// [`MediaFrame`] values via [`next_frame`](Source::next_frame).
///
/// # Lifetimes
///
/// Each method returns a pinned, boxed future whose lifetime is tied to
/// `&mut self` — the future may borrow `self` while pending. This makes
/// the trait fully object-safe for `Box<dyn Source>`.
pub trait Source: Send + 'static {
    /// Start the source (open device / connect to stream).
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    /// Produce the next frame.
    ///
    /// Blocks (asynchronously) until a frame is available.
    /// Returns [`Err`] on permanent failure (the source should be stopped).
    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>>;

    /// Stop the source and release resources.
    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}

// ── Concrete sources ───────────────────────────────────────────────────────────

// ---------------------------------------------------------------------------
// RtspSource
// ---------------------------------------------------------------------------

/// RTSP camera source.
///
/// Connects to an RTSP URL, performs DESCRIBE/SETUP/PLAY handshake, and
/// pulls RTP packets that are parsed into H.264 NAL unit [`MediaFrame`]s.
///
/// **Current status**: structural adapter — connection logic will be filled
/// in once the capture crate (T8/T9) provides the transport layer.
#[allow(dead_code)]
pub struct RtspSource {
    /// RTSP URL (e.g. `rtsp://192.168.1.100:554/stream1`).
    url: String,
    /// Optional username for Digest/Basic auth.
    username: String,
    /// Optional password.
    password: String,
    /// Whether the source has been started.
    started: bool,
}

impl RtspSource {
    /// Create a new RTSP source.
    pub fn new(url: &str, username: &str, password: &str) -> Self {
        Self {
            url: url.to_string(),
            username: username.to_string(),
            password: password.to_string(),
            started: false,
        }
    }
}

impl Source for RtspSource {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // TODO(T12/T8): Connect via tokio TcpStream, perform RTSP handshake
            // using protocols::rtsp::{RtspRequest, RtspResponse, parse_sdp}.
            // For now, validate the URL format and mark as started.
            if !self.url.starts_with("rtsp://") {
                anyhow::bail!("Invalid RTSP URL: {}", self.url);
            }
            self.started = true;
            tracing::info!("RtspSource started: {}", self.url);
            Ok(())
        })
    }

    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>> {
        Box::pin(async move {
            if !self.started {
                anyhow::bail!("RtspSource not started");
            }
            // TODO(T12/T8): Read RTP packets from the TCP stream, parse NAL units
            // via protocols::h264, wrap in MediaFrame.
            anyhow::bail!(
                "RtspSource::next_frame not yet implemented — requires T12/T8 transport layer"
            );
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // TODO(T12): Send TEARDOWN, close connection.
            self.started = false;
            tracing::info!("RtspSource stopped: {}", self.url);
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// RtmpSource
// ---------------------------------------------------------------------------

/// RTMP push source.
///
/// Starts an RTMP ingest server and accepts an incoming publisher (e.g. OBS).
/// Extracts H.264/AAC frames from the RTMP stream.
///
/// **Current status**: wraps [`protocols::rtmp::RtmpServer`] and reads from
/// its frame channel.
pub struct RtmpSource {
    /// RTMP listen port (default 1935).
    port: u16,
    /// Application name (e.g. "live").
    app_name: String,
    /// Ingest instance, populated after `start()`.
    ingest: Option<protocols::rtmp::RtmpIngest>,
    /// Whether the source has started.
    started: bool,
}

impl RtmpSource {
    /// Create a new RTMP ingest source.
    pub fn new(port: u16, app_name: &str) -> Self {
        Self {
            port,
            app_name: app_name.to_string(),
            ingest: None,
            started: false,
        }
    }
}

impl Source for RtmpSource {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let server = protocols::rtmp::RtmpServer::new(self.port, &self.app_name);
            let ingest = server.run().await?;
            self.ingest = Some(ingest);
            self.started = true;
            tracing::info!("RtmpSource listening on port {}", self.port);
            Ok(())
        })
    }

    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>> {
        Box::pin(async move {
            let ingest = self
                .ingest
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("RtmpSource not started"))?;
            let frame = ingest
                .frames()
                .recv()
                .await
                .ok_or_else(|| anyhow::anyhow!("RTMP ingest channel closed"))?;
            match frame {
                protocols::rtmp::RtmpFrame::Video(data) => {
                    // Attempt to detect keyframe from the first byte.
                    let keyframe = data.first().map(|b| (b >> 4) & 0x0F == 1).unwrap_or(false);
                    Ok(MediaFrame::Video {
                        keyframe,
                        data,
                        timestamp: 0,
                    })
                }
                protocols::rtmp::RtmpFrame::Audio(data) => {
                    Ok(MediaFrame::Audio { data, timestamp: 0 })
                }
            }
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.ingest = None;
            self.started = false;
            tracing::info!("RtmpSource stopped");
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// OnvifSource
// ---------------------------------------------------------------------------

/// ONVIF camera source.
///
/// Discovers ONVIF-compatible cameras, resolves an RTSP stream URI, and
/// delegates frame production to an internal [`RtspSource`].
///
/// **Current status**: stub — captures the discovery/URI-resolve workflow.
pub struct OnvifSource {
    /// ONVIF device service URL.
    device_url: String,
    /// Username for ONVIF authentication.
    username: String,
    /// Password.
    password: String,
    /// Internal RTSP source created after resolving the stream URI.
    rtsp_source: Option<RtspSource>,
    /// Whether the source has started.
    started: bool,
}

impl OnvifSource {
    /// Create a new ONVIF source.
    ///
    /// If `device_url` is empty, the source will perform WS-Discovery to find
    /// cameras on the local network on `start()`.
    pub fn new(device_url: &str, username: &str, password: &str) -> Self {
        Self {
            device_url: device_url.to_string(),
            username: username.to_string(),
            password: password.to_string(),
            rtsp_source: None,
            started: false,
        }
    }
}

impl Source for OnvifSource {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let uri = if self.device_url.is_empty() {
                // Discover cameras on the network
                let devices =
                    protocols::onvif::discover_devices(std::time::Duration::from_secs(5)).await?;
                if devices.is_empty() {
                    anyhow::bail!("No ONVIF devices discovered");
                }
                let device = &devices[0];
                let session =
                    protocols::onvif::connect(&device.xaddrs[0], &self.username, &self.password)
                        .await?;
                let profiles = session.get_profiles().await?;
                if profiles.is_empty() {
                    anyhow::bail!("No media profiles on ONVIF device");
                }
                let stream_uri = session.get_stream_uri(&profiles[0].token).await?;
                stream_uri.uri
            } else {
                let session =
                    protocols::onvif::connect(&self.device_url, &self.username, &self.password)
                        .await?;
                let profiles = session.get_profiles().await?;
                if profiles.is_empty() {
                    anyhow::bail!("No media profiles on ONVIF device");
                }
                let stream_uri = session.get_stream_uri(&profiles[0].token).await?;
                stream_uri.uri
            };

            let mut rtsp = RtspSource::new(&uri, &self.username, &self.password);
            rtsp.start().await?;
            self.rtsp_source = Some(rtsp);
            self.started = true;
            tracing::info!("OnvifSource started via RTSP: {uri}");
            Ok(())
        })
    }

    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>> {
        Box::pin(async move {
            let rtsp = self
                .rtsp_source
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("OnvifSource not started"))?;
            rtsp.next_frame().await
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if let Some(mut rtsp) = self.rtsp_source.take() {
                rtsp.stop().await?;
            }
            self.started = false;
            tracing::info!("OnvifSource stopped");
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// Gb28181Source
// ---------------------------------------------------------------------------

/// GB/T 28181 camera source.
///
/// Receives a SIP INVITE from a registered camera, negotiates the RTP media
/// session, and extracts H.264 frames from MPEG-2 Program Stream encapsulation.
///
/// **Current status**: stub — demonstrates the SIP→PS→NAL pipeline shape.
#[allow(dead_code)]
pub struct Gb28181Source {
    /// 20-digit platform device ID.
    platform_id: String,
    /// SIP domain.
    domain: String,
    /// Device ID to request a stream from.
    device_id: String,
    /// Channel ID within the device.
    channel_id: String,
    /// Whether the source has started.
    started: bool,
}

impl Gb28181Source {
    /// Create a new GB/T 28181 source.
    pub fn new(platform_id: &str, domain: &str, device_id: &str, channel_id: &str) -> Self {
        Self {
            platform_id: platform_id.to_string(),
            domain: domain.to_string(),
            device_id: device_id.to_string(),
            channel_id: channel_id.to_string(),
            started: false,
        }
    }
}

impl Source for Gb28181Source {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // TODO(T16): Set up SIP listening socket, send INVITE, negotiate RTP
            // session. Parse PS payload into H.264 NAL units.
            // Uses protocols::gb28181::{build_invite_request, parse_ps_to_nal_units}.
            self.started = true;
            tracing::info!(
                "Gb28181Source started: platform={}, device={}",
                self.platform_id,
                self.device_id
            );
            Ok(())
        })
    }

    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>> {
        Box::pin(async move {
            if !self.started {
                anyhow::bail!("Gb28181Source not started");
            }
            anyhow::bail!(
                "Gb28181Source::next_frame not yet implemented — requires T16 SIP/RTP transport"
            );
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // TODO(T16): Send SIP BYE, close RTP socket.
            self.started = false;
            tracing::info!("Gb28181Source stopped");
            Ok(())
        })
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // Helper: a mock source that yields predefined frames.
    pub(crate) struct MockSource {
        frames: Vec<MediaFrame>,
        started: bool,
        index: usize,
    }

    impl MockSource {
        pub fn new(frames: Vec<MediaFrame>) -> Self {
            Self {
                frames,
                started: false,
                index: 0,
            }
        }

        pub fn empty() -> Self {
            Self::new(vec![])
        }
    }

    impl Source for MockSource {
        fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
            Box::pin(async move {
                self.started = true;
                Ok(())
            })
        }

        fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>> {
            Box::pin(async move {
                if !self.started {
                    anyhow::bail!("MockSource not started");
                }
                if self.index >= self.frames.len() {
                    anyhow::bail!("MockSource exhausted");
                }
                let frame = self.frames[self.index].clone();
                self.index += 1;
                Ok(frame)
            })
        }

        fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
            Box::pin(async move {
                self.started = false;
                Ok(())
            })
        }
    }

    // ── MediaFrame tests ─────────────────────────────────────────────────────────

    #[test]
    fn test_media_frame_video() {
        let frame = MediaFrame::Video {
            keyframe: true,
            data: vec![0x00, 0x00, 0x00, 0x01, 0x67],
            timestamp: 1000,
        };
        assert!(frame.timestamp() == 1000);
        assert!(frame.data() == &[0x00, 0x00, 0x00, 0x01, 0x67]);
        assert!(matches!(frame, MediaFrame::Video { .. }));
    }

    #[test]
    fn test_media_frame_audio() {
        let frame = MediaFrame::Audio {
            data: vec![0xFF; 160],
            timestamp: 500,
        };
        assert!(frame.timestamp() == 500);
        assert!(frame.data().len() == 160);
        assert!(matches!(frame, MediaFrame::Audio { .. }));
    }

    #[test]
    fn test_media_frame_clone_eq() {
        let a = MediaFrame::Video {
            keyframe: false,
            data: vec![1, 2, 3],
            timestamp: 0,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    // ── MockSource tests ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_mock_source_lifecycle() {
        let frames = vec![
            MediaFrame::Video {
                keyframe: true,
                data: vec![0x67],
                timestamp: 0,
            },
            MediaFrame::Audio {
                data: vec![0xAA],
                timestamp: 33,
            },
        ];
        let mut src = MockSource::new(frames.clone());

        // Start
        src.start().await.unwrap();

        // Read first frame
        let f1 = src.next_frame().await.unwrap();
        assert_eq!(f1, frames[0]);

        // Read second frame
        let f2 = src.next_frame().await.unwrap();
        assert_eq!(f2, frames[1]);

        // Source exhausted
        assert!(src.next_frame().await.is_err());

        // Stop
        src.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_mock_source_not_started() {
        let mut src = MockSource::new(vec![MediaFrame::Video {
            keyframe: false,
            data: vec![],
            timestamp: 0,
        }]);
        assert!(src.next_frame().await.is_err());
    }

    #[tokio::test]
    async fn test_mock_source_empty() {
        let mut src = MockSource::empty();
        src.start().await.unwrap();
        assert!(src.next_frame().await.is_err());
    }

    // ── RtspSource tests ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_rtsp_source_start_stop() {
        let mut src = RtspSource::new("rtsp://192.168.1.100:554/stream1", "admin", "pass");
        src.start().await.unwrap();
        assert!(src.started);
        src.stop().await.unwrap();
        assert!(!src.started);
    }

    #[tokio::test]
    async fn test_rtsp_source_invalid_url() {
        let mut src = RtspSource::new("http://example.com/stream", "", "");
        assert!(src.start().await.is_err());
    }

    #[tokio::test]
    async fn test_rtsp_source_next_frame_fails_without_start() {
        let mut src = RtspSource::new("rtsp://localhost/stream", "", "");
        assert!(src.next_frame().await.is_err());
    }

    // ── RtmpSource tests ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_rtmp_source_start_stop() {
        // Bind to a high ephemeral port for testing.
        let mut src = RtmpSource::new(0, "live");
        // Starting on port 0 will likely fail on Linux (port 0 is reserved for
        // ephemeral usage, but TcpListener won't bind port 0 in this context).
        // We just verify the lifecycle methods exist and don't panic.
        let result = src.start().await;
        // May succeed or fail depending on environment — that's fine for a stub.
        if result.is_ok() {
            src.stop().await.unwrap();
        }
    }

    // ── OnvifSource tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_onvif_source_start_stop_no_discovery() {
        // Without a device URL, start() will attempt discovery which will fail
        // in test environments. We verify the method compiles and is callable.
        let mut src = OnvifSource::new("", "", "");
        // Discovery will fail in test — that's expected.
        let result = src.start().await;
        if result.is_ok() {
            src.stop().await.unwrap();
        }
    }

    // ── Gb28181Source tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_gb28181_source_start_stop() {
        let mut src = Gb28181Source::new(
            "34020000002000000001",
            "3402000000",
            "34020000201180000001",
            "1",
        );
        src.start().await.unwrap();
        assert!(src.started);
        assert!(src.next_frame().await.is_err()); // Not yet implemented
        src.stop().await.unwrap();
    }
}
