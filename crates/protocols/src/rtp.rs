#![cfg_attr(test, deny(warnings))]

/// RTP packet parser/constructor per RFC 3550
/// H.264 payload support per RFC 6184 (Single NAL, STAP-A, FU-A)
use anyhow::{Result, anyhow};

/// RTP header minimum fixed size (12 bytes)
pub const RTP_HEADER_SIZE: usize = 12;

/// RTP version 2
pub const RTP_VERSION: u8 = 2;

/// Common dynamic payload type for H.264
pub const H264_PAYLOAD_TYPE: u8 = 96;

/// Default MTU-safe maximum payload before fragmentation
pub const RTP_MTU: usize = 1400;

/// RTP header flags (first two bytes)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpHeaderFlags {
    /// Version (must be 2)
    pub version: u8,
    /// Padding flag
    pub padding: bool,
    /// Extension flag
    pub extension: bool,
    /// CSRC count (4 bits)
    pub csrc_count: u8,
    /// Marker bit
    pub marker: bool,
    /// Payload type (7 bits)
    pub payload_type: u8,
}

impl Default for RtpHeaderFlags {
    fn default() -> Self {
        Self {
            version: RTP_VERSION,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type: H264_PAYLOAD_TYPE,
        }
    }
}

/// An RTP packet (RFC 3550)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpPacket {
    pub flags: RtpHeaderFlags,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub csrc_list: Vec<u32>,
    pub extension_profile: Option<u16>,
    pub extension_data: Vec<u8>,
    pub payload: Vec<u8>,
}

impl RtpPacket {
    /// Parse an RTP packet from raw bytes.
    ///
    /// Returns an error if the data is too short, has an unsupported version,
    /// or contains truncated header fields. Padding bytes are stripped from
    /// the returned payload (the canonical form).
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < RTP_HEADER_SIZE {
            return Err(anyhow!(
                "RTP packet too short: {} bytes (minimum {})",
                data.len(),
                RTP_HEADER_SIZE
            ));
        }

        let first_byte = data[0];
        let second_byte = data[1];

        let version = (first_byte >> 6) & 0x03;
        if version != RTP_VERSION {
            return Err(anyhow!("Unsupported RTP version: {} (expected 2)", version));
        }

        let padding = ((first_byte >> 5) & 0x01) != 0;
        let extension = ((first_byte >> 4) & 0x01) != 0;
        let csrc_count = first_byte & 0x0F;

        let marker = ((second_byte >> 7) & 0x01) != 0;
        let payload_type = second_byte & 0x7F;

        let sequence_number = u16::from_be_bytes([data[2], data[3]]);
        let timestamp = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let ssrc = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);

        let mut offset = RTP_HEADER_SIZE;

        // ── CSRC list ────────────────────────────────────────────────
        let mut csrc_list = Vec::with_capacity(csrc_count as usize);
        for _ in 0..csrc_count {
            if offset + 4 > data.len() {
                return Err(anyhow!(
                    "RTP packet truncated: missing CSRC entry at offset {}",
                    offset
                ));
            }
            let csrc = u32::from_be_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);
            csrc_list.push(csrc);
            offset += 4;
        }

        // ── Extension header ─────────────────────────────────────────
        let (extension_profile, extension_data) = if extension {
            if offset + 4 > data.len() {
                return Err(anyhow!(
                    "RTP packet truncated: missing extension header at offset {}",
                    offset
                ));
            }
            let profile = u16::from_be_bytes([data[offset], data[offset + 1]]);
            let ext_len_words = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
            offset += 4;

            // Extension length is in 32-bit words
            let ext_data_end_byte = offset + ext_len_words * 4;
            if ext_data_end_byte > data.len() {
                return Err(anyhow!(
                    "RTP packet truncated: extension data extends to {} but packet is {} bytes",
                    ext_data_end_byte,
                    data.len()
                ));
            }
            let ext_data = data[offset..ext_data_end_byte].to_vec();
            offset = ext_data_end_byte;
            (Some(profile), ext_data)
        } else {
            (None, vec![])
        };

        // ── Payload ──────────────────────────────────────────────────
        let raw_payload = &data[offset..];

        let payload = if padding {
            if raw_payload.is_empty() {
                return Err(anyhow!("RTP padding flag is set but no payload present"));
            }
            let pad_len = raw_payload.last().copied().unwrap_or(0) as usize;
            if pad_len == 0 || pad_len > raw_payload.len() {
                return Err(anyhow!(
                    "Invalid RTP padding length: {} (payload len: {})",
                    pad_len,
                    raw_payload.len()
                ));
            }
            raw_payload[..raw_payload.len() - pad_len].to_vec()
        } else {
            raw_payload.to_vec()
        };

        Ok(Self {
            flags: RtpHeaderFlags {
                version,
                padding,
                extension,
                csrc_count,
                marker,
                payload_type,
            },
            sequence_number,
            timestamp,
            ssrc,
            csrc_list,
            extension_profile,
            extension_data,
            payload,
        })
    }

    /// Serialize the RTP packet to bytes.
    ///
    /// Produces a valid RTP packet with correct header layout, CSRC list,
    /// optional extension (padded to 32-bit boundary), and payload.
    pub fn to_bytes(&self) -> Vec<u8> {
        let ext_data_len = if self.flags.extension {
            // Pad extension data to 32-bit boundary
            let raw_len = self.extension_data.len();

            (raw_len + 3) & !3
        } else {
            0
        };

        let total_len = RTP_HEADER_SIZE
            + self.csrc_list.len() * 4
            + if self.flags.extension {
                4 + ext_data_len
            } else {
                0
            }
            + self.payload.len();

        let mut buf = Vec::with_capacity(total_len);

        let first_byte: u8 = (self.flags.version << 6)
            | ((self.flags.padding as u8) << 5)
            | ((self.flags.extension as u8) << 4)
            | (self.flags.csrc_count & 0x0F);

        let second_byte: u8 = ((self.flags.marker as u8) << 7) | (self.flags.payload_type & 0x7F);

        buf.push(first_byte);
        buf.push(second_byte);
        buf.extend_from_slice(&self.sequence_number.to_be_bytes());
        buf.extend_from_slice(&self.timestamp.to_be_bytes());
        buf.extend_from_slice(&self.ssrc.to_be_bytes());

        // CSRC list
        for csrc in &self.csrc_list {
            buf.extend_from_slice(&csrc.to_be_bytes());
        }

        // Extension
        if self.flags.extension {
            let profile = self.extension_profile.unwrap_or(0);
            let ext_word_len = (ext_data_len / 4) as u16;
            buf.extend_from_slice(&profile.to_be_bytes());
            buf.extend_from_slice(&ext_word_len.to_be_bytes());
            buf.extend_from_slice(&self.extension_data);
            // Pad to 32-bit boundary with zeros
            let pad_count = ext_data_len - self.extension_data.len();
            buf.extend(std::iter::repeat_n(0u8, pad_count));
        }

        // Payload
        buf.extend_from_slice(&self.payload);

        debug_assert_eq!(buf.len(), total_len, "RTP serialized size mismatch");
        buf
    }
}

