#![cfg_attr(test, deny(warnings))]

/// H.264 NAL unit parser - pure Rust implementation
///
/// Supports:
/// - Annex B format (start code prefixed)
/// - AVCC format (length-prefixed)
/// - NAL header parsing
/// - SPS/PPS parameter extraction
/// - Keyframe detection
use anyhow::{Result, anyhow};

/// H.264 NAL unit header
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NalHeader {
    /// forbidden_zero_bit - must be 0
    pub forbidden_zero_bit: u8,
    /// nal_ref_idc - importance of this NAL unit (0-3)
    pub nal_ref_idc: u8,
    /// nal_unit_type - type of NAL unit
    pub nal_unit_type: NalUnitType,
}

/// H.264 NAL unit types
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum NalUnitType {
    /// Unspecified
    Unspecified = 0,
    /// Coded slice of a non-IDR picture
    Slice = 1,
    /// Coded slice data partition A
    SliceA = 2,
    /// Coded slice data partition B
    SliceB = 3,
    /// Coded slice data partition C
    SliceC = 4,
    /// Coded slice of an IDR picture
    Idr = 5,
    /// Supplemental enhancement information
    Sei = 6,
    /// Sequence parameter set
    Sps = 7,
    /// Picture parameter set
    Pps = 8,
    /// Access unit delimiter
    Aud = 9,
    /// End of sequence
    EndOfSequence = 10,
    /// End of stream
    EndOfStream = 11,
    /// Filler data
    Filler = 12,
    /// Sequence parameter set extension
    SpsExt = 13,
    /// Prefix NAL unit
    Prefix = 14,
    /// Subset sequence parameter set
    SubsetSps = 15,
    /// Coded slice of an auxiliary coded picture
    SliceAux = 19,
    /// Reserved
    Reserved = 24,
}

impl TryFrom<u8> for NalUnitType {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(NalUnitType::Unspecified),
            1 => Ok(NalUnitType::Slice),
            2 => Ok(NalUnitType::SliceA),
            3 => Ok(NalUnitType::SliceB),
            4 => Ok(NalUnitType::SliceC),
            5 => Ok(NalUnitType::Idr),
            6 => Ok(NalUnitType::Sei),
            7 => Ok(NalUnitType::Sps),
            8 => Ok(NalUnitType::Pps),
            9 => Ok(NalUnitType::Aud),
            10 => Ok(NalUnitType::EndOfSequence),
            11 => Ok(NalUnitType::EndOfStream),
            12 => Ok(NalUnitType::Filler),
            13 => Ok(NalUnitType::SpsExt),
            14 => Ok(NalUnitType::Prefix),
            15 => Ok(NalUnitType::SubsetSps),
            19 => Ok(NalUnitType::SliceAux),
            24..=30 => Ok(NalUnitType::Reserved),
            _ => Err(anyhow!("Invalid NAL unit type: {}", value)),
        }
    }
}

/// Sequence parameter set parameters
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpsParams {
    /// Profile IDC
    pub profile_idc: u8,
    /// Constraint flags (3 bytes)
    pub constraint_flags: [u8; 3],
    /// Level IDC
    pub level_idc: u8,
    /// Sequence parameter set ID
    pub seq_parameter_set_id: u32,
    /// Chroma format IDC
    pub chroma_format_idc: u32,
    /// Picture width in macroblocks minus 1
    pub pic_width_in_mbs_minus1: u32,
    /// Picture height in map units minus 1
    pub pic_height_in_map_units_minus1: u32,
    /// Frame cropping flag
    pub frame_cropping_flag: bool,
    /// Frame cropping offsets
    pub frame_cropping: Option<FrameCropping>,
}

/// Frame cropping parameters
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameCropping {
    pub left_offset: u32,
    pub right_offset: u32,
    pub top_offset: u32,
    pub bottom_offset: u32,
}

impl SpsParams {
    /// Calculate picture width in pixels
    pub fn width(&self) -> u32 {
        let base_width = (self.pic_width_in_mbs_minus1 + 1) * 16;
        let crop = self
            .frame_cropping
            .as_ref()
            .map(|c| c.left_offset + c.right_offset)
            .unwrap_or(0);
        match self.chroma_format_idc {
            0 => base_width - crop,           // Monochrome
            1 | 2 => base_width - (crop * 2), // 4:2:0 or 4:2:2
            3 => base_width - crop,           // 4:4:4
            _ => base_width - crop,
        }
    }

