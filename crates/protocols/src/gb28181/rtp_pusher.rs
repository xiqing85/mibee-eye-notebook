//! RTP pusher — constructs and sends RTP packets to a destination.

use std::net::SocketAddr;

use super::sip::Transport;
use crate::rtp::{RtpHeaderFlags, RtpPacket};

/// Information about an RTP stream established via SIP INVITE.
#[derive(Debug, Clone)]
pub struct RtpStreamInfo {
    /// Device ID that is sending/receiving the stream
    pub device_id: String,
    /// Channel ID
    pub channel_id: String,
    /// SSRC of the RTP stream
    pub ssrc: u32,
    /// Transport protocol
    pub transport: Transport,
    /// Remote address for RTP data
    pub remote_addr: String,
    /// Remote port for RTP data
    pub remote_port: u16,
}

/// Constructs RTP packets for pushing media to a destination.
///
/// Uses the `crate::rtp::RtpPacket` for constructing RFC 3550 compliant
/// RTP packets. Each call to `build_rtp_packet` wraps a H.264 NAL unit
/// in a Single NAL Unit packet (RFC 6184) and increments the sequence number.
#[derive(Debug, Clone)]
pub struct RtpPusher {
    /// Destination socket address
    pub destination: SocketAddr,
    /// Synchronization source identifier
    pub ssrc: u32,
    /// Sequence number (incremented per packet)
    pub sequence_number: u16,
    /// Timestamp (90kHz clock, typical for H.264)
    pub timestamp: u32,
    /// RTP payload type (typically 96 for H.264/PS)
    pub payload_type: u8,
}

impl RtpPusher {
    /// Create a new RTP pusher.
    #[tracing::instrument(skip_all)]
    pub fn new(destination: SocketAddr, ssrc: u32, payload_type: u8) -> Self {
        Self {
            destination,
            ssrc,
            sequence_number: 0,
            timestamp: 0,
            payload_type,
        }
    }

    /// Build an RTP packet containing a H.264 NAL unit.
    ///
    /// Uses Single NAL Unit packet format (RFC 6184 section 5.6).
    /// The sequence number is auto-incremented after each packet.
    /// Returns the serialized RTP packet bytes.
    #[tracing::instrument(skip_all)]
    pub fn build_rtp_packet(&mut self, nal: &[u8]) -> Vec<u8> {
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
            timestamp: self.timestamp,
            ssrc: self.ssrc,
            csrc_list: vec![],
            extension_profile: None,
            extension_data: vec![],
            payload: nal.to_vec(),
        };

        let bytes = packet.to_bytes();

        // Increment sequence number for next packet
        self.sequence_number = self.sequence_number.wrapping_add(1);

        bytes
    }

    /// Increment the timestamp by the given amount.
    ///
    /// Typical increment for 30fps H.264 at 90kHz clock is 3000 (90000/30).
    #[tracing::instrument(skip_all)]
    pub fn increment_timestamp(&mut self, increment: u32) {
        self.timestamp = self.timestamp.wrapping_add(increment);
    }
}
