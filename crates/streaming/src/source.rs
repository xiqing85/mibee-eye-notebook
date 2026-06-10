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

use std::collections::HashMap;
use std::net::SocketAddr;

use std::future::Future;
use std::pin::Pin;

use anyhow::{anyhow, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use protocols::rtsp::{RtspRequest, RtspResponse, RtspMethod, RtspAuth, parse_sdp};
use protocols::rtp::{RtpPacket, is_single_nal, is_stapa, is_fua, parse_stapa, parse_fua_indicator, parse_fua_header};
use protocols::h264::{parse_nal_header, is_keyframe};

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

pub struct RtspSource {
    /// RTSP URL (e.g. `rtsp://192.168.1.100:554/stream1`).
    url: String,
    /// Optional username for Digest/Basic auth.
    username: String,
    /// Optional password.
    password: String,
    /// Whether the source has been started.
    started: bool,
    /// TCP connection to the RTSP server.
    tcp_stream: Option<tokio::net::TcpStream>,
    /// CSeq counter for RTSP requests.
    cseq: u32,
    /// RTSP session ID from SETUP response.
    session_id: Option<String>,
    /// Authentication state (populated after 401 challenge).
    auth: Option<RtspAuth>,
    /// Interleaved RTP channel number (typically 0).
    rtp_channel: u8,
    /// FU-A reassembly state: NAL reference IDC.
    fu_nal_ref_idc: u8,
    /// FU-A reassembly state: NAL unit type.
    fu_nal_unit_type: u8,
    /// FU-A reassembly state: accumulated data (without NAL header).
    fu_data: Vec<u8>,
    /// Queue of assembled NAL frames ready to yield.
    nal_queue: Vec<MediaFrame>,
    /// Buffer for partially read TCP data.
    read_buf: Vec<u8>,
}

impl RtspSource {
    /// Create a new RTSP source.
    pub fn new(url: &str, username: &str, password: &str) -> Self {
        Self {
            url: url.to_string(),
            username: username.to_string(),
            password: password.to_string(),
            started: false,
            tcp_stream: None,
            cseq: 0,
            session_id: None,
            auth: None,
            rtp_channel: 0,
            fu_nal_ref_idc: 0,
            fu_nal_unit_type: 0,
            fu_data: Vec::new(),
            nal_queue: Vec::new(),
            read_buf: Vec::new(),
        }
    }

    /// Parse an RTSP URL into (host, port, path).
    fn parse_url(&self) -> Result<(String, u16, String)> {
        let rest = self.url
            .strip_prefix("rtsp://")
            .ok_or_else(|| anyhow!("Invalid RTSP URL: {}", self.url))?;

        let (host_part, path) = match rest.split_once('/') {
            Some((h, p)) => (h, format!("/{}", p)),
            None => (rest, String::from("/")),
        };

        // Strip user:pass@ if present
        let host = match host_part.rsplit_once('@') {
            Some((_, h)) => h,
            None => host_part,
        };

        let (host, port) = match host.split_once(':') {
            Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(554)),
            None => (host.to_string(), 554u16),
        };

        Ok((host, port, path))
    }

    /// Resolve a control URL from an SDP media description attribute.
    fn resolve_url(&self, control: &str) -> String {
        if control.starts_with("rtsp://") {
            return control.to_string();
        }
        if control.starts_with('/') {
            // Relative to host
            let host = self.url.trim_start_matches("rtsp://");
            let host = host.split('/').next().unwrap_or("");
            let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
            return format!("rtsp://{host}{control}");
        }
        // Relative to base URL
        let base = self.url.trim_end_matches('/');
        format!("{base}/{control}")
    }

    /// Serialize, write, and read a complete RTSP response.
    async fn send_request(&mut self, request: &RtspRequest) -> Result<RtspResponse> {
        let stream = self.tcp_stream.as_mut()
            .ok_or_else(|| anyhow!("TCP stream not connected"))?;

        let data = request.serialize();
        stream.write_all(&data).await?;

        self.read_response().await
    }

    /// Read a complete RTSP response from the TCP stream.
    async fn read_response(&mut self) -> Result<RtspResponse> {
        let stream = self.tcp_stream.as_mut()
            .ok_or_else(|| anyhow!("TCP stream not connected"))?;

        let mut buf = [0u8; 8192];
        loop {
            match RtspResponse::parse(&self.read_buf) {
                Ok((resp, consumed)) => {
                    self.read_buf.drain(..consumed);
                    return Ok(resp);
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("Truncated body") || msg.contains("No header terminator") {
                        let n = stream.read(&mut buf).await?;
                        if n == 0 {
                            anyhow::bail!("RTSP connection closed while reading response");
                        }
                        self.read_buf.extend_from_slice(&buf[..n]);
                    } else {
                        return Err(e);
                    }
                }
            }
        }
    }

    /// Read one interleaved $-framed packet from the TCP stream.
    async fn read_interleaved(&mut self) -> Result<(u8, Vec<u8>)> {
        let stream = self.tcp_stream.as_mut()
            .ok_or_else(|| anyhow!("TCP stream not connected"))?;

        let mut scratch = [0u8; 65536];
        loop {
            // Ensure we have data in read_buf
            if self.read_buf.is_empty() {
                let n = stream.read(&mut scratch).await?;
                if n == 0 {
                    anyhow::bail!("RTSP connection closed");
                }
                self.read_buf.extend_from_slice(&scratch[..n]);
            }

            // Find $ marker (0x24)
            if let Some(pos) = self.read_buf.iter().position(|&b| b == 0x24) {
                // Need 4 bytes for marker + channel + length
                if self.read_buf.len() < pos + 4 {
                    let n = stream.read(&mut scratch).await?;
                    if n == 0 { anyhow::bail!("RTSP connection closed"); }
                    self.read_buf.extend_from_slice(&scratch[..n]);
                    continue;
                }

                let channel = self.read_buf[pos + 1];
                let length = u16::from_be_bytes([self.read_buf[pos + 2], self.read_buf[pos + 3]]) as usize;
                let packet_end = pos + 4 + length;

                if self.read_buf.len() < packet_end {
                    let n = stream.read(&mut scratch).await?;
                    if n == 0 { anyhow::bail!("RTSP connection closed"); }
                    self.read_buf.extend_from_slice(&scratch[..n]);
                    continue;
                }

                let data = self.read_buf[pos + 4..packet_end].to_vec();
                self.read_buf.drain(..packet_end);
                return Ok((channel, data));
            }

            // No $ marker found. Check for leftover RTSP response data.
            if self.read_buf.starts_with(b"RTSP") {
                if let Ok((resp, consumed)) = RtspResponse::parse(&self.read_buf) {
                    tracing::warn!("Unexpected RTSP response in interleaved stream: {} {}", resp.status_code, resp.reason);
                    self.read_buf.drain(..consumed);
                    continue;
                }
            }

            // Buffer too large without finding $: clear and retry
            if self.read_buf.len() > 65536 {
                self.read_buf.clear();
            }
            let n = stream.read(&mut scratch).await?;
            if n == 0 { anyhow::bail!("RTSP connection closed"); }
            self.read_buf.extend_from_slice(&scratch[..n]);
        }
    }

    /// Process an RTP payload, extracting H.264 NAL units and pushing to nal_queue.
    fn process_rtp_payload(&mut self, payload: &[u8], timestamp: u32) -> Result<()> {
        if payload.is_empty() {
            return Ok(());
        }

        if is_single_nal(payload) {
            let (header, _) = parse_nal_header(payload)?;
            let keyframe = is_keyframe(header.nal_unit_type, header.nal_ref_idc);
            self.nal_queue.push(MediaFrame::Video {
                keyframe,
                data: payload.to_vec(),
                timestamp: timestamp as u64,
            });
        } else if is_stapa(payload) {
            let nals = parse_stapa(payload)?;
            for nal in nals {
                if nal.is_empty() {
                    continue;
                }
                if let Ok((header, _)) = parse_nal_header(&nal) {
                    let keyframe = is_keyframe(header.nal_unit_type, header.nal_ref_idc);
                    self.nal_queue.push(MediaFrame::Video {
                        keyframe,
                        data: nal,
                        timestamp: timestamp as u64,
                    });
                }
            }
        } else if is_fua(payload) {
            if payload.len() < 2 {
                return Ok(());
            }
            let indicator = parse_fua_indicator(payload[0]);
            let header = parse_fua_header(payload[1]);

            if header.start {
                // Start new FU-A sequence
                self.fu_nal_ref_idc = indicator.nal_ref_idc;
                self.fu_nal_unit_type = header.nal_unit_type;
                self.fu_data.clear();
                self.fu_data.extend_from_slice(&payload[2..]);
            } else if !self.fu_data.is_empty() {
                self.fu_data.extend_from_slice(&payload[2..]);

                if header.end {
                    // Reconstruct the NAL header byte
                    let nal_header_byte = (self.fu_nal_ref_idc << 5) | self.fu_nal_unit_type;
                    let mut nal = vec![nal_header_byte];
                    nal.append(&mut self.fu_data);

                    if let Ok((parsed_header, _)) = parse_nal_header(&nal) {
                        let keyframe = is_keyframe(parsed_header.nal_unit_type, parsed_header.nal_ref_idc);
                        self.nal_queue.push(MediaFrame::Video {
                            keyframe,
                            data: nal,
                            timestamp: timestamp as u64,
                        });
                    }
                    self.fu_data.clear();
                }
            }
            // If not start and fu_data is empty, we missed the start fragment - ignore
        }

        Ok(())
    }
}