// ─── RFC 6184 H.264 RTP Payload ────────────────────────────────────────────

/// RTP H.264 NAL unit types relevant to RTP payload handling (RFC 6184)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtpNalType {
    /// Single NAL unit (types 1-23)
    Single(u8),
    /// STAP-A — Single-time aggregation (type 24)
    StapA,
    /// FU-A — Fragmentation unit (type 28)
    FuA,
}

impl TryFrom<u8> for RtpNalType {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            1..=23 => Ok(RtpNalType::Single(value)),
            24 => Ok(RtpNalType::StapA),
            28 => Ok(RtpNalType::FuA),
            _ => Err(anyhow!("Unsupported RTP H.264 NAL type: {}", value)),
        }
    }
}

/// FU-A indicator byte (first byte of an FU-A payload)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuAIndicator {
    pub forbidden_zero_bit: u8,
    pub nal_ref_idc: u8,
}

/// FU-A header byte (second byte of an FU-A payload)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuAHeader {
    pub start: bool,
    pub end: bool,
    pub reserved: u8,
    pub nal_unit_type: u8,
}

/// Parse the FU-A indicator byte (first byte of an FU-A RTP payload).
pub fn parse_fua_indicator(byte: u8) -> FuAIndicator {
    FuAIndicator {
        forbidden_zero_bit: (byte >> 7) & 0x01,
        nal_ref_idc: (byte >> 5) & 0x03,
    }
}

/// Parse the FU-A header byte (second byte of an FU-A RTP payload).
pub fn parse_fua_header(byte: u8) -> FuAHeader {
    FuAHeader {
        start: ((byte >> 7) & 0x01) != 0,
        end: ((byte >> 6) & 0x01) != 0,
        reserved: (byte >> 5) & 0x01,
        nal_unit_type: byte & 0x1F,
    }
}

/// Build FU-A indicator byte from nal_ref_idc (type is implicitly 28).
pub fn build_fua_indicator(nal_ref_idc: u8) -> u8 {
    ((nal_ref_idc & 0x03) << 5) | 28
}

/// Build FU-A header byte.
pub fn build_fua_header(start: bool, end: bool, nal_unit_type: u8) -> u8 {
    ((start as u8) << 7) | ((end as u8) << 6) | (nal_unit_type & 0x1F)
}

/// Determine the RTP NAL type from an H.264 RTP payload.
pub fn get_rtp_nal_type(payload: &[u8]) -> Result<RtpNalType> {
    if payload.is_empty() {
        return Err(anyhow!("Empty RTP payload"));
    }
    let nal_type = payload[0] & 0x1F;
    RtpNalType::try_from(nal_type)
}

/// Check if the RTP payload is a single NAL unit (types 1-23).
pub fn is_single_nal(payload: &[u8]) -> bool {
    payload.first().is_some_and(|&b| matches!(b & 0x1F, 1..=23))
}

/// Check if the RTP payload is STAP-A (type 24).
pub fn is_stapa(payload: &[u8]) -> bool {
    payload.first().is_some_and(|&b| (b & 0x1F) == 24)
}

/// Check if the RTP payload is FU-A (type 28).
pub fn is_fua(payload: &[u8]) -> bool {
    payload.first().is_some_and(|&b| (b & 0x1F) == 28)
}