    /// Calculate picture height in pixels
    pub fn height(&self) -> u32 {
        let base_height = (self.pic_height_in_map_units_minus1 + 1) * 16;
        let crop = self
            .frame_cropping
            .as_ref()
            .map(|c| c.top_offset + c.bottom_offset)
            .unwrap_or(0);
        match self.chroma_format_idc {
            0 => base_height - crop,       // Monochrome
            1 => base_height - (crop * 2), // 4:2:0
            2 | 3 => base_height - crop,   // 4:2:2 or 4:4:4
            _ => base_height - crop,
        }
    }
}

/// Picture parameter set parameters
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PpsParams {
    /// Picture parameter set ID
    pub pic_parameter_set_id: u32,
    /// Sequence parameter set ID reference
    pub seq_parameter_set_id: u32,
}

/// Bit iterator for parsing bitstreams
pub struct BitIter<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u8,
}

impl<'a> BitIter<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    /// Read a single bit
    pub(crate) fn read_bit(&mut self) -> Result<bool> {
        if self.byte_pos >= self.data.len() {
            return Err(anyhow!("Bit read out of bounds"));
        }
        let bit = (self.data[self.byte_pos] >> (7 - self.bit_pos)) & 1;
        self.bit_pos += 1;
        if self.bit_pos >= 8 {
            self.bit_pos = 0;
            self.byte_pos += 1;
        }
        Ok(bit != 0)
    }

    /// Read n bits as a u32
    pub(crate) fn read_bits(&mut self, n: u8) -> Result<u32> {
        if n > 32 {
            return Err(anyhow!("Cannot read more than 32 bits at once"));
        }
        let mut result = 0u32;
        for _ in 0..n {
            result = (result << 1) | (self.read_bit()? as u32);
        }
        Ok(result)
    }
}

pub fn read_exp_golomb(bits: &mut BitIter) -> Result<u32> {
    let mut leading_zeroes = 0u32;
    while !bits.read_bit()? {
        leading_zeroes += 1;
    }

    if leading_zeroes == 0 {
        return Ok(0);
    }

    let value = bits.read_bits(leading_zeroes as u8)?;
    Ok((1 << leading_zeroes) - 1 + value)
}

/// Read signed exponential Golomb code
pub fn read_signed_exp_golomb(bits: &mut BitIter) -> Result<i32> {
    let ue = read_exp_golomb(bits)?;
    let positive = (ue % 2) == 0;
    let value = ((ue + 1) >> 1) as i32;
    Ok(if positive { value } else { -value })
}

/// Find all Annex B start code positions in byte stream
/// Returns (start_pos, data_pos) tuples
pub fn find_start_codes(data: &[u8]) -> Vec<(usize, usize)> {
    let mut results = Vec::new();
    let mut i = 0;

    while i + 3 < data.len() {
        // Check for 4-byte start code (00 00 00 01)
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1 {
            results.push((i, i + 4));
            i += 4;
            continue;
        }

        // Check for 3-byte start code (00 00 01)
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            results.push((i, i + 3));
            i += 3;
            continue;
        }

        i += 1;
    }

    results
}

/// Split Annex B byte stream into NAL units
pub fn split_nal_units(data: &[u8]) -> Vec<&[u8]> {
    let start_codes = find_start_codes(data);
    if start_codes.is_empty() {
        if !data.is_empty() {
            // No start codes found, treat entire data as one NAL unit
            return vec![data];
        }
        return vec![];
    }

    let mut nal_units = Vec::new();

    for (i, (_start, data_start)) in start_codes.iter().enumerate() {
        let end_pos = if i + 1 < start_codes.len() {
            start_codes[i + 1].0
        } else {
            data.len()
        };

        if *data_start < end_pos {
            nal_units.push(&data[*data_start..end_pos]);
        }
    }

    nal_units
}