impl Source for RtspSource {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if !self.url.starts_with("rtsp://") {
                anyhow::bail!("Invalid RTSP URL: {}", self.url);
            }

            let (host, port, path) = self.parse_url()?;

            // Connect
            let addr = format!("{}:{}", host, port);
            let stream = tokio::net::TcpStream::connect(&addr).await
                .map_err(|e| anyhow!("Failed to connect to {addr}: {e}"))?;
            self.tcp_stream = Some(stream);

            // Build the full base URL for requests
            let base_url = if port == 554 {
                format!("rtsp://{}{}", host, path)
            } else {
                format!("rtsp://{}:{}{}", host, port, path)
            };

            // ── DESCRIBE ─────────────────────────────────────────────────
            self.cseq += 1;
            let mut describe = RtspRequest::new(RtspMethod::Describe, &base_url);
            describe.set_cseq(self.cseq);
            describe.add_header("Accept", "application/sdp");
            if let Some(ref auth) = self.auth {
                if let Some(hdr) = auth.authorization_header("DESCRIBE", &base_url) {
                    describe.add_header("Authorization", &hdr);
                }
            }

            let response = self.send_request(&describe).await?;

            // Handle 401 auth challenge
            let sdp_body = if response.status_code == 401 {
                let www_auth = response.get_header("WWW-Authenticate")
                    .ok_or_else(|| anyhow!("401 response without WWW-Authenticate header"))?;
                self.auth = Some(RtspAuth::from_www_authenticate(www_auth, &self.username, &self.password)?);

                self.cseq += 1;
                let mut describe2 = RtspRequest::new(RtspMethod::Describe, &base_url);
                describe2.set_cseq(self.cseq);
                describe2.add_header("Accept", "application/sdp");
                if let Some(ref auth) = self.auth {
                    if let Some(hdr) = auth.authorization_header("DESCRIBE", &base_url) {
                        describe2.add_header("Authorization", &hdr);
                    }
                }

                let resp2 = self.send_request(&describe2).await?;
                if resp2.status_code != 200 {
                    anyhow::bail!("DESCRIBE failed after auth: {} {}", resp2.status_code, resp2.reason);
                }
                resp2.body
            } else if response.status_code == 200 {
                response.body
            } else {
                anyhow::bail!("DESCRIBE failed: {} {}", response.status_code, response.reason);
            };

