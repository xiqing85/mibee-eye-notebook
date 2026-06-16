//! RTP interleaved framing, transport info parsing, and RTCP sequence tracking.
//!
//! Provides:
//! - [`TransportInfo`] for RTP/RTCP transport header parsing and serialization
//! - [`build_interleaved_frame`] for constructing `$<channel><len><data>` frames
//! - [`SequenceTracker`] for detecting RTP sequence number gaps

use anyhow::{Result, bail};
use tokio::io::{AsyncWrite, AsyncWriteExt};

// ═══════════════════════════════════════════════════════════════════════════════
// Transport Info
// ═══════════════════════════════════════════════════════════════════════════════

/// Transport information for RTP/RTCP session.
#[derive(Debug, Clone, PartialEq)]
pub struct TransportInfo {
    /// Interleaved channel pair for TCP transport: (rtp_channel, rtcp_channel).
    pub interleaved: Option<(u8, u8)>,
    /// Client port pair for UDP transport: (rtp_port, rtcp_port).
    pub client_port: Option<(u16, u16)>,
    /// Server port pair for UDP transport: (rtp_port, rtcp_port).
    pub server_port: Option<(u16, u16)>,
    /// Session identifier.
    pub session_id: String,
    /// Synchronization source identifier.
    pub ssrc: Option<u32>,
    /// Transport mode (e.g., "unicast").
    pub mode: Option<String>,
}

impl TransportInfo {
    /// Parse a Transport header value.
    ///
    /// Example: `RTP/AVP/TCP;interleaved=0-1;ssrc=ABCD1234`
    #[tracing::instrument(skip_all)]
    pub fn parse(header: &str) -> Result<Self> {
        let params: Vec<&str> = header.split(';').collect();
        let mut interleaved: Option<(u8, u8)> = None;
        let mut client_port: Option<(u16, u16)> = None;
        let mut server_port: Option<(u16, u16)> = None;
        let session_id = String::new();
        let mut ssrc: Option<u32> = None;
        let mut mode: Option<String> = None;

        for param in &params {
            let param = param.trim();
            if let Some((key, value)) = param.split_once('=') {
                let key = key.trim().to_lowercase();
                let value = value.trim();
                match key.as_str() {
                    "interleaved" => {
                        let ports: Vec<&str> = value.split('-').collect();
                        if ports.len() == 2 {
                            let rtp = ports[0].parse().ok();
                            let rtcp = ports[1].parse().ok();
                            if let (Some(r), Some(c)) = (rtp, rtcp) {
                                interleaved = Some((r, c));
                            }
                        }
                    }
                    "client_port" => {
                        let ports: Vec<&str> = value.split('-').collect();
                        if ports.len() == 2 {
                            let rtp = ports[0].parse().ok();
                            let rtcp = ports[1].parse().ok();
                            if let (Some(r), Some(c)) = (rtp, rtcp) {
                                client_port = Some((r, c));
                            }
                        }
                    }
                    "server_port" => {
                        let ports: Vec<&str> = value.split('-').collect();
                        if ports.len() == 2 {
                            let rtp = ports[0].parse().ok();
                            let rtcp = ports[1].parse().ok();
                            if let (Some(r), Some(c)) = (rtp, rtcp) {
                                server_port = Some((r, c));
                            }
                        }
                    }
                    "ssrc" => {
                        // SSRC can be hex or decimal
                        ssrc = parse_ssrc(value);
                    }
                    "mode" => {
                        mode = Some(value.to_string());
                    }
                    _ => {}
                }
            } else {
                let p = param.trim().to_lowercase();
                // Handle transport protocol prefix like "RTP/AVP/TCP"
                if p.starts_with("rtp/") {
                    // Transport specifier; ignore for parsing
                }
            }
        }

        Ok(Self {
            interleaved,
            client_port,
            server_port,
            session_id,
            ssrc,
            mode,
        })
    }

    /// Serialize to Transport header value string.
    #[tracing::instrument(skip_all)]
    pub fn serialize(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        parts.push("RTP/AVP/TCP".to_string());

        if let Some((rtp, rtcp)) = self.interleaved {
            parts.push(format!("interleaved={rtp}-{rtcp}"));
        }
        if let Some((rtp, rtcp)) = self.client_port {
            parts.push(format!("client_port={rtp}-{rtcp}"));
        }
        if let Some((rtp, rtcp)) = self.server_port {
            parts.push(format!("server_port={rtp}-{rtcp}"));
        }
        if let Some(ssrc) = self.ssrc {
            parts.push(format!("ssrc=0x{ssrc:08x}"));
        }
        if let Some(ref mode) = self.mode {
            parts.push(format!("mode={mode}"));
        }

        parts.join(";")
    }
}

