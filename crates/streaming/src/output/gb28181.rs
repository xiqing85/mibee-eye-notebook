//! GB/T 28181 RTP push output adapter.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;

use anyhow::Result;

use crate::output::{Output, parse_h264_nal_units};
use crate::source::MediaFrame;
use protocols::rtp::{RTP_HEADER_SIZE, RTP_MTU, RtpHeaderFlags, RtpPacket, fragment_nal};

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