            // ── Parse SDP ────────────────────────────────────────────────
            let sdp_str = std::str::from_utf8(&sdp_body)
                .map_err(|e| anyhow!("Invalid UTF-8 in SDP body: {e}"))?;
            let sdp = parse_sdp(sdp_str)
                .map_err(|e| anyhow!("Failed to parse SDP: {e}"))?;

            // Find the first video track
            let video_track = sdp.media_descriptions.iter()
                .find(|md| md.media_type == "video")
                .ok_or_else(|| anyhow!("No video track found in SDP"))?;

            // Resolve the control URL for SETUP
            let control = video_track.control.as_deref().unwrap_or("track1");
            let setup_url = self.resolve_url(control);

            tracing::debug!("RTSP DESCRIBE OK: video track control={}", setup_url);

            // ── SETUP ────────────────────────────────────────────────────
            self.cseq += 1;
            let mut setup = RtspRequest::new(RtspMethod::Setup, &setup_url);
            setup.set_cseq(self.cseq);
            setup.add_header("Transport", "RTP/AVP/TCP;interleaved=0-1");
            if let Some(ref auth) = self.auth {
                if let Some(hdr) = auth.authorization_header("SETUP", &setup_url) {
                    setup.add_header("Authorization", &hdr);
                }
            }

            let setup_resp = self.send_request(&setup).await?;
            if setup_resp.status_code != 200 {
                anyhow::bail!("SETUP failed: {} {}", setup_resp.status_code, setup_resp.reason);
            }