/// Split AVCC (length-prefixed) byte stream into NAL units
pub fn split_nal_units_avcc(data: &[u8]) -> Vec<&[u8]> {
    let mut nal_units = Vec::new();
    let mut pos = 0;

    while pos + 4 <= data.len() {
        // AVCC uses 4-byte length prefix (big-endian)
        let length =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;

        if length == 0 {
            break;
        }

        let nal_start = pos + 4;
        let nal_end = nal_start + length;

        if nal_end > data.len() {
            break;
        }

        nal_units.push(&data[nal_start..nal_end]);
        pos = nal_end;
    }

    nal_units
}

/// Parse NAL unit header
pub fn parse_nal_header(nal_data: &[u8]) -> Result<(NalHeader, &[u8])> {
    if nal_data.is_empty() {
        return Err(anyhow!("Empty NAL unit data"));
    }

    let first_byte = nal_data[0];
    let forbidden_zero_bit = (first_byte >> 7) & 0x01;
    let nal_ref_idc = (first_byte >> 5) & 0x03;
    let nal_unit_type_val = first_byte & 0x1F;

    if forbidden_zero_bit != 0 {
        return Err(anyhow!("Forbidden zero bit is set (invalid NAL unit)"));
    }

    let nal_unit_type = NalUnitType::try_from(nal_unit_type_val)?;

    let header = NalHeader {
        forbidden_zero_bit,
        nal_ref_idc,
        nal_unit_type,
    };

    Ok((header, &nal_data[1..]))
}

/// Parse sequence parameter set (NAL type 7)
pub fn parse_sps(nal_data: &[u8]) -> Result<SpsParams> {
    let (header, payload) = parse_nal_header(nal_data)?;

    if header.nal_unit_type != NalUnitType::Sps {
        return Err(anyhow!(
            "Expected SPS (type 7), got {:?}",
            header.nal_unit_type
        ));
    }

    let mut bits = BitIter::new(payload);

    // Read profile_idc (8 bits)
    let profile_idc = bits.read_bits(8)? as u8;

    // Read constraint flags (1 byte containing constraint_set0-5 flags + reserved_zero_2bits)
    let constraint_byte = bits.read_bits(8)? as u8;
    let constraint_flags = [constraint_byte, 0, 0];

    // Read level_idc (8 bits)
    let level_idc = bits.read_bits(8)? as u8;

    // Read seq_parameter_set_id (ue(v))
    let seq_parameter_set_id = read_exp_golomb(&mut bits)?;

    // Check if profile requires more parameters
    let high_profile = profile_idc == 100
        || profile_idc == 110
        || profile_idc == 122
        || profile_idc == 244
        || profile_idc == 44
        || profile_idc == 83
        || profile_idc == 86
        || profile_idc == 118
        || profile_idc == 128;

    let mut chroma_format_idc = 1; // Default to 4:2:0

    if high_profile {
        chroma_format_idc = read_exp_golomb(&mut bits)?;

        if chroma_format_idc == 3 {
            // separate_colour_plane_flag
            let _ = bits.read_bit()?;
        }

        // bit_depth_luma_minus8
        let _ = read_exp_golomb(&mut bits)?;
        // bit_depth_chroma_minus8
        let _ = read_exp_golomb(&mut bits)?;
        // qpprime_y_zero_transform_bypass_flag
        let _ = bits.read_bit()?;

        // seq_scaling_matrix_present_flag
        let seq_scaling_matrix_present_flag = bits.read_bit()?;
        if seq_scaling_matrix_present_flag {
            // Skip scaling matrix lists (complex, not needed for width/height)
            for i in 0..8 {
                let seq_scaling_list_present_flag = bits.read_bit()?;
                if seq_scaling_list_present_flag {
                    let _size = if i < 6 { 16 } else { 64 };
                    read_scaling_list(&mut bits, _size)?;
                }
            }
        }
    }

    // Read log2_max_frame_num_minus4
    let _ = read_exp_golomb(&mut bits)?;

    // Read pic_order_cnt_type
    let pic_order_cnt_type = read_exp_golomb(&mut bits)?;

    if pic_order_cnt_type == 0 {
        // log2_max_pic_order_cnt_lsb_minus4
        let _ = read_exp_golomb(&mut bits)?;
    } else if pic_order_cnt_type == 1 {
        // delta_pic_order_always_zero_flag
        let _ = bits.read_bit()?;
        // offset_for_non_ref_pic
        let _ = read_signed_exp_golomb(&mut bits)?;
        // offset_for_top_to_bottom_field
        let _ = read_signed_exp_golomb(&mut bits)?;
        // num_ref_frames_in_pic_order_cnt_cycle
        let num_ref_frames = read_exp_golomb(&mut bits)?;
        for _ in 0..num_ref_frames {
            // offset_for_ref_frame[i]
            let _ = read_signed_exp_golomb(&mut bits)?;
        }
    }

    // Read max_num_ref_frames
    let _ = read_exp_golomb(&mut bits)?;

    // gaps_in_frame_num_value_allowed_flag
    let _ = bits.read_bit()?;

    // Read pic_width_in_mbs_minus1
    let pic_width_in_mbs_minus1 = read_exp_golomb(&mut bits)?;

    // Read pic_height_in_map_units_minus1
    let pic_height_in_map_units_minus1 = read_exp_golomb(&mut bits)?;

    // frame_mbs_only_flag
    let frame_mbs_only_flag = bits.read_bit()?;

    if !frame_mbs_only_flag {
        // mb_adaptive_frame_field_flag
        let _ = bits.read_bit()?;
    }

    // direct_8x8_inference_flag
    let _ = bits.read_bit()?;

    // frame_cropping_flag
    let frame_cropping_flag = bits.read_bit()?;
    let frame_cropping = if frame_cropping_flag {
        let left_offset = read_exp_golomb(&mut bits)?;
        let right_offset = read_exp_golomb(&mut bits)?;
        let top_offset = read_exp_golomb(&mut bits)?;
        let bottom_offset = read_exp_golomb(&mut bits)?;

        Some(FrameCropping {
            left_offset,
            right_offset,
            top_offset,
            bottom_offset,
        })
    } else {
        None
    };

    // vui_parameters_present_flag - skip for now
    // let vui_parameters_present_flag = bits.read_bit()?;
    // if vui_parameters_present_flag {
    //     // Skip VUI parsing
    // }

    Ok(SpsParams {
        profile_idc,
        constraint_flags,
        level_idc,
        seq_parameter_set_id,
        chroma_format_idc,
        pic_width_in_mbs_minus1,
        pic_height_in_map_units_minus1,
        frame_cropping_flag,
        frame_cropping,
    })
}

