//! RTSP server output adapter.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use tokio::sync::{broadcast, oneshot};

use crate::output::Output;
use crate::source::MediaFrame;

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
    /// Oneshot sender for notifying the stream manager when SPS/PPS are first cached.
    /// Used to build the SDP with sprop-parameter-sets for RTSP DESCRIBE.
    sps_pps_tx: Option<oneshot::Sender<(Vec<u8>, Vec<u8>)>>,
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
            sps_pps_tx: None,
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
            sps_pps_tx: None,
        }
    }

    /// Set the oneshot sender for SPS/PPS notifications.
    /// When SPS and PPS are first cached from the frame stream, they will be sent
    /// through this channel so the stream manager can update the RTSP server's SDP.
    pub fn set_sps_pps_tx(&mut self, tx: oneshot::Sender<(Vec<u8>, Vec<u8>)>) {
        self.sps_pps_tx = Some(tx);
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
                            // Notify the stream manager when both are available.
                            if nal_type == 7 {
                                self.cached_sps = Some(data.clone());
                                if self.cached_pps.is_some() {
                                    if let Some(tx) = self.sps_pps_tx.take() {
                                        if let (Some(sps), Some(pps)) =
                                            (self.cached_sps.clone(), self.cached_pps.clone())
                                        {
                                            let _ = tx.send((sps, pps));
                                        }
                                    }
                                }
                            } else if nal_type == 8 {
                                self.cached_pps = Some(data.clone());
                                if self.cached_sps.is_some() {
                                    if let Some(tx) = self.sps_pps_tx.take() {
                                        if let (Some(sps), Some(pps)) =
                                            (self.cached_sps.clone(), self.cached_pps.clone())
                                        {
                                            let _ = tx.send((sps, pps));
                                        }
                                    }
                                }
                            }
                            let ts = self.rtp_timestamp;

                            // Re-send cached SPS/PPS before P-frames so any client
                            // that connected mid-GOP can decode immediately without
                            // waiting for the next IDR keyframe.
                            if nal_type == 1 {
                                if let Some(sps) = &self.cached_sps {
                                    for pkt in
                                        build_rtp_packets(sps, &mut self.rtp_seq, ts, self.ssrc)
                                    {
                                        let _ = tx.send(pkt);
                                    }
                                }
                                if let Some(pps) = &self.cached_pps {
                                    for pkt in
                                        build_rtp_packets(pps, &mut self.rtp_seq, ts, self.ssrc)
                                    {
                                        let _ = tx.send(pkt);
                                    }
                                }
                            }

                            // Send the actual NAL unit as RTP packets.
                            for pkt in build_rtp_packets(&data, &mut self.rtp_seq, ts, self.ssrc) {
                                let _ = tx.send(pkt);
                            }

                            // Only advance timestamp for slice NALs (types 1 and 5).
                            // SPS(7), PPS(8), SEI(6) are part of the same access unit
                            // and MUST share the timestamp of their slice (RFC 6184 §5.1).
                            if nal_type == 1 || nal_type == 5 {
                                self.rtp_timestamp = self.rtp_timestamp.wrapping_add(3000);
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
fn build_rtp_packets(nal_data: &[u8], seq: &mut u16, timestamp: u32, ssrc: u32) -> Vec<Vec<u8>> {
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

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast;

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

        // Verify data arrives as an RTP-encapsulated packet on the channel
        let received = rx.recv().await.expect("Should receive data on channel");
        // 12-byte RTP header + 3-byte NAL payload = 15 bytes
        assert_eq!(
            received.len(),
            15,
            "Expected 12-byte RTP header + 3-byte NAL"
        );
        // RTP version + marker + PT
        assert_eq!(received[0], 0x80, "RTP version=2, no extensions");
        assert_eq!(received[1], 0x60, "marker=1, PT=96 (H.264)");
        // SSRC = 1
        assert_eq!(&received[8..12], &[0, 0, 0, 1], "SSRC should be 1");
        // Payload matches the frame data
        assert_eq!(
            &received[12..],
            &[0x67, 0x42, 0x80],
            "NAL payload should match"
        );

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
}