            // Extract session ID (format: "12345678" or "12345678;timeout=60")
            let session_hdr = setup_resp.get_header("Session")
                .ok_or_else(|| anyhow!("SETUP response missing Session header"))?;
            self.session_id = Some(
                session_hdr.split(';').next().unwrap_or(session_hdr).to_string()
            );

            // Extract interleaved channel from Transport header
            let transport_hdr = setup_resp.get_header("Transport")
                .ok_or_else(|| anyhow!("SETUP response missing Transport header"))?;
            let transport_info = protocols::rtsp::TransportInfo::parse(transport_hdr)?;
            self.rtp_channel = transport_info.interleaved.map(|(r, _)| r).unwrap_or(0);

            tracing::debug!("RTSP SETUP OK: session={}, channel={}",
                self.session_id.as_deref().unwrap_or("?"),
                self.rtp_channel
            );

            // ── PLAY ─────────────────────────────────────────────────────
            self.cseq += 1;
            let mut play = RtspRequest::new(RtspMethod::Play, &base_url);
            play.set_cseq(self.cseq);
            play.add_header("Session", self.session_id.as_deref().unwrap_or(""));
            play.add_header("Range", "npt=0.000-");
            if let Some(ref auth) = self.auth {
                if let Some(hdr) = auth.authorization_header("PLAY", &base_url) {
                    play.add_header("Authorization", &hdr);
                }
            }

            let play_resp = self.send_request(&play).await?;
            if play_resp.status_code != 200 {
                anyhow::bail!("PLAY failed: {} {}", play_resp.status_code, play_resp.reason);
            }

            tracing::info!("RTSP PLAY OK: {}", self.url);
            self.started = true;
            Ok(())
        })
    }

    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>> {
        Box::pin(async move {
            if !self.started {
                anyhow::bail!("RtspSource not started");
            }

            loop {
                // Check queue first
                if let Some(frame) = self.nal_queue.pop() {
                    return Ok(frame);
                }

                // Read interleaved data
                let (channel, data) = self.read_interleaved().await?;

                // Skip RTCP (typically channel 1)
                if channel != self.rtp_channel {
                    continue;
                }

                // Parse RTP packet
                let rtp = RtpPacket::parse(&data)
                    .map_err(|e| anyhow!("Failed to parse RTP packet: {e}"))?;

                // Process payload and queue frames
                self.process_rtp_payload(&rtp.payload, rtp.timestamp)?;
            }
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // Send TEARDOWN if we have an active session
            if let Some(ref session_id) = self.session_id {
                if let Some(ref mut stream) = self.tcp_stream {
                    self.cseq += 1;
                    let mut teardown = RtspRequest::new(RtspMethod::Teardown, &self.url);
                    teardown.set_cseq(self.cseq);
                    teardown.add_header("Session", session_id);
                    if let Some(ref auth) = self.auth {
                        if let Some(hdr) = auth.authorization_header("TEARDOWN", &self.url) {
                            teardown.add_header("Authorization", &hdr);
                        }
                    }

                    let data = teardown.serialize();
                    let _ = stream.write_all(&data).await;
                }
            }

            // Close the TCP stream and reset state
            self.tcp_stream = None;
            self.session_id = None;
            self.started = false;
            self.nal_queue.clear();
            self.fu_data.clear();
            self.read_buf.clear();

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
/// **Current status**: implemented — discovers ONVIF cameras, resolves RTSP URI, delegates to [`RtspSource`].
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
/// **Current status**: structural adapter — SIP signaling via [`Gb28181Platform`],
/// UDP RTP reception, PS→H.264 extraction, and [`MediaFrame`] production are
/// implemented. Requires an already-registered device in the platform registry.
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
    /// UDP socket for receiving RTP packets from the camera.
    udp_socket: Option<tokio::net::UdpSocket>,
    /// Expected SSRC for packet filtering.
    ssrc: Option<u32>,
    /// GB/T 28181 platform instance used for SIP signaling.
    platform: Option<protocols::gb28181::Gb28181Platform>,
    /// Unique SIP Call-ID for the INVITE session.
    call_id: Option<String>,
    /// Queue of parsed [`MediaFrame`]s ready for consumption.
    nal_queue: Vec<MediaFrame>,
    /// Tracks seen RTP sequence numbers for duplicate detection.
    sequence_map: HashMap<u16, bool>,
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
            udp_socket: None,
            ssrc: None,
            platform: None,
            call_id: None,
            nal_queue: Vec::new(),
            sequence_map: HashMap::new(),
        }
    }
}