/// Fragment a large NAL unit into FU-A RTP packets.
///
/// * `nal_data` — NAL unit body, **excluding** the 1-byte NAL header.
/// * `nal_ref_idc` — NAL reference IDC (0-3) from the original NAL header.
/// * `nal_unit_type` — Original NAL unit type (1-23).
/// * `payload_type` — RTP payload type to use for every fragment.
/// * `sequence_number` — Starting RTP sequence number (incremented per fragment).
/// * `timestamp` — RTP timestamp (same for all fragments).
/// * `ssrc` — Synchronization source identifier.
/// * `mtu` — Maximum Transmission Unit (must be > RTP_HEADER_SIZE + 2).
///
/// Returns a vector of complete `RtpPacket`s, one per fragment.
/// Only the final fragment has the marker bit set.
#[allow(clippy::too_many_arguments)]
pub fn fragment_nal(
    nal_data: &[u8],
    nal_ref_idc: u8,
    nal_unit_type: u8,
    payload_type: u8,
    sequence_number: u16,
    timestamp: u32,
    ssrc: u32,
    mtu: usize,
) -> Result<Vec<RtpPacket>> {
    if nal_data.is_empty() {
        return Err(anyhow!("Cannot fragment empty NAL data"));
    }
    if mtu <= RTP_HEADER_SIZE + 2 {
        return Err(anyhow!(
            "MTU too small for FU-A: {} (need > {})",
            mtu,
            RTP_HEADER_SIZE + 2
        ));
    }

    let fu_indicator = build_fua_indicator(nal_ref_idc);
    let max_payload = mtu - RTP_HEADER_SIZE - 2; // 2 bytes for FU indicator + FU header
    if max_payload == 0 {
        return Err(anyhow!(
            "MTU too small: header overhead ({}) exceeds MTU ({})",
            RTP_HEADER_SIZE + 2,
            mtu
        ));
    }

    let num_packets = nal_data.len().div_ceil(max_payload);
    let mut packets = Vec::with_capacity(num_packets);
    let mut offset = 0;
    let mut seq = sequence_number;

    for i in 0..num_packets {
        let is_first = i == 0;
        let is_last = i == num_packets - 1;
        let chunk_end = (offset + max_payload).min(nal_data.len());
        let chunk = &nal_data[offset..chunk_end];

        let fu_header = build_fua_header(is_first, is_last, nal_unit_type);

        let mut payload = Vec::with_capacity(2 + chunk.len());
        payload.push(fu_indicator);
        payload.push(fu_header);
        payload.extend_from_slice(chunk);

        packets.push(RtpPacket {
            flags: RtpHeaderFlags {
                version: RTP_VERSION,
                padding: false,
                extension: false,
                csrc_count: 0,
                marker: is_last,
                payload_type,
            },
            sequence_number: seq,
            timestamp,
            ssrc,
            csrc_list: vec![],
            extension_profile: None,
            extension_data: vec![],
            payload,
        });

        offset = chunk_end;
        seq = seq.wrapping_add(1);
    }

    Ok(packets)
}

/// Reassemble FU-A fragments back into the original NAL unit.
///
/// Takes a slice of FU-A `RtpPacket`s (must be in order, with start/end
/// flags set correctly on the first and last packets respectively).
///
/// Returns the reconstructed NAL unit including its original 1-byte NAL header
/// (reconstructed from the FU indicator + FU header of the first fragment).
pub fn reassemble_fua(packets: &[RtpPacket]) -> Result<Vec<u8>> {
    if packets.is_empty() {
        return Err(anyhow!("No FU-A packets to reassemble"));
    }

    let first_payload = &packets[0].payload;
    if first_payload.len() < 2 {
        return Err(anyhow!(
            "FU-A packet 0 payload too short: {} bytes",
            first_payload.len()
        ));
    }

    let indicator = parse_fua_indicator(first_payload[0]);
    let header = parse_fua_header(first_payload[1]);

    if indicator.forbidden_zero_bit != 0 {
        return Err(anyhow!("FU-A forbidden zero bit is set in fragment 0"));
    }

    if !header.start {
        return Err(anyhow!("First FU-A fragment must have the start flag set"));
    }

    // Reconstruct the original NAL header byte
    let nal_header_byte =
        (indicator.forbidden_zero_bit << 7) | (indicator.nal_ref_idc << 5) | header.nal_unit_type;

    // Verify all fragments are consistent and properly sequenced
    for (i, packet) in packets.iter().enumerate() {
        let pld = &packet.payload;
        if pld.len() < 2 {
            return Err(anyhow!(
                "FU-A fragment {} payload too short: {} bytes",
                i,
                pld.len()
            ));
        }

        let fi = parse_fua_indicator(pld[0]);
        let fh = parse_fua_header(pld[1]);

        if fi.nal_ref_idc != indicator.nal_ref_idc {
            return Err(anyhow!(
                "FU-A fragment {} nal_ref_idc mismatch: expected {}, got {}",
                i,
                indicator.nal_ref_idc,
                fi.nal_ref_idc
            ));
        }

        if fh.nal_unit_type != header.nal_unit_type {
            return Err(anyhow!(
                "FU-A fragment {} nal_unit_type mismatch: expected {}, got {}",
                i,
                header.nal_unit_type,
                fh.nal_unit_type
            ));
        }

        if i == 0 && !fh.start {
            return Err(anyhow!(
                "FU-A fragment 0 must have start=1, got start={}",
                fh.start
            ));
        }
        if i == packets.len() - 1 && !fh.end {
            return Err(anyhow!(
                "FU-A fragment {} must have end=1, got end={}",
                i,
                fh.end
            ));
        }
        if i > 0 && i < packets.len() - 1 && (fh.start || fh.end) {
            return Err(anyhow!(
                "FU-A fragment {}: middle fragment has start={}, end={}",
                i,
                fh.start,
                fh.end
            ));
        }
    }

    // Collect NAL data from all fragments (skip 2-byte FU indicator/header)
    let total_data_len: usize = packets
        .iter()
        .map(|p| p.payload.len().saturating_sub(2))
        .sum();
    let mut nal_unit = Vec::with_capacity(1 + total_data_len);
    nal_unit.push(nal_header_byte);

    for packet in packets {
        nal_unit.extend_from_slice(&packet.payload[2..]);
    }

    Ok(nal_unit)
}

