//! PS (Program Stream) and PES packet parsing.
//!
//! Extracts H.264 NAL units from MPEG-2 Program Stream encapsulation
//! used by GB/T 28181 for RTP media transport.

use anyhow::{Result, bail};

/// MPEG-2 Program Stream pack header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsPackHeader {
    /// System Clock Reference (27MHz clock)
    pub scr: u64,
    /// Multiplex rate (50 bytes/sec units)
    pub mux_rate: u32,
    /// Pack stuffing length in bytes
    pub stuffing_length: u8,
}

/// PES (Packetized Elementary Stream) packet header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PesPacket {
    /// Stream ID (0xE0-0xEF for video, 0xC0-0xDF for audio)
    pub stream_id: u8,
    /// Packet length (0 if unbounded)
    pub length: u16,
    /// PTS (Presentation Time Stamp) in 90kHz ticks
    pub pts: Option<u64>,
    /// DTS (Decode Time Stamp) in 90kHz ticks
    pub dts: Option<u64>,
    /// Payload data (elementary stream)
    pub data: Vec<u8>,
}

/// Find all PS start codes in a byte stream.
/// Returns positions of start code prefixes (0x00 0x00 0x01).
fn find_ps_start_codes(data: &[u8]) -> Vec<usize> {
    let mut positions = Vec::new();
    if data.len() < 4 {
        return positions;
    }
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            positions.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    positions
}

/// Parse a PS pack header from data starting at a pack_start_code
/// (0x00 0x00 0x01 0xBA).
///
/// Returns (header, bytes_consumed).
pub fn parse_ps_pack_header(data: &[u8]) -> Result<(PsPackHeader, usize)> {
    if data.len() < 4 || data[0] != 0 || data[1] != 0 || data[2] != 1 || data[3] != 0xBA {
        bail!("Invalid PS pack start code");
    }

    if data.len() < 14 {
        bail!("PS pack header too short");
    }

    let marker_check = data[4];
    let is_mpeg2 = (marker_check & 0xC0) == 0x40; // Bits 7-6 = 01 for MPEG-2

    let scr: u64;
    let _mux_rate: u32;
    let offset: usize;

    if is_mpeg2 {
        // MPEG-2 system stream (recommended by GB28181)
        let b4 = data[4];
        let b5 = data[5];
        let b6 = data[6];
        let b7 = data[7];
        let b8 = data[8];
        let b9 = data[9];
        let b10 = data[10];

        // Reconstruct SCR (33 bits base + 9 bits extension = 42 bits total)
        let scr_base_high = (b4 as u64 & 0x38) >> 3;
        let scr_base_mid_hi = b5 as u64 >> 1;
        let scr_base_mid_lo = (b5 as u64 & 1) << 14 | ((b6 as u64 >> 1) & 0x7FFF);
        let scr_base = (scr_base_high << 30) | (scr_base_mid_hi << 15) | scr_base_mid_lo;

        let _scr_ext = ((b7 as u64 & 0x20) >> 4)
            | ((b7 as u64 & 0x01) << 2)
            | (((b4 as u64 & 0x01) << 1) & 0x01);

        scr = scr_base * 300; // 27MHz = 90kHz * 300

        let mux_rate_val = ((b7 as u32 & 0x3F) << 16) | ((b8 as u32) << 8) | (b9 as u32);
        _mux_rate = mux_rate_val >> 2;

        let stuffing_length = b10 & 0x07;
        offset = 11 + stuffing_length as usize;
    } else {
        scr = 0;
        _mux_rate = 0;
        offset = 12;
    }

    Ok((
        PsPackHeader {
            scr,
            mux_rate: _mux_rate,
            stuffing_length: if is_mpeg2 { data[10] & 0x07 } else { 0 },
        },
        offset,
    ))
}