impl Source for Gb28181Source {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // 1. Create a Gb28181Platform for SIP signaling
            let sip_addr: SocketAddr = "0.0.0.0:5060".parse()?;
            let mut platform = protocols::gb28181::Gb28181Platform::new(
                sip_addr,
                &self.platform_id,
                &self.domain,
            );

            // 2. Send INVITE via invite_preview() to initiate the RTP stream
            //    In production, the device must have already registered via SIP REGISTER.
            let stream_info = platform
                .invite_preview(&self.device_id, &self.channel_id)
                .await?;

            // 3. Create a UDP socket bound to an ephemeral port
            let bind_addr: SocketAddr = "0.0.0.0:0".parse()?;
            let socket = tokio::net::UdpSocket::bind(bind_addr).await?;

            // 4. Connect the UDP socket to the remote RTP address
            let remote_port = if stream_info.remote_port > 0 {
                stream_info.remote_port
            } else {
                10000 // Default RTP port for GB/T 28181
            };
            let remote_addr: SocketAddr =
                format!("{}:{}", stream_info.remote_addr, remote_port).parse()?;
            socket.connect(remote_addr).await?;

            // 5. Generate a unique Call-ID for this session
            let call_id = format!("gb28181-{}", uuid::Uuid::new_v4());

            // 6. Register the stream in the platform state
            //    Must clone here to retain stream_info for our own state
            platform.add_stream(call_id.clone(), stream_info.clone());

            // 7. Store everything
            self.udp_socket = Some(socket);
            self.ssrc = Some(stream_info.ssrc);
            self.platform = Some(platform);
            self.call_id = Some(call_id);
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

            // Return queued frames first
            if let Some(frame) = self.nal_queue.pop() {
                return Ok(frame);
            }

            let ssrc_check = self.ssrc;
            let mut buf = [0u8; 65535];

            // Take the socket out of self to avoid borrow issues across .await
            let socket = match self.udp_socket.take() {
                Some(s) => s,
                None => anyhow::bail!("Gb28181Source: UDP socket not available"),
            };