/// Parse a STAP-A payload, returning the individual NAL units.
///
/// Each returned NAL unit includes its original 1-byte NAL header.
/// Returns an error if the payload is malformed or truncated.
pub fn parse_stapa(payload: &[u8]) -> Result<Vec<Vec<u8>>> {
    if payload.is_empty() {
        return Err(anyhow!("Empty STAP-A payload"));
    }

    let stap_type = payload[0] & 0x1F;
    if stap_type != 24 {
        return Err(anyhow!("Not a STAP-A payload: NAL type {}", stap_type));
    }

    let mut nals = Vec::new();
    let mut offset = 1; // Skip STAP-A indicator byte

    while offset < payload.len() {
        if offset + 2 > payload.len() {
            return Err(anyhow!(
                "STAP-A truncated: missing 16-bit NAL size at offset {}",
                offset
            ));
        }

        let nal_size = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
        offset += 2;

        if nal_size == 0 {
            return Err(anyhow!(
                "STAP-A: zero-length NAL unit at offset {}",
                offset - 2
            ));
        }

        if offset + nal_size > payload.len() {
            return Err(anyhow!(
                "STAP-A truncated: NAL size {} at offset {} exceeds remaining data {}",
                nal_size,
                offset,
                payload.len() - offset
            ));
        }

        nals.push(payload[offset..offset + nal_size].to_vec());
        offset += nal_size;
    }

    Ok(nals)
}