/// Parse an SSRC value that may be hex (with or without 0x prefix) or decimal.
pub(super) fn parse_ssrc(value: &str) -> Option<u32> {
    let value = value.trim();
    // Try decimal first
    if let Ok(v) = value.parse::<u32>() {
        return Some(v);
    }
    // Then try hex
    u32::from_str_radix(value.trim_start_matches("0x"), 16).ok()
}

/// Case-insensitive header lookup helper.
pub(super) fn get_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Interleaved RTP framing
// ═══════════════════════════════════════════════════════════════════════════════

/// Construct an interleaved RTP frame in the format `$<channel:1B><length:2B><data>`.
///
/// This can be used by external pipelines to inject RTP data into an active RTSP
/// session's TCP stream.
#[tracing::instrument(skip_all)]
pub fn build_interleaved_frame(channel: u8, rtp_data: &[u8]) -> Result<Vec<u8>> {
    let len = rtp_data.len();
    if len > u16::MAX as usize {
        bail!("RTP data too large for interleaved transport: {len} bytes");
    }
    let mut frame = Vec::with_capacity(4 + len);
    frame.push(0x24); // '$'
    frame.push(channel);
    frame.extend_from_slice(&(len as u16).to_be_bytes());
    frame.extend_from_slice(rtp_data);
    Ok(frame)
}

#[allow(dead_code)]
/// Send an interleaved RTP frame on the writer in `$channel length data` format.
pub(super) async fn send_interleaved_rtp<W: AsyncWrite + Unpin>(
    writer: &mut W,
    channel: u8,
    rtp_data: &[u8],
) -> Result<()> {
    let len = rtp_data.len();
    if len > u16::MAX as usize {
        bail!("RTP packet too large for interleaved transport: {len} bytes");
    }
    let mut frame = Vec::with_capacity(4 + len);
    frame.push(0x24); // '$' magic byte
    frame.push(channel);
    frame.extend_from_slice(&(len as u16).to_be_bytes());
    frame.extend_from_slice(rtp_data);
    writer.write_all(&frame).await?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// RTP Sequence Number Gap Detection
// ═══════════════════════════════════════════════════════════════════════════════

/// Minimal RTP header info extracted for sequence tracking.
pub(super) struct RtpHeaderInfo {
    pub(super) sequence_number: u16,
    pub(super) ssrc: u32,
}

/// Parse just the RTP header fields needed for sequence gap tracking.
pub(super) fn parse_rtp_header_for_tracking(data: &[u8]) -> Option<RtpHeaderInfo> {
    if data.len() < 12 {
        return None;
    }
    let version = (data[0] >> 6) & 0x03;
    if version != 2 {
        return None;
    }
    Some(RtpHeaderInfo {
        sequence_number: u16::from_be_bytes([data[2], data[3]]),
        ssrc: u32::from_be_bytes([data[8], data[9], data[10], data[11]]),
    })
}

/// Tracks RTP sequence numbers to detect packet loss gaps.
#[derive(Debug, Clone)]
pub(super) struct SequenceTracker {
    /// Sequence number of the last RTP packet seen, if any.
    last_seq: Option<u16>,
}

impl SequenceTracker {
    pub(super) fn new() -> Self {
        Self { last_seq: None }
    }

    #[allow(dead_code)]
    /// Reset tracking state (used on stream restart).
    pub(super) fn reset(&mut self) {
        self.last_seq = None;
    }

    /// The next expected sequence number, based on the last seen packet.
    pub(super) fn expected(&self) -> u16 {
        self.last_seq.map(|s| s.wrapping_add(1)).unwrap_or(0)
    }

    /// Check a new sequence number.
    ///
    /// Returns `Some(gap)` where `gap` is the number of packets skipped
    /// (gap > 1 indicates packet loss). Returns `None` if the sequence is
    /// contiguous or this is the first packet.
    pub(super) fn check(&mut self, seq: u16) -> Option<u16> {
        match self.last_seq {
            Some(last) => {
                self.last_seq = Some(seq);
                let expected = last.wrapping_add(1);
                if seq == expected {
                    return None;
                }
                let gap = seq.wrapping_sub(expected);
                if gap > 0 { Some(gap) } else { None }
            }
            None => {
                self.last_seq = Some(seq);
                None
            }
        }
    }
}