            loop {
                // 1. Read an RTP packet from the UDP socket
                let n = match socket.recv(&mut buf).await {
                    Ok(n) if n >= 12 => n,
                    Ok(_) => continue, // Too short for an RTP header
                    Err(e) => {
                        self.udp_socket = Some(socket);
                        anyhow::bail!("Gb28181Source: UDP recv error: {}", e);
                    }
                };

                // 2. Parse as an RTP packet
                let rtp = match protocols::rtp::RtpPacket::parse(&buf[..n]) {
                    Ok(p) => p,
                    Err(_) => continue,
                };

                // 3. Verify SSRC matches what we expect
                if let Some(expected) = ssrc_check {
                    if rtp.ssrc != expected {
                        continue;
                    }
                }

                // 4. Check for duplicate sequence numbers
                if self.sequence_map.contains_key(&rtp.sequence_number) {
                    continue;
                }
                self.sequence_map.insert(rtp.sequence_number, true);

                // 5. Parse the PS (Program Stream) payload into H.264 NAL units
                let nal_units =
                    match protocols::gb28181::parse_ps_to_nal_units(&rtp.payload) {
                        Ok(nals) => nals,
                        Err(_) => continue,
                    };

                if nal_units.is_empty() {
                    continue;
                }

                // 6. Convert RTP timestamp (90 kHz clock) to milliseconds
                let timestamp_ms = (rtp.timestamp as u64) / 90;

                // 7. Queue all extracted NAL units as MediaFrame::Video
                for nal in nal_units.into_iter().rev() {
                    let (header, _payload) =
                        match protocols::h264::parse_nal_header(&nal) {
                            Ok(h) => h,
                            Err(_) => continue,
                        };
                    let keyframe = protocols::h264::is_keyframe(
                        header.nal_unit_type,
                        header.nal_ref_idc,
                    );

                    self.nal_queue.push(MediaFrame::Video {
                        keyframe,
                        data: nal,
                        timestamp: timestamp_ms,
                    });
                }

                // 8. Return the first queued frame
                if let Some(frame) = self.nal_queue.pop() {
                    self.udp_socket = Some(socket);
                    return Ok(frame);
                }
            }
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // Remove the stream from the platform state
            if let Some(ref mut platform) = self.platform {
                if let Some(ref call_id) = self.call_id {
                    platform.remove_stream(call_id);
                }
            }

