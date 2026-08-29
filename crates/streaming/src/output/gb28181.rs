//! GB/T 28181 RTP push output adapter.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;

use anyhow::Result;

use crate::output::{Output, parse_h264_nal_units};
use crate::source::MediaFrame;
use protocols::rtp::{RtpHeaderFlags, RtpPacket};

// ---------------------------------------------------------------------------
// Gb28181Output
// ---------------------------------------------------------------------------

/// GB/T 28181 RTP push output.
///
/// Receives H.264 frames and sends them as PS-over-RTP/UDP packets to the
/// destination address specified in a SIP INVITE. PS data is fragmented
/// across MTU 1400 with the marker bit set on the last packet of each
/// access unit (per GB/T 28181).
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

    /// Attach a pre-bound UDP socket (e.g., one whose local port was
    /// advertised in the SIP 200 OK SDP answer). `start` will use it
    /// instead of binding a new ephemeral socket.
    pub fn with_socket(mut self, socket: tokio::net::UdpSocket) -> Self {
        self.socket = Some(socket);
        self
    }
}

impl Output for Gb28181Output {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // Use a pre-bound socket when provided (its port was advertised
            // in the 200 OK SDP answer); otherwise bind an ephemeral one.
            if self.socket.is_none() {
                let socket = tokio::net::UdpSocket::bind("0.0.0.0:0")
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to bind UDP socket: {e}"))?;
                self.socket = Some(socket);
            }
            let socket = self
                .socket
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("UDP socket unavailable"))?;
            socket
                .connect(self.destination)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to connect UDP socket: {e}"))?;
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
                keyframe,
                data,
                timestamp,
            } => {
                let data = data.clone();
                let is_keyframe = *keyframe;
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
                    let ts_rtp_64 = (ts_ms as u64).wrapping_mul(90);
                    let ts_rtp = ts_rtp_64 as u32;

                    // Mux H.264 NAL units to MPEG-2 PS via gb28181-rs (shared
                    // wire format with the Pi camera products; balanced
                    // PES_packet_length + >64KB AU splitting per v0.2.0).
                    // PSM inclusion on keyframes is handled by the muxer.
                    let ps_data = gb28181_rs::ps::mux_h264_to_ps(
                        &nal_units.iter().map(|n| n.as_slice()).collect::<Vec<_>>(),
                        is_keyframe,
                        ts_rtp_64,
                        ts_rtp_64,
                    );

                    if ps_data.is_empty() {
                        return Ok(());
                    }

                    // Fragment PS data across RTP packets (MTU 1400)
                    // GB/T 28181 mandates PT=96 for PS-over-RTP
                    const PS_MTU: usize = 1400;
                    let total_len = ps_data.len();
                    let mut offset = 0;
                    while offset < total_len {
                        let chunk_end = (offset + PS_MTU).min(total_len);
                        let is_last = chunk_end == total_len;

                        let packet = RtpPacket {
                            flags: RtpHeaderFlags {
                                version: 2,
                                padding: false,
                                extension: false,
                                csrc_count: 0,
                                marker: is_last, // Marker bit on LAST packet of access unit
                                payload_type: 96, // PT=96 for PS-over-RTP per GB28181
                            },
                            sequence_number: self.sequence_number,
                            timestamp: ts_rtp,
                            ssrc: self.ssrc,
                            csrc_list: vec![],
                            extension_profile: None,
                            extension_data: vec![],
                            payload: ps_data[offset..chunk_end].to_vec(),
                        };
                        let bytes = packet.to_bytes();
                        socket
                            .send(&bytes)
                            .await
                            .map_err(|e| anyhow::anyhow!("Failed to send RTP packet: {e}"))?;
                        self.sequence_number = self.sequence_number.wrapping_add(1);
                        offset = chunk_end;
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

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn test_gb28181_output_sends_ps_over_rtp() {
        // Verify PS-over-RTP packetization
        let receiver = tokio::net::UdpSocket::bind("127.0.0.1:22000")
            .await
            .unwrap();
        let dest: SocketAddr = receiver.local_addr().unwrap();

        let mut out = Gb28181Output::new(dest, 0x12345678, 96, "test-ps-rtp");
        out.start().await.unwrap();

        // Send a small IDR frame (SPS + PPS + IDR slice)
        let frame = MediaFrame::Video {
            keyframe: true,
            data: vec![
                0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x80, 0x1E, 0xD9, // SPS
                0x00, 0x00, 0x00, 0x01, 0x68, 0xCE, 0x38, 0x80, // PPS
                0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x00, 0x01, 0x23, 0x45, // IDR
            ],
            timestamp: 100,
        };
        out.send_frame(&frame).await.unwrap();

        // Verify RTP packet was received
        let mut buf = vec![0u8; 1500];
        let len = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            receiver.recv(&mut buf),
        )
        .await
        .expect("Should receive RTP packet")
        .expect("recv should succeed");

        // RTP header is 12 bytes, PS payload follows
        assert!(len > 12, "RTP packet too short: {}", len);
        // Verify RTP version (first 2 bits = 2)
        assert_eq!(buf[0] >> 6, 2, "RTP version should be 2");
        // Verify payload type (PT=96 for PS-over-RTP)
        assert_eq!(buf[1] & 0x7F, 96, "Payload type should be 96 for PS");
        // Verify SSRC from INVITE
        let ssrc_be = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
        assert_eq!(ssrc_be, 0x12345678, "SSRC should match INVITE value");
        // Verify PS data starts with pack header 0x00 0x00 0x01 0xBA
        assert!(
            buf[12..].starts_with(&[0x00, 0x00, 0x01, 0xBA]),
            "Should start with PS pack header"
        );

        out.stop().await.unwrap();
    }
}
