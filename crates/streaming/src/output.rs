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
use tokio::sync::broadcast;

use crate::source::MediaFrame;
use anyhow::Result;
use protocols::h264;
use protocols::rtmp::{RtmpPushClient, build_video_nalus, build_video_sequence_header};
use protocols::rtp::{RTP_HEADER_SIZE, RTP_MTU, RtpHeaderFlags, RtpPacket, fragment_nal};
use std::net::SocketAddr;

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
    /// Channel sender for pushing RTP packets to the RTSP server.
    frame_tx: Option<broadcast::Sender<Vec<u8>>>,
    /// Whether the output has been started.
    started: bool,
    /// RTP sequence number (incremented per packet).
    rtp_seq: u16,
    /// RTP timestamp in 90 kHz units (incremented per frame).
    rtp_timestamp: u32,
    /// Cached SPS NAL (type 7) — re-sent before P-frames so new clients can decode immediately.
    cached_sps: Option<Vec<u8>>,
    /// Cached PPS NAL (type 8) — re-sent alongside SPS.
    cached_pps: Option<Vec<u8>>,
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
            rtp_seq: 0,
            rtp_timestamp: 0,
            cached_sps: None,
            cached_pps: None,
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
        frame_tx: broadcast::Sender<Vec<u8>>,
    ) -> Self {
        Self {
            stream_path,
            sdp_body,
            ssrc,
            frame_tx: Some(frame_tx),
            started: false,
            rtp_seq: 0,
            rtp_timestamp: 0,
            cached_sps: None,
            cached_pps: None,
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
                            let nal_type = data.first().map(|b| b & 0x1f).unwrap_or(0);

                            // Cache SPS/PPS whenever they appear in the stream.
                            if nal_type == 7 {
                                self.cached_sps = Some(data.clone());
                            } else if nal_type == 8 {
                                self.cached_pps = Some(data.clone());
                            }

                            let ts = self.rtp_timestamp;

                            // Before P-frames, re-send cached SPS/PPS so any client
                            // that connected mid-GOP can decode immediately without
                            // waiting for the next IDR keyframe.
                            if nal_type == 1 {
                                if let Some(sps) = &self.cached_sps {
                                    let pkts =
                                        build_rtp_packets(sps, &mut self.rtp_seq, ts, self.ssrc);
                                    for p in pkts {
                                        let _ = tx.send(p);
                                    }
                                }
                                if let Some(pps) = &self.cached_pps {
                                    let pkts =
                                        build_rtp_packets(pps, &mut self.rtp_seq, ts, self.ssrc);
                                    for p in pkts {
                                        let _ = tx.send(p);
                                    }
                                }
                            }

                            // Send the actual NAL unit as RTP packets.
                            let packets =
                                build_rtp_packets(&data, &mut self.rtp_seq, ts, self.ssrc);
                            self.rtp_timestamp = self.rtp_timestamp.wrapping_add(3000);
                            for pkt in packets {
                                let _ = tx.send(pkt);
                            }
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

// ═══════════════════════════════════════════════════════════════════════════════
// RTP Packetization (RFC 6184)
// ═══════════════════════════════════════════════════════════════════════════════

/// RTP payload type for H.264.
const RTP_PT_H264: u8 = 96;

/// Maximum NAL size before FU-A fragmentation kicks in.
const RTP_MAX_PAYLOAD: usize = 1400;

/// Build RTP packet(s) from a single H.264 NAL unit (start code already stripped).
///
/// Small NALs use Single NAL Unit Packet mode (§5.6).
/// Large NALs use FU-A fragmentation (§5.8).
fn build_rtp_packets(
    nal_data: &[u8],
    seq: &mut u16,
    timestamp: u32,
    ssrc: u32,
) -> Vec<Vec<u8>> {
    if nal_data.is_empty() {
        return vec![];
    }

    let nal_header = nal_data[0];
    let nal_type = nal_header & 0x1F;
    let nri = nal_header & 0x60;
    // Marker bit on last packet of access unit (IDR=5 or non-IDR slice=1).
    let marker = nal_type == 5 || nal_type == 1;

    if nal_data.len() <= RTP_MAX_PAYLOAD {
        // Single NAL Unit Packet (RFC 6184 §5.6).
        let mut pkt = Vec::with_capacity(12 + nal_data.len());
        pkt.push(0x80); // V=2, P=0, X=0, CC=0
        pkt.push((marker as u8) << 7 | RTP_PT_H264);
        pkt.extend_from_slice(&seq.to_be_bytes());
        pkt.extend_from_slice(&timestamp.to_be_bytes());
        pkt.extend_from_slice(&ssrc.to_be_bytes());
        pkt.extend_from_slice(nal_data);
        *seq = seq.wrapping_add(1);
        vec![pkt]
    } else {
        // FU-A Fragmentation (RFC 6184 §5.8).
        let fu_indicator = 28 | nri;
        let body = &nal_data[1..]; // strip original NAL header byte
        let max_frag = RTP_MAX_PAYLOAD - 2;
        let mut packets = Vec::new();
        let mut offset = 0;

        while offset < body.len() {
            let chunk = std::cmp::min(max_frag, body.len() - offset);
            let is_first = offset == 0;
            let is_last = offset + chunk >= body.len();

            let mut pkt = Vec::with_capacity(12 + 2 + chunk);
            pkt.push(0x80);
            let m = is_last && marker;
            pkt.push((m as u8) << 7 | RTP_PT_H264);
            pkt.extend_from_slice(&seq.to_be_bytes());
            pkt.extend_from_slice(&timestamp.to_be_bytes());
            pkt.extend_from_slice(&ssrc.to_be_bytes());
            pkt.push(fu_indicator);
            pkt.push((is_first as u8) << 7 | (is_last as u8) << 6 | nal_type);
            pkt.extend_from_slice(&body[offset..offset + chunk]);
            packets.push(pkt);
            *seq = seq.wrapping_add(1);
            offset += chunk;
        }
        packets
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

// ---------------------------------------------------------------------------
// Gb28181Output
// ---------------------------------------------------------------------------

/// GB/T 28181 RTP push output.
///
/// Receives H.264 frames and sends them as RTP/UDP packets to the
/// destination address specified in a SIP INVITE. For large NAL units
/// (> MTU - RTP header size), FU-A fragmentation per RFC 6184 is used.
#[allow(dead_code)]
pub struct Gb28181Output {
    /// Destination socket address (SIP platform's media receiver).
    destination: SocketAddr,
    /// SSRC for RTP packets.
    ssrc: u32,
    /// RTP payload type (typically 96 for PS/H.264).
    payload_type: u8,
    /// UDP socket for sending RTP packets.
    socket: Option<tokio::net::UdpSocket>,
    /// Whether the output has been started.
    started: bool,
    /// Call-ID of the SIP session this output belongs to.
    call_id: String,
    /// RTP sequence number (incremented per packet).
    sequence_number: u16,
    /// RTP timestamp (90kHz clock, updated per frame).
    timestamp: u32,
}

impl Gb28181Output {
    /// Create a new GB28181 RTP push output.
    pub fn new(destination: SocketAddr, ssrc: u32, payload_type: u8, call_id: &str) -> Self {
        Self {
            destination,
            ssrc,
            payload_type,
            socket: None,
            started: false,
            call_id: call_id.to_string(),
            sequence_number: 0,
            timestamp: 0,
        }
    }

    /// Get the Call-ID associated with this output.
    pub fn call_id(&self) -> &str {
        &self.call_id
    }
}

impl Output for Gb28181Output {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let socket = tokio::net::UdpSocket::bind("0.0.0.0:0")
                .await
                .map_err(|e| anyhow::anyhow!("Failed to bind UDP socket: {e}"))?;
            socket
                .connect(self.destination)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect UDP socket: {e}"))?;
            self.socket = Some(socket);
            self.started = true;
            tracing::info!(
                call_id = %self.call_id,
                dest = %self.destination,
                "Gb28181Output started"
            );
            Ok(())
        })
    }

    fn send_frame(
        &mut self,
        frame: &MediaFrame,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        match frame {
            MediaFrame::Video {
                data, timestamp, ..
            } => {
                let data = data.clone();
                let ts_ms = *timestamp;
                Box::pin(async move {
                    if !self.started {
                        anyhow::bail!("Gb28181Output not started");
                    }

                    let socket = self
                        .socket
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Gb28181Output socket not available"))?;

                    // Parse NAL units from frame data (handles Annex B and AVCC)
                    let nal_units = parse_h264_nal_units(&data);
                    if nal_units.is_empty() {
                        return Ok(());
                    }

                    // Convert ms timestamp to 90kHz RTP clock
                    let ts_rtp = (ts_ms as u32).wrapping_mul(90);

                    for nal in &nal_units {
                        if nal.len() <= RTP_MTU.saturating_sub(RTP_HEADER_SIZE) {
                            // Single NAL unit packet (RFC 6184 Section 5.6)
                            let packet = RtpPacket {
                                flags: RtpHeaderFlags {
                                    version: 2,
                                    padding: false,
                                    extension: false,
                                    csrc_count: 0,
                                    marker: false,
                                    payload_type: self.payload_type,
                                },
                                sequence_number: self.sequence_number,
                                timestamp: ts_rtp,
                                ssrc: self.ssrc,
                                csrc_list: vec![],
                                extension_profile: None,
                                extension_data: vec![],
                                payload: nal.clone(),
                            };
                            let bytes = packet.to_bytes();
                            socket
                                .send(&bytes)
                                .await
                                .map_err(|e| anyhow::anyhow!("Failed to send RTP packet: {e}"))?;
                            self.sequence_number = self.sequence_number.wrapping_add(1);
                        } else {
                            // FU-A fragmentation (RFC 6184 Section 5.8)
                            let nal_header = nal[0];
                            let nal_ref_idc = (nal_header >> 5) & 0x03;
                            let nal_unit_type = nal_header & 0x1F;
                            let nal_body = &nal[1..];

                            let packets = fragment_nal(
                                nal_body,
                                nal_ref_idc,
                                nal_unit_type,
                                self.payload_type,
                                self.sequence_number,
                                ts_rtp,
                                self.ssrc,
                                RTP_MTU,
                            )?;

                            for pkt in &packets {
                                let bytes = pkt.to_bytes();
                                socket.send(&bytes).await.map_err(|e| {
                                    anyhow::anyhow!("Failed to send FU-A fragment: {e}")
                                })?;
                            }

                            self.sequence_number =
                                self.sequence_number.wrapping_add(packets.len() as u16);
                        }
                    }

                    // Advance internal timestamp for next frame
                    self.timestamp = ts_rtp.wrapping_add(1);
                    Ok(())
                })
            }
            MediaFrame::Audio { .. } => Box::pin(async move {
                // Audio frames not yet supported via GB28181 push; silently drop.
                Ok(())
            }),
        }
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.socket = None;
            self.started = false;
            tracing::info!(call_id = %self.call_id, "Gb28181Output stopped");
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
        let (tx, _rx) = broadcast::channel(16);
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
        let (tx, mut rx) = broadcast::channel(16);
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
        let (tx, mut rx) = broadcast::channel(16);
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

    // ── Gb28181Output tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_gb28181_output_new() {
        let dest: SocketAddr = "127.0.0.1:20000".parse().unwrap();
        let out = Gb28181Output::new(dest, 0x1234, 96, "test-call-id");
        assert_eq!(out.call_id(), "test-call-id");
        assert_eq!(out.ssrc, 0x1234);
        assert_eq!(out.payload_type, 96);
        assert_eq!(out.destination, dest);
        assert!(!out.started);
        assert!(out.socket.is_none());
    }

    #[tokio::test]
    async fn test_gb28181_output_start_stop() {
        let dest: SocketAddr = "127.0.0.1:20001".parse().unwrap();
        let mut out = Gb28181Output::new(dest, 0x5678, 96, "test-call-start-stop");
        assert!(!out.started);
        out.start().await.unwrap();
        assert!(out.started);
        assert!(out.socket.is_some());
        out.stop().await.unwrap();
        assert!(!out.started);
        assert!(out.socket.is_none());
    }

    #[tokio::test]
    async fn test_gb28181_output_send_video_frame() {
        let receiver = tokio::net::UdpSocket::bind("127.0.0.1:21000")
            .await
            .unwrap();
        let dest: SocketAddr = receiver.local_addr().unwrap();

        let mut out = Gb28181Output::new(dest, 0x9ABC, 96, "test-send-frame");
        out.start().await.unwrap();

        // Send a small frame (single NAL unit)
        let frame = MediaFrame::Video {
            keyframe: true,
            data: vec![
                0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x80, 0x1E, // SPS
            ],
            timestamp: 42,
        };
        out.send_frame(&frame).await.unwrap();

        // Verify a packet was received
        let mut buf = vec![0u8; 1500];
        let len = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            receiver.recv(&mut buf),
        )
        .await
        .expect("Should receive RTP packet")
        .expect("recv should succeed");

        // RTP header is 12 bytes, payload follows
        assert!(len > 12, "RTP packet too short: {}", len);
        // Verify RTP version (first 2 bits = 2)
        assert_eq!(buf[0] >> 6, 2, "RTP version should be 2");

        out.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_gb28181_output_ignore_audio() {
        let dest: SocketAddr = "127.0.0.1:21001".parse().unwrap();
        let mut out = Gb28181Output::new(dest, 0, 96, "test-ignore-audio");
        out.start().await.unwrap();

        // Audio frames should be silently dropped
        let frame = MediaFrame::Audio {
            data: vec![0xFF; 160],
            timestamp: 100,
        };
        out.send_frame(&frame).await.unwrap();

        out.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_gb28181_output_not_started_fails() {
        let dest: SocketAddr = "127.0.0.1:21002".parse().unwrap();
        let mut out = Gb28181Output::new(dest, 0, 96, "test-not-started");
        let frame = MediaFrame::Video {
            keyframe: false,
            data: vec![0x67],
            timestamp: 0,
        };
        assert!(out.send_frame(&frame).await.is_err());
    }

    #[tokio::test]
    async fn test_gb28181_output_fua_fragmentation() {
        // Create a large NAL unit that exceeds MTU
        let nal_body: Vec<u8> = (0..2000).map(|i| (i % 256) as u8).collect();
        let mut nal = vec![0x65]; // header: ref_idc=3, type=5 (IDR)
        nal.extend_from_slice(&nal_body);

        // Use protocols::rtp::fragment_nal directly to verify FU-A
        let packets = fragment_nal(&nal_body, 3, 5, 96, 0, 1000, 0xABCD, RTP_MTU).unwrap();

        assert!(packets.len() > 1, "Large NAL should produce >1 fragment");
        assert_eq!(packets[0].ssrc, 0xABCD);
        assert_eq!(packets[0].timestamp, 1000);

        // First fragment has start=1, end=0
        let first_fh = protocols::rtp::parse_fua_header(packets[0].payload[1]);
        assert!(first_fh.start);
        assert!(!first_fh.end);

        // Last fragment has start=0, end=1
        let last_fh = protocols::rtp::parse_fua_header(packets.last().unwrap().payload[1]);
        assert!(!last_fh.start);
        assert!(last_fh.end);
    }
}