            // Drop the UDP socket and reset all state
            self.udp_socket = None;
            self.platform = None;
            self.call_id = None;
            self.ssrc = None;
            self.nal_queue.clear();
            self.sequence_map.clear();
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
    // Use a port with nothing listening to get quick connection refused
    let mut src = RtspSource::new("rtsp://127.0.0.1:1/stream1", "admin", "pass");
    let result = src.start().await;
    // Connection refused is expected since nothing listens on port 1
    assert!(result.is_err(), "Expected connection to be refused");
    assert!(!src.started);
    src.stop().await.unwrap();
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

#[tokio::test]
async fn test_rtsp_source_parse_url() {
    let src = RtspSource::new("rtsp://192.168.1.100:554/stream1", "", "");
    let (host, port, path) = src.parse_url().unwrap();
    assert_eq!(host, "192.168.1.100");
    assert_eq!(port, 554);
    assert_eq!(path, "/stream1");

    // Default port
    let src = RtspSource::new("rtsp://camera.local/stream", "", "");
    let (host, port, path) = src.parse_url().unwrap();
    assert_eq!(host, "camera.local");
    assert_eq!(port, 554);
    assert_eq!(path, "/stream");

    // With auth in URL
    let src = RtspSource::new("rtsp://admin:pass@10.0.0.1:8554/path", "", "");
    let (host, port, path) = src.parse_url().unwrap();
    assert_eq!(host, "10.0.0.1");
    assert_eq!(port, 8554);
    assert_eq!(path, "/path");

    // Non-standard port
    let src = RtspSource::new("rtsp://example.com:8554/test", "", "");
    let (host, port, path) = src.parse_url().unwrap();
    assert_eq!(host, "example.com");
    assert_eq!(port, 8554);
    assert_eq!(path, "/test");
}

#[tokio::test]
async fn test_rtsp_source_resolve_url() {
    let src = RtspSource::new("rtsp://192.168.1.100:554/stream1", "", "");
    // Relative control
    assert_eq!(
        src.resolve_url("track1"),
        "rtsp://192.168.1.100:554/stream1/track1"
    );
    // Absolute path control
    assert_eq!(
        src.resolve_url("/absolute/track"),
        "rtsp://192.168.1.100:554/absolute/track"
    );
    // Full URL control
    assert_eq!(
        src.resolve_url("rtsp://other.com/track"),
        "rtsp://other.com/track"
    );

    // With auth in URL
    let src = RtspSource::new("rtsp://admin:pass@10.0.0.1/stream", "", "");
    assert_eq!(
        src.resolve_url("track1"),
        "rtsp://admin:pass@10.0.0.1/stream/track1"
    );
    assert_eq!(
        src.resolve_url("/absolute/track"),
        "rtsp://10.0.0.1/absolute/track"
    );
}

#[tokio::test]
async fn test_rtsp_source_process_rtp_single_nal() {
    let mut src = RtspSource::new("rtsp://localhost/stream", "", "");
    // IDR slice (NAL type 5, ref_idc=3)
    let payload = vec![0x65, 0x01, 0x02, 0x03];
    src.process_rtp_payload(&payload, 1000).unwrap();
    assert_eq!(src.nal_queue.len(), 1);
    if let Some(frame) = src.nal_queue.pop() {
        assert!(matches!(frame, MediaFrame::Video { .. }));
        assert_eq!(frame.timestamp(), 1000);
        assert_eq!(frame.data(), &payload);
    } else {
        panic!("Expected a frame");
    }
}

#[tokio::test]
async fn test_rtsp_source_process_rtp_fua() {
    let mut src = RtspSource::new("rtsp://localhost/stream", "", "");
    // FU-A start: indicator=0x7C (ref_idc=3, type=28), header=0x85 (start=1, type=5 IDR)
    let payload_start = vec![0x7C, 0x85, 0xAA, 0xBB];
    src.process_rtp_payload(&payload_start, 2000).unwrap();
    assert!(src.nal_queue.is_empty());
    assert!(!src.fu_data.is_empty());

    // FU-A middle
    let payload_mid = vec![0x7C, 0x05, 0xCC, 0xDD];
    src.process_rtp_payload(&payload_mid, 2000).unwrap();
    assert!(src.nal_queue.is_empty());

    // FU-A end: header=0x45 (end=1, type=5)
    let payload_end = vec![0x7C, 0x45, 0xEE, 0xFF];
    src.process_rtp_payload(&payload_end, 2000).unwrap();
    assert_eq!(src.nal_queue.len(), 1);

    if let Some(frame) = src.nal_queue.pop() {
        assert!(matches!(frame, MediaFrame::Video { .. }));
        assert_eq!(frame.timestamp(), 2000);
        // NAL header should be reconstructed: ref_idc=3, type=5 => 0x65
        assert_eq!(frame.data()[0], 0x65);
        assert_eq!(&frame.data()[1..], &[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    } else {
        panic!("Expected a frame");
    }
}

#[tokio::test]
async fn test_rtsp_source_process_rtp_stapa() {
    let mut src = RtspSource::new("rtsp://localhost/stream", "", "");
    // Build STAP-A with two NALs: SPS (0x67) and PPS (0x68), both ref_idc=3
    // STAP-A indicator = (3<<5)|24 = 0x78
    let sps = [0x67, 0x42, 0xC0];
    let pps = [0x68, 0xCE, 0x38];

    let mut payload = vec![0x78];
    // SPS NAL: 2-byte length + data
    let sps_len = (sps.len() as u16).to_be_bytes();
    payload.extend_from_slice(&sps_len);
    payload.extend_from_slice(&sps);
    // PPS NAL: 2-byte length + data
    let pps_len = (pps.len() as u16).to_be_bytes();
    payload.extend_from_slice(&pps_len);
    payload.extend_from_slice(&pps);

    src.process_rtp_payload(&payload, 3000).unwrap();
    assert_eq!(src.nal_queue.len(), 2);

    // Vec::pop() is LIFO - PPS was pushed second so it comes out first
    if let Some(frame) = src.nal_queue.pop() {
        assert_eq!(frame.timestamp(), 3000);
        assert_eq!(frame.data(), &pps);
    }
    // SPS was pushed first so it comes out second
    if let Some(frame) = src.nal_queue.pop() {
        assert_eq!(frame.timestamp(), 3000);
        assert_eq!(frame.data(), &sps);
    }
}

#[tokio::test]
async fn test_rtsp_source_stop_cleanup() {
    let mut src = RtspSource::new("rtsp://localhost/stream", "", "");
    // Should not panic even without being started
    src.stop().await.unwrap();
    assert!(!src.started);
    assert!(src.tcp_stream.is_none());
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
        // Without a real SIP server / registered device, start() will fail.
        // Gracefully handle both outcomes (like OnvifSource test pattern).
        let result = src.start().await;
        if result.is_ok() {
            assert!(src.started);
            // next_frame may also fail since no real RTP source,
            // but we verify the method compiles and is callable:
            let _ = src.next_frame().await;
            src.stop().await.unwrap();
            assert!(!src.started);
        }
    }
}