/// Build a STAP-A payload from multiple NAL units.
///
/// Each NAL unit must include its 1-byte NAL header.
/// All NAL units MUST have the same `nal_ref_idc` value (checked).
pub fn build_stapa(nalus: &[&[u8]]) -> Result<Vec<u8>> {
    if nalus.is_empty() {
        return Err(anyhow!("Cannot build STAP-A from empty NAL list"));
    }

    // All NAL units must share the same nal_ref_idc per RFC 6184 Section 5.7
    let nal_ref_idc = (nalus[0]
        .first()
        .ok_or_else(|| anyhow!("Empty NAL unit 0 in STAP-A"))?
        >> 5)
        & 0x03;

    for (i, nal) in nalus.iter().enumerate() {
        let first = nal
            .first()
            .ok_or_else(|| anyhow!("STAP-A: NAL unit {} is empty", i))?;
        let ref_idc = (first >> 5) & 0x03;
        if ref_idc != nal_ref_idc {
            return Err(anyhow!(
                "STAP-A: NAL unit {} has nal_ref_idc {} but expected {} (all NALs must share the same NRI)",
                i,
                ref_idc,
                nal_ref_idc
            ));
        }
    }

    // Build payload: STAP-A indicator + each NAL (16-bit size + data)
    let mut total_size = 1; // STAP-A indicator byte
    for nal in nalus {
        total_size += 2 + nal.len();
    }

    let mut payload = Vec::with_capacity(total_size);
    payload.push((nal_ref_idc << 5) | 24); // STAP-A indicator

    for nal in nalus {
        let size = nal.len();
        payload.extend_from_slice(&(size as u16).to_be_bytes());
        payload.extend_from_slice(nal);
    }

    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── RTP Header Round-Trip ────────────────────────────────────────

    #[test]
    fn test_rtp_header_roundtrip_minimal() {
        let packet = RtpPacket {
            flags: RtpHeaderFlags {
                version: 2,
                padding: false,
                extension: false,
                csrc_count: 0,
                marker: false,
                payload_type: 96,
            },
            sequence_number: 12345,
            timestamp: 987654321,
            ssrc: 0xDEADBEEF,
            csrc_list: vec![],
            extension_profile: None,
            extension_data: vec![],
            payload: vec![0x00, 0x01, 0x02, 0x03],
        };

        let bytes = packet.to_bytes();
        assert_eq!(bytes.len(), RTP_HEADER_SIZE + 4);

        let parsed = RtpPacket::parse(&bytes).unwrap();
        assert_eq!(parsed.flags.version, 2);
        assert_eq!(parsed.flags.payload_type, 96);
        assert!(!parsed.flags.marker);
        assert!(!parsed.flags.padding);
        assert!(!parsed.flags.extension);
        assert_eq!(parsed.flags.csrc_count, 0);
        assert_eq!(parsed.sequence_number, 12345);
        assert_eq!(parsed.timestamp, 987654321);
        assert_eq!(parsed.ssrc, 0xDEADBEEF);
        assert!(parsed.csrc_list.is_empty());
        assert_eq!(parsed.payload, vec![0x00, 0x01, 0x02, 0x03]);
    }

    #[test]
    fn test_rtp_marker_bit_roundtrip() {
        let packet = RtpPacket {
            flags: RtpHeaderFlags {
                version: 2,
                padding: false,
                extension: false,
                csrc_count: 0,
                marker: true,
                payload_type: 96,
            },
            sequence_number: 789,
            timestamp: 0,
            ssrc: 0x12345678,
            csrc_list: vec![],
            extension_profile: None,
            extension_data: vec![],
            payload: vec![0xFF],
        };

        let bytes = packet.to_bytes();
        let parsed = RtpPacket::parse(&bytes).unwrap();
        assert!(parsed.flags.marker);
        assert_eq!(parsed.sequence_number, 789);
        assert_eq!(parsed.ssrc, 0x12345678);
    }

    #[test]
    fn test_rtp_extension_roundtrip() {
        let packet = RtpPacket {
            flags: RtpHeaderFlags {
                version: 2,
                padding: false,
                extension: true,
                csrc_count: 0,
                marker: false,
                payload_type: 96,
            },
            sequence_number: 100,
            timestamp: 200,
            ssrc: 0x300,
            csrc_list: vec![],
            extension_profile: Some(0xBEDE),
            extension_data: vec![0xAA, 0xBB, 0xCC], // 3 bytes, padded to 4
            payload: vec![],
        };

        let bytes = packet.to_bytes();
        let parsed = RtpPacket::parse(&bytes).unwrap();

        assert!(parsed.flags.extension);
        assert_eq!(parsed.extension_profile, Some(0xBEDE));
        // Extension data on wire is padded to 32-bit boundary; parser returns wire content
        assert_eq!(parsed.extension_data, vec![0xAA, 0xBB, 0xCC, 0x00]);
    }

    #[test]
    fn test_rtp_with_csrc_list() {
        let packet = RtpPacket {
            flags: RtpHeaderFlags {
                version: 2,
                padding: false,
                extension: false,
                csrc_count: 2,
                marker: false,
                payload_type: 0,
            },
            sequence_number: 42,
            timestamp: 3600,
            ssrc: 0x100,
            csrc_list: vec![0x200, 0x300],
            extension_profile: None,
            extension_data: vec![],
            payload: vec![],
        };

        let bytes = packet.to_bytes();
        let parsed = RtpPacket::parse(&bytes).unwrap();

        assert_eq!(parsed.flags.csrc_count, 2);
        assert_eq!(parsed.csrc_list, vec![0x200, 0x300]);
        assert_eq!(parsed.ssrc, 0x100);
    }

    #[test]
    fn test_rtp_zero_payload() {
        let packet = RtpPacket {
            flags: RtpHeaderFlags::default(),
            sequence_number: 0,
            timestamp: 0,
            ssrc: 0,
            csrc_list: vec![],
            extension_profile: None,
            extension_data: vec![],
            payload: vec![],
        };

        let bytes = packet.to_bytes();
        assert_eq!(bytes.len(), RTP_HEADER_SIZE);
        let parsed = RtpPacket::parse(&bytes).unwrap();
        assert!(parsed.payload.is_empty());
    }

    #[test]
    fn test_rtp_sequence_wraparound() {
        let packet = RtpPacket {
            flags: RtpHeaderFlags::default(),
            sequence_number: 0xFFFF,
            timestamp: 0,
            ssrc: 0,
            csrc_list: vec![],
            extension_profile: None,
            extension_data: vec![],
            payload: vec![0x00],
        };
        let bytes = packet.to_bytes();
        let parsed = RtpPacket::parse(&bytes).unwrap();
        assert_eq!(parsed.sequence_number, 0xFFFF);

        let packet2 = RtpPacket {
            flags: RtpHeaderFlags::default(),
            sequence_number: 0x0000,
            timestamp: 0,
            ssrc: 0,
            csrc_list: vec![],
            extension_profile: None,
            extension_data: vec![],
            payload: vec![0x00],
        };
        let bytes2 = packet2.to_bytes();
        let parsed2 = RtpPacket::parse(&bytes2).unwrap();
        assert_eq!(parsed2.sequence_number, 0x0000);
    }

    // ─── RTP Parse Error Handling ─────────────────────────────────────

    #[test]
    fn test_rtp_short_packet() {
        let err = RtpPacket::parse(&[0x80, 0x00]).unwrap_err();
        assert!(err.to_string().contains("too short"), "Got: {err}");
    }

    #[test]
    fn test_rtp_bad_version() {
        let data = [0x00; 12]; // version=0
        let err = RtpPacket::parse(&data).unwrap_err();
        assert!(err.to_string().contains("version"), "Got: {err}");
    }

    #[test]
    fn test_rtp_truncated_csrc() {
        // V=2, CC=5, but only 12 bytes (= no CSRC entries present)
        let data = [
            0x85, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
        ];
        let err = RtpPacket::parse(&data).unwrap_err();
        assert!(err.to_string().contains("CSRC"), "Got: {err}");
    }

    #[test]
    fn test_rtp_padding_parse() {
        // V=2, P=1, PT=0, seq=1, ts=0, ssrc=1, payload=DEADBEEF, padding=1
        let bytes = vec![
            0xA0, 0x00, // V=2, P=1, X=0, CC=0 | M=0, PT=0
            0x00, 0x01, // seq=1
            0x00, 0x00, 0x00, 0x00, // ts=0
            0x00, 0x00, 0x00, 0x01, // ssrc=1
            0xDE, 0xAD, 0xBE, 0xEF, // payload
            0x01, // padding length = 1
        ];

        let parsed = RtpPacket::parse(&bytes).unwrap();
        // Padding stripped from payload
        assert_eq!(
            parsed.payload,
            vec![0xDE, 0xAD, 0xBE, 0xEF],
            "Padding should be stripped"
        );
        // Original padding flag is preserved
        assert!(parsed.flags.padding);
    }

    #[test]
    fn test_rtp_padding_invalid() {
        // padding=1, pad length byte says 99 but payload is only 5 bytes
        let bytes = vec![
            0xA0, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x02,
            0x03, 0x04, 0x63, // pad_len=99
        ];
        let err = RtpPacket::parse(&bytes).unwrap_err();
        assert!(err.to_string().contains("padding"), "Got: {err}");
    }

    // ─── FU-A Fragmentation / Reassembly ──────────────────────────────

    #[test]
    fn test_fua_single_fragment() {
        let nal_data: Vec<u8> = (0..100).map(|i| (i & 0xFF) as u8).collect();
        let nal_header = 0x65; // forbidden=0, ref_idc=3, type=5 (IDR)

        let mut nal_unit = vec![nal_header];
        nal_unit.extend_from_slice(&nal_data);

        let packets = fragment_nal(&nal_data, 3, 5, 96, 1, 1000, 0xCAFE, 1500).unwrap();
        assert_eq!(packets.len(), 1, "Small NAL should produce 1 fragment");
        assert!(packets[0].flags.marker, "Single packet should have marker");

        let reassembled = reassemble_fua(&packets).unwrap();
        assert_eq!(reassembled, nal_unit);
    }

    #[test]
    fn test_fua_multi_fragment() {
        let nal_data: Vec<u8> = (0..2000).map(|i| (i % 256) as u8).collect();
        let nal_header = 0x65;
        let mut nal_unit = vec![nal_header];
        nal_unit.extend_from_slice(&nal_data);

        let mtu = 500;
        let packets = fragment_nal(&nal_data, 3, 5, 96, 100, 50000, 0xABCD, mtu).unwrap();

        let max_payload = mtu - RTP_HEADER_SIZE - 2;
        let expected_count = nal_data.len().div_ceil(max_payload);
        assert_eq!(packets.len(), expected_count);

        // Verify FU-A markers
        let first_fh = parse_fua_header(packets[0].payload[1]);
        assert!(first_fh.start, "First fragment should have start=1");
        assert!(!first_fh.end, "First fragment should not have end=1");

        let last_fh = parse_fua_header(packets.last().unwrap().payload[1]);
        assert!(!last_fh.start, "Last fragment should not have start=1");
        assert!(last_fh.end, "Last fragment should have end=1");

        // Verify marker bit only on last packet
        for (i, p) in packets.iter().enumerate() {
            assert_eq!(
                p.flags.marker,
                i == packets.len() - 1,
                "Packet {i} marker bit mismatch"
            );
        }

        // Verify sequential sequence numbers
        for (i, p) in packets.iter().enumerate() {
            assert_eq!(p.sequence_number, 100 + i as u16);
        }

        // Reassemble and verify
        let reassembled = reassemble_fua(&packets).unwrap();
        assert_eq!(reassembled.len(), nal_unit.len());
        assert_eq!(reassembled, nal_unit);
    }

    #[test]
    fn test_fua_payload_type_preserved() {
        let nal_data: Vec<u8> = (0..500).map(|i| i as u8).collect();
        let packets = fragment_nal(&nal_data, 3, 5, 106, 1, 0, 0, 200).unwrap();
        for p in &packets {
            assert_eq!(p.flags.payload_type, 106);
        }
    }

    #[test]
    fn test_fua_empty_data() {
        let err = fragment_nal(&[], 3, 5, 96, 1, 0, 0, 1500).unwrap_err();
        assert!(err.to_string().contains("empty"), "Got: {err}");
    }

    #[test]
    fn test_fua_mtu_too_small() {
        let data = vec![0x00; 10];
        let err = fragment_nal(&data, 3, 5, 96, 1, 0, 0, RTP_HEADER_SIZE + 1).unwrap_err();
        assert!(err.to_string().contains("MTU"), "Got: {err}");
    }

    #[test]
    fn test_fua_reassembly_missing_start() {
        let nal_data: Vec<u8> = (0..300).map(|i| i as u8).collect();
        let mut packets = fragment_nal(&nal_data, 3, 5, 96, 1, 0, 0, 100).unwrap();
        packets[0].payload[1] &= !0x80; // Clear start bit on first fragment

        let err = reassemble_fua(&packets).unwrap_err();
        assert!(err.to_string().contains("start"), "Got: {err}");
    }

    #[test]
    fn test_fua_reassembly_ref_idc_mismatch() {
        let nal_data: Vec<u8> = (0..300).map(|i| i as u8).collect();
        let mut packets = fragment_nal(&nal_data, 3, 5, 96, 1, 0, 0, 100).unwrap();
        if packets.len() > 1 {
            packets[1].payload[0] = (1 << 5) | 28; // Change ref_idc from 3 to 1
        }
        let err = reassemble_fua(&packets).unwrap_err();
        assert!(err.to_string().contains("nal_ref_idc"), "Got: {err}");
    }

    #[test]
    fn test_fua_reassembly_empty_packets() {
        let err = reassemble_fua(&[]).unwrap_err();
        assert!(err.to_string().contains("No FU-A packets"), "Got: {err}");
    }

    #[test]
    fn test_fua_reassembly_short_payload() {
        let packet = RtpPacket {
            flags: RtpHeaderFlags::default(),
            sequence_number: 1,
            timestamp: 0,
            ssrc: 0,
            csrc_list: vec![],
            extension_profile: None,
            extension_data: vec![],
            payload: vec![0x7C], // Only FU indicator, no FU header
        };
        let err = reassemble_fua(&[packet]).unwrap_err();
        assert!(err.to_string().contains("too short"), "Got: {err}");
    }

    // ─── FU-A Header/Indicator Build/Parse ────────────────────────────

    #[test]
    fn test_fua_indicator_header_roundtrip() {
        let indicator = build_fua_indicator(3);
        let parsed = parse_fua_indicator(indicator);
        assert_eq!(parsed.nal_ref_idc, 3);
        assert_eq!(parsed.forbidden_zero_bit, 0);

        let header = build_fua_header(true, false, 5);
        let parsed_h = parse_fua_header(header);
        assert!(parsed_h.start);
        assert!(!parsed_h.end);
        assert_eq!(parsed_h.nal_unit_type, 5);
        assert_eq!(parsed_h.reserved, 0);
    }

    #[test]
    fn test_fua_header_all_combinations() {
        let cases = [
            (true, false, 5, "start"),
            (false, true, 5, "end"),
            (true, true, 5, "both"),
            (false, false, 7, "neither"),
        ];
        for (start, end, nal_type, _name) in &cases {
            let h = build_fua_header(*start, *end, *nal_type);
            let p = parse_fua_header(h);
            assert_eq!(p.start, *start, "start mismatch for {_name}");
            assert_eq!(p.end, *end, "end mismatch for {_name}");
            assert_eq!(p.nal_unit_type, *nal_type, "nal_type mismatch for {_name}");
        }
    }

    // ─── STAP-A Build / Parse ─────────────────────────────────────────

    #[test]
    fn test_stapa_single_nal() {
        let nal = vec![0x67, 0x42, 0xC0, 0x1E, 0xD9]; // SPS with header
        let payload = build_stapa(&[&nal]).unwrap();
        let nals = parse_stapa(&payload).unwrap();
        assert_eq!(nals.len(), 1);
        assert_eq!(nals[0], nal);
    }

    #[test]
    fn test_stapa_multiple_nals() {
        let sps = vec![0x67, 0x42, 0xC0, 0x1E, 0xD9]; // SPS
        let pps = vec![0x68, 0xCE, 0x38, 0x80]; // PPS
        let sei = vec![0x66, 0x05, 0x04]; // SEI with ref_idc=3 to match SPS/PPS

        let payload = build_stapa(&[&sps, &pps, &sei]).unwrap();
        let nals = parse_stapa(&payload).unwrap();

        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0], sps);
        assert_eq!(nals[1], pps);
        assert_eq!(nals[2], sei);
    }

    #[test]
    fn test_stapa_empty_list() {
        let err = build_stapa(&[]).unwrap_err();
        assert!(err.to_string().contains("empty"), "Got: {err}");
    }

    #[test]
    fn test_stapa_nal_ref_idc_mismatch() {
        // ref_idc=3 (nal=0x67) vs ref_idc=1 (nal=0x21)
        let nal_a = vec![0x67, 0x42]; // ref_idc=3, type=7
        let nal_b = vec![0x21, 0x42]; // ref_idc=1, type=1
        let err = build_stapa(&[&nal_a, &nal_b]).unwrap_err();
        assert!(err.to_string().contains("nal_ref_idc"), "Got: {err}");
    }

    #[test]
    fn test_stapa_empty_nal_unit_in_list() {
        let err = build_stapa(&[&[0x67, 0x42], &[]]).unwrap_err();
        assert!(err.to_string().contains("empty"), "Got: {err}");
    }

    #[test]
    fn test_stapa_truncated_payload() {
        // Payload that claims a 100-byte NAL but only provides 3 bytes
        let payload = vec![
            (3 << 5) | 24, // STAP-A indicator, ref_idc=3
            0x00,
            0x64, // NAL size = 100
            0x01,
            0x02,
            0x03, // only 3 bytes of NAL data
        ];
        let err = parse_stapa(&payload).unwrap_err();
        assert!(err.to_string().contains("truncated"), "Got: {err}");
    }

    #[test]
    fn test_stapa_zero_length_nal() {
        let payload = vec![
            (3 << 5) | 24, // STAP-A indicator
            0x00,
            0x04, // 4-byte NAL
            0x67,
            0x42,
            0xC0,
            0x1E,
            0x00,
            0x00, // Zero-length NAL
        ];
        let err = parse_stapa(&payload).unwrap_err();
        assert!(err.to_string().contains("zero-length"), "Got: {err}");
    }

    #[test]
    fn test_stapa_invalid_type() {
        // Payload starting with type 5 (IDR), not STAP-A
        let payload = vec![0x65, 0x00, 0x01, 0xFF];
        let err = parse_stapa(&payload).unwrap_err();
        assert!(err.to_string().contains("Not a STAP-A"), "Got: {err}");
    }

    #[test]
    fn test_stapa_empty_payload() {
        let err = parse_stapa(&[]).unwrap_err();
        assert!(err.to_string().contains("Empty"), "Got: {err}");
    }

    // ─── RTP NAL Type Detection ───────────────────────────────────────

    #[test]
    fn test_is_single_nal() {
        assert!(is_single_nal(&[0x65])); // type 5 (IDR)
        assert!(is_single_nal(&[0x67])); // type 7 (SPS)
        assert!(is_single_nal(&[0x41])); // type 1 (Slice, non-IDR)
        assert!(!is_single_nal(&[0x78])); // type 24 (STAP-A)
        assert!(!is_single_nal(&[0x7C])); // type 28 (FU-A)
        assert!(!is_single_nal(&[])); // empty
    }

    #[test]
    fn test_is_stapa() {
        assert!(is_stapa(&[0x78])); // ref_idc=3, type=24
        assert!(!is_stapa(&[0x65])); // type 5
        assert!(!is_stapa(&[0x7C])); // type 28
        assert!(!is_stapa(&[]));
    }

    #[test]
    fn test_is_fua() {
        assert!(is_fua(&[0x7C])); // ref_idc=3, type=28
        assert!(!is_fua(&[0x65])); // type 5
        assert!(!is_fua(&[0x78])); // type 24
        assert!(!is_fua(&[]));
    }

    #[test]
    fn test_get_rtp_nal_type_ok() {
        assert_eq!(get_rtp_nal_type(&[0x65]).unwrap(), RtpNalType::Single(5));
        assert_eq!(get_rtp_nal_type(&[0x78]).unwrap(), RtpNalType::StapA);
        assert_eq!(get_rtp_nal_type(&[0x7C]).unwrap(), RtpNalType::FuA);
    }

    #[test]
    fn test_get_rtp_nal_type_empty() {
        let err = get_rtp_nal_type(&[]).unwrap_err();
        assert!(err.to_string().contains("Empty"), "Got: {err}");
    }

    #[test]
    fn test_get_rtp_nal_type_unsupported() {
        let err = get_rtp_nal_type(&[0x00]).unwrap_err(); // type 0 (unspecified)
        assert!(err.to_string().contains("Unsupported"), "Got: {err}");
    }

    // ─── Integration: Large NAL → FU-A → Reassemble ──────────────────

    #[test]
    fn test_fua_large_nal_integration() {
        // Simulate a large IDR slice (typical size: ~20KB for 720p)
        let nal_data_len = 20_000;
        let nal_data: Vec<u8> = (0..nal_data_len).map(|i| (i & 0xFF) as u8).collect();
        let nal_header = 0x65; // forbidden=0, ref_idc=3, type=5

        let mut original_nal = vec![nal_header];
        original_nal.extend_from_slice(&nal_data);

        let packets = fragment_nal(&nal_data, 3, 5, 96, 0, 42, 0xFEEDFACE, RTP_MTU).unwrap();
        assert!(
            packets.len() > 1,
            "Large NAL should produce >1 fragment, got {}",
            packets.len()
        );

        // Verify all fragments have consistent SSRC and timestamp
        for p in &packets {
            assert_eq!(p.timestamp, 42);
            assert_eq!(p.ssrc, 0xFEEDFACE);
        }

        // Reassemble
        let reassembled = reassemble_fua(&packets).unwrap();
        assert_eq!(reassembled, original_nal);
    }

    // ─── STAP-A Build → Bytes → Parse Integration ────────────────────

    #[test]
    fn test_stapa_bytes_roundtrip() {
        let sps = vec![0x67, 0x42, 0xC0, 0x1E, 0xD9, 0x00, 0x78, 0x02, 0x27, 0xD5];
        let pps = vec![0x68, 0xCE, 0x38, 0x80];
        let sei = vec![0x66, 0x05]; // SEI with ref_idc=3 to match SPS/PPS

        let payload = build_stapa(&[&sps, &pps, &sei]).unwrap();
        let nals = parse_stapa(&payload).unwrap();

        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0], sps);
        assert_eq!(nals[1], pps);
        assert_eq!(nals[2], sei);

        // Validate that all reassembled NALs start with their original NAL headers
        assert_eq!(nals[0][0] & 0x1F, 7, "First NAL should be SPS");
        assert_eq!(nals[1][0] & 0x1F, 8, "Second NAL should be PPS");
        assert_eq!(nals[2][0] & 0x1F, 6, "Third NAL should be SEI");
    }

    // ─── RTP Version 2 Enforcement ────────────────────────────────────

    #[test]
    fn test_rtp_version_must_be_2() {
        // Version 0
        assert!(RtpPacket::parse(&[0x00; 12]).is_err());
        // Version 1
        assert!(RtpPacket::parse(&[0x40; 12]).is_err());
        // Version 3
        assert!(RtpPacket::parse(&[0xC0; 12]).is_err());
        // Version 2 (valid)
        assert!(
            RtpPacket::parse(&[
                0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00
            ])
            .is_ok()
        );
    }
}