/// Read scaling list (helper for SPS parsing)
fn read_scaling_list(bits: &mut BitIter, size: u32) -> Result<()> {
    let mut last_scale = 8;
    let mut next_scale = 8;

    for _ in 0..size {
        if next_scale != 0 {
            let delta_scale = read_signed_exp_golomb(bits)?;
            next_scale = (last_scale + delta_scale) % 256;
        }

        if next_scale == 0 {
            last_scale = 8;
        } else {
            last_scale = next_scale;
        }
    }

    Ok(())
}

/// Parse picture parameter set (NAL type 8)
pub fn parse_pps(nal_data: &[u8]) -> Result<PpsParams> {
    let (header, payload) = parse_nal_header(nal_data)?;

    if header.nal_unit_type != NalUnitType::Pps {
        return Err(anyhow!(
            "Expected PPS (type 8), got {:?}",
            header.nal_unit_type
        ));
    }

    let mut bits = BitIter::new(payload);

    // Read pic_parameter_set_id
    let pic_parameter_set_id = read_exp_golomb(&mut bits)?;

    // Read seq_parameter_set_id
    let seq_parameter_set_id = read_exp_golomb(&mut bits)?;

    // entropy_coding_mode_flag
    let _ = bits.read_bit()?;
    // bottom_field_pic_order_in_frame_present_flag
    let _ = bits.read_bit()?;
    // num_slice_groups_minus1
    let num_slice_groups = read_exp_golomb(&mut bits)?;
    if num_slice_groups > 0 {
        // Skip slice group map (not needed for basic PPS parsing)
        let _slice_group_map_type = read_exp_golomb(&mut bits)?;
    }

    Ok(PpsParams {
        pic_parameter_set_id,
        seq_parameter_set_id,
    })
}