/// Parse a PES packet from data starting at a packet_start_code_prefix.
///
/// Returns (pes_packet, bytes_consumed).
pub fn parse_pes_packet(data: &[u8]) -> Result<(PesPacket, usize)> {
    if data.len() < 6 {
        bail!("PES packet too short");
    }
    if data[0] != 0 || data[1] != 0 || data[2] != 1 {
        bail!("Invalid PES start code prefix");
    }

    let stream_id = data[3];
    let length = u16::from_be_bytes([data[4], data[5]]);

    if data.len() < 6 + 3 {
        bail!("PES packet truncated before header fields");
    }

    let mut offset = 6; // Start of optional PES header

    // Handle padding stream
    if stream_id == 0xBE {
        if length > 0 {
            offset += length as usize;
        }
        return Ok((
            PesPacket {
                stream_id,
                length,
                pts: None,
                dts: None,
                data: Vec::new(),
            },
            offset.min(data.len()),
        ));
    }

    if (0xC0..=0xEF).contains(&stream_id) {
        // Audio (0xC0-0xDF) or Video (0xE0-0xEF) stream
        if offset + 2 > data.len() {
            bail!("PES packet truncated at optional header fields");
        }

        let pes_header_flags = data[offset];
        let pes_header_length = data[offset + 1] as usize;
        offset += 2;

        if offset + pes_header_length > data.len() {
            bail!("PES packet truncated: optional header length exceeds data");
        }

        let mut pts: Option<u64> = None;
        let mut dts: Option<u64> = None;

        let pts_dts_flags = (pes_header_flags >> 6) & 0x03;

        if pts_dts_flags == 2 || pts_dts_flags == 3 {
            pts = Some(parse_pts_dts(&data[offset..offset + 5]));
        }

        if pts_dts_flags == 3 {
            dts = Some(parse_pts_dts(&data[offset + 5..offset + 10]));
        }

        // Skip remaining optional header fields (stuffing, etc.)
        offset = 6 + 3 + pes_header_length; // 6 bytes prefix+length + 3 bytes header info

        let remaining_len = if length > 0 {
            let header_overhead = offset - 6;
            (length as usize).saturating_sub(header_overhead)
        } else {
            data.len().saturating_sub(offset)
        };

        let payload_end = (offset + remaining_len).min(data.len());
        let payload = data[offset..payload_end].to_vec();

        Ok((
            PesPacket {
                stream_id,
                length,
                pts,
                dts,
                data: payload,
            },
            payload_end,
        ))
    } else if stream_id == 0xBC || (0xB9..=0xBB).contains(&stream_id) {
        // Program stream map, end code, padding
        let payload_end = if length > 0 {
            (6 + length as usize).min(data.len())
        } else {
            data.len()
        };
        Ok((
            PesPacket {
                stream_id,
                length,
                pts: None,
                dts: None,
                data: data[6..payload_end].to_vec(),
            },
            payload_end,
        ))
    } else {
        // Other stream types
        let payload_end = if length > 0 {
            (6 + length as usize).min(data.len())
        } else {
            data.len()
        };
        Ok((
            PesPacket {
                stream_id,
                length,
                pts: None,
                dts: None,
                data: data[6..payload_end].to_vec(),
            },
            payload_end,
        ))
    }
}

/// Parse a 5-byte PTS/DTS value (33 bits packed with marker bits).
fn parse_pts_dts(bytes: &[u8]) -> u64 {
    if bytes.len() < 5 {
        return 0;
    }
    let b0 = bytes[0] as u64;
    let b1 = bytes[1] as u64;
    let b2 = bytes[2] as u64;
    let b3 = bytes[3] as u64;
    let b4 = bytes[4] as u64;

    ((b0 >> 1) & 0x07) << 30
        | (b1 << 22)
        | ((b2 >> 1) & 0x7F) << 15
        | (b3 << 7)
        | ((b4 >> 1) & 0x7F)
}

/// Extract H.264 payload data from a PS (Program Stream) data buffer.
///
/// This parses the MPEG-2 Program Stream encapsulation used by GB/T 28181
/// and returns the H.264 data found within video PES packets.
pub fn parse_ps_to_h264(ps_data: &[u8]) -> Result<Vec<Vec<u8>>> {
    let start_codes = find_ps_start_codes(ps_data);
    if start_codes.is_empty() {
        bail!("No PS start codes found in data");
    }

    let mut h264_data = Vec::new();

    let mut i = 0;
    while i < start_codes.len() {
        let pos = start_codes[i];
        if pos + 3 >= ps_data.len() {
            break;
        }

        let stream_id = ps_data[pos + 3];
        if stream_id == 0xBA || stream_id == 0xBB {
            i += 1;
            continue;
        } else if (0xE0..=0xEF).contains(&stream_id) {
            // Video PES packet
            if let Ok((pes, _consumed)) = parse_pes_packet(&ps_data[pos..]) {
                if !pes.data.is_empty() {
                    h264_data.push(pes.data);
                }
            }
            i += 1;
        } else {
            i += 1;
        }
    }

    Ok(h264_data)
}

/// Extract H.264 NAL units from a PS stream.
///
/// This function first extracts PES payloads (PS to PES), then finds
/// Annex B NAL units within the concatenated H.264 data.
pub fn parse_ps_to_nal_units(ps_data: &[u8]) -> Result<Vec<Vec<u8>>> {
    let pes_payloads = parse_ps_to_h264(ps_data)?;

    if pes_payloads.is_empty() {
        return Ok(Vec::new());
    }

    let total_size: usize = pes_payloads.iter().map(|d| d.len()).sum();
    let mut combined = Vec::with_capacity(total_size);
    for payload in &pes_payloads {
        combined.extend_from_slice(payload);
    }

    // Find Annex B start codes and extract NAL units using existing H.264 parser
    let nal_units = crate::h264::split_nal_units(&combined);
    let result: Vec<Vec<u8>> = nal_units.into_iter().map(|nal| nal.to_vec()).collect();
    Ok(result)
}