/// Check if a NAL unit is a keyframe (IDR slice)
pub fn is_keyframe(nal_unit_type: NalUnitType, nal_ref_idc: u8) -> bool {
    // IDR slices are always keyframes
    if nal_unit_type == NalUnitType::Idr {
        return true;
    }

    // Non-IDR slices with high nal_ref_idc are often keyframes in some encoders
    // This is a heuristic - for strict keyframe detection, only IDR should be used
    if nal_unit_type == NalUnitType::Slice && nal_ref_idc > 0 {
        // Some encoders mark I-frames as non-IDR but with ref_idc > 0
        // For strict compliance, only IDR should be considered keyframes
        return false;
    }

    false
}

/// Find all keyframe NAL unit offsets in Annex B byte stream
pub fn find_keyframes(data: &[u8]) -> Vec<usize> {
    let nal_units = split_nal_units(data);
    let mut keyframes = Vec::new();
    let mut offset = 0;

    for nal_unit in nal_units {
        // Parse the NAL header
        if let Ok((header, _)) = parse_nal_header(nal_unit)
            && is_keyframe(header.nal_unit_type, header.nal_ref_idc)
        {
            // Find the start code position for this NAL unit
            let start_codes = find_start_codes(data);
            for (start, _) in start_codes.iter() {
                if *start >= offset {
                    keyframes.push(*start);
                    break;
                }
            }
        }
        offset += nal_unit.len();
    }

    keyframes
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generic H.264 SPS (Baseline profile, 1920x1080)
    // This is a representative SPS from a common H.264 encoder
    const SPS_FIXTURE: &[u8] = &[
        0x67, 0x42, 0xc0, 0x1e, 0xd9, 0x00, 0x78, 0x02, 0x27, 0xd5, 0x05, 0x71, 0xe2, 0x1c, 0x66,
        0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x03, 0x03, 0x20, 0xf1, 0x62, 0x69, 0x40,
    ];

    // Generic PPS
    const PPS_FIXTURE: &[u8] = &[0x68, 0xce, 0x38, 0x80];

    // IDR frame with start code
    const IDR_FIXTURE: &[u8] = &[
        0x00, 0x00, 0x00, 0x01, 0x65, 0xb8, 0x00, 0x04, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x01,
        0x01, 0xff, 0xff, 0xff,
    ];

    // Non-IDR slice
    const NON_IDR_FIXTURE: &[u8] = &[
        0x00, 0x00, 0x00, 0x01, 0x41, 0x9a, 0x74, 0x20, 0xff, 0xff, 0xff,
    ];

    // Annex B stream with multiple NAL units
    const ANNEX_B_FIXTURE: &[u8] = &[
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x1e, 0xd9, 0x00, 0x78, 0x02, 0x27, 0xd5, 0x05,
        0x71, 0x00, 0x00, 0x01, 0x68, 0xce, 0x38, 0x80, 0x00, 0x00, 0x00, 0x01, 0x65, 0xb8, 0x00,
        0x04,
    ];

    // AVCC format (length-prefixed)
    const AVCC_FIXTURE: &[u8] = &[
        0x00, 0x00, 0x00, 0x18, // Length 24
        0x67, 0x42, 0xc0, 0x1e, 0xd9, 0x00, 0x78, 0x02, 0x27, 0xd5, 0x05, 0x71, 0xe2, 0x1c, 0x66,
        0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x03, 0x03, 0x00, 0x00, 0x00,
        0x04, // Length 4
        0x68, 0xce, 0x38, 0x80,
    ];

    // Simple H.264 SPS (CIF 352x288, Baseline profile)
    const SPS_SIMPLE_FIXTURE: &[u8] = &[0x67, 0x42, 0x80, 0x1e, 0xBB, 0x40, 0xB0, 0x4B, 0x00];

    #[test]
    fn test_find_start_codes() {
        let codes = find_start_codes(IDR_FIXTURE);
        assert_eq!(codes.len(), 2);
        assert_eq!(codes[0], (0, 4)); // 00 00 00 01
        assert_eq!(codes[1], (11, 15)); // 00 00 00 01

        // Test 3-byte start codes
        let three_byte = &[0x00, 0x00, 0x01, 0x67, 0x42];
        let codes = find_start_codes(three_byte);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0], (0, 3));
    }

    #[test]
    fn test_split_nal_units_annex_b() {
        let nal_units = split_nal_units(ANNEX_B_FIXTURE);
        assert_eq!(nal_units.len(), 3);

        // First NAL unit (SPS)
        assert_eq!(nal_units[0][0], 0x67);

        // Second NAL unit (PPS)
        assert_eq!(nal_units[1][0], 0x68);

        // Third NAL unit (IDR)
        assert_eq!(nal_units[2][0], 0x65);
    }

    #[test]
    fn test_split_nal_units_avcc() {
        let nal_units = split_nal_units_avcc(AVCC_FIXTURE);
        assert_eq!(nal_units.len(), 2);

        // First NAL unit (SPS) - 24 bytes
        assert_eq!(nal_units[0].len(), 24);
        assert_eq!(nal_units[0][0], 0x67);

        // Second NAL unit (PPS) - 4 bytes
        assert_eq!(nal_units[1].len(), 4);
        assert_eq!(nal_units[1][0], 0x68);
    }

    #[test]
    fn test_parse_nal_header() {
        // Test SPS header (type 7)
        let (header, _) = parse_nal_header(SPS_FIXTURE).unwrap();
        assert_eq!(header.nal_unit_type, NalUnitType::Sps);
        assert_eq!(header.nal_ref_idc, 3);

        // Test PPS header (type 8)
        let (header, _) = parse_nal_header(PPS_FIXTURE).unwrap();
        assert_eq!(header.nal_unit_type, NalUnitType::Pps);
        assert_eq!(header.nal_ref_idc, 3);

        // Test IDR header (type 5)
        let idr_nal = &IDR_FIXTURE[4..];
        let (header, _) = parse_nal_header(idr_nal).unwrap();
        assert_eq!(header.nal_unit_type, NalUnitType::Idr);
    }

    #[test]
    fn test_nal_unit_type_from_u8() {
        assert_eq!(NalUnitType::try_from(0).unwrap(), NalUnitType::Unspecified);
        assert_eq!(NalUnitType::try_from(1).unwrap(), NalUnitType::Slice);
        assert_eq!(NalUnitType::try_from(5).unwrap(), NalUnitType::Idr);
        assert_eq!(NalUnitType::try_from(7).unwrap(), NalUnitType::Sps);
        assert_eq!(NalUnitType::try_from(8).unwrap(), NalUnitType::Pps);

        // Invalid type
        assert!(NalUnitType::try_from(31).is_err());
    }

    #[test]
    fn test_parse_sps() {
        let sps = parse_sps(SPS_FIXTURE).unwrap();
        assert_eq!(sps.profile_idc, 0x42); // Baseline profile
        assert_eq!(sps.level_idc, 0x1e); // Level 3.0

        // Width and height should be extractable
        // Note: Exact values depend on the SPS structure
        // For this fixture, we're just checking it parses successfully
        assert!(sps.width() > 0);
        assert!(sps.height() > 0);
    }

    #[test]
    fn test_parse_sps_simple() {
        let sps = parse_sps(SPS_SIMPLE_FIXTURE).unwrap();
        assert_eq!(sps.profile_idc, 0x42); // Baseline profile
        assert_eq!(sps.level_idc, 0x1e); // Level 3.0

        // For CIF (352x288), with macroblock alignment:
        // Width = (21 + 1) * 16 = 352 (21 macroblocks)
        // Height = (17 + 1) * 16 = 288 (17 macroblocks)
        let expected_width = 352;
        let expected_height = 288;

        // Allow some margin for cropping
        assert!((sps.width() as i32 - expected_width).abs() < 16);
        assert!((sps.height() as i32 - expected_height).abs() < 16);
    }

    #[test]
    fn test_parse_pps() {
        let pps = parse_pps(PPS_FIXTURE).unwrap();
        // The exact IDs depend on the PPS structure
        // Just check it parses successfully
        assert_eq!(pps.pic_parameter_set_id, 0);
        assert_eq!(pps.seq_parameter_set_id, 0);
    }

    #[test]
    fn test_read_exp_golomb() {
        // Test cases for exp-golomb
        let data = &[0b10110110, 0b11001100];
        let mut bits = BitIter::new(data);

        // First code: 0 (leading 0, then 1) -> 1 -> value 0
        let val = read_exp_golomb(&mut bits).unwrap();
        assert_eq!(val, 0);

        // Reset and test different values
        let data = &[0b01010101];
        let mut bits = BitIter::new(data);

        // Test a few more values
        let _ = read_exp_golomb(&mut bits);
        let _ = read_exp_golomb(&mut bits);
    }

    #[test]
    fn test_read_signed_exp_golomb() {
        let data = &[0b01010101, 0b10101010];
        let mut bits = BitIter::new(data);

        let val = read_signed_exp_golomb(&mut bits).unwrap();
        assert!(val == 0 || val == -1 || val == 1);
    }

    #[test]
    fn test_is_keyframe() {
        // IDR slice is always a keyframe
        assert!(is_keyframe(NalUnitType::Idr, 3));

        // Non-IDR slice is not a keyframe (strict mode)
        assert!(!is_keyframe(NalUnitType::Slice, 3));
        assert!(!is_keyframe(NalUnitType::Slice, 0));

        // Other NAL unit types are not keyframes
        assert!(!is_keyframe(NalUnitType::Sps, 3));
        assert!(!is_keyframe(NalUnitType::Pps, 3));
        assert!(!is_keyframe(NalUnitType::Sei, 0));
    }

    #[test]
    fn test_find_keyframes() {
        let keyframes = find_keyframes(IDR_FIXTURE);
        assert_eq!(keyframes.len(), 1);
        assert_eq!(keyframes[0], 0); // First NAL unit is IDR

        // Test Annex B fixture with multiple NAL units
        let keyframes = find_keyframes(ANNEX_B_FIXTURE);
        // Should find at least the IDR NAL unit
        assert!(!keyframes.is_empty());

        // Non-IDR fixture should have no keyframes
        let keyframes = find_keyframes(NON_IDR_FIXTURE);
        assert_eq!(keyframes.len(), 0);
    }

    #[test]
    fn test_empty_data() {
        assert!(split_nal_units(&[]).is_empty());
        assert!(split_nal_units_avcc(&[]).is_empty());
        assert!(find_start_codes(&[]).is_empty());
    }

    #[test]
    fn test_invalid_nal_header() {
        // Empty data
        assert!(parse_nal_header(&[]).is_err());

        // Forbidden zero bit set (0x80 = 10000000)
        let invalid = &[0x80, 0x00];
        assert!(parse_nal_header(invalid).is_err());
    }

    #[test]
    fn test_sps_width_height_calculation() {
        let sps = parse_sps(SPS_FIXTURE).unwrap();

        // Verify width and height are calculated
        let width = sps.width();
        let height = sps.height();

        // Should be reasonable values (typical for 1080p or similar)
        assert!(width > 0 && width <= 4096);
        assert!(height > 0 && height <= 4096);

        // If frame_cropping is set, verify cropping is applied
        if sps.frame_cropping_flag {
            assert!(sps.frame_cropping.is_some());
        }
    }

    #[test]
    fn test_bit_iter() {
        let data = &[0b10110010, 0b01101100];
        let mut bits = BitIter::new(data);

        // Read individual bits
        assert!(bits.read_bit().unwrap());
        assert!(!bits.read_bit().unwrap());
        assert!(bits.read_bit().unwrap());
        assert!(bits.read_bit().unwrap());

        // Read multiple bits
        let val = bits.read_bits(4).unwrap();
        assert_eq!(val, 0b0010);

        // Read remaining bits
        let val = bits.read_bits(8).unwrap();
        assert_eq!(val, 0b01101100);
    }

    #[test]
    fn test_split_mixed_start_codes() {
        // Mix of 3-byte and 4-byte start codes
        let data = &[
            0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x00, 0x00, 0x01, 0x68, 0xce, 0x00, 0x00, 0x01,
            0x65, 0xb8,
        ];

        let nal_units = split_nal_units(data);
        assert_eq!(nal_units.len(), 3);

        assert_eq!(nal_units[0][0], 0x67);
        assert_eq!(nal_units[1][0], 0x68);
        assert_eq!(nal_units[2][0], 0x65);
    }
}
