/// G.711 μ-law (u-law) encoding constants
const BIAS: i32 = 132;
const CLIP: i32 = 32635;

/// Compress 16-bit PCM sample to 8-bit μ-law (ITU-T G.711)
pub fn pcm_to_mulaw(sample: i16) -> u8 {
    let sign = if sample < 0 { 0x80u8 } else { 0u8 };
    let mut mag = (sample.unsigned_abs() as i32).min(CLIP);
    mag += BIAS;

    let segment = if mag >= 0x1000 {
        7
    } else if mag >= 0x800 {
        6
    } else if mag >= 0x400 {
        5
    } else if mag >= 0x200 {
        4
    } else if mag >= 0x100 {
        3
    } else if mag >= 0x80 {
        2
    } else if mag >= 0x40 {
        1
    } else {
        0
    };

    let mantissa = ((mag >> (segment + 3)) & 0x0F) as u8;
    let encoded = sign | ((segment as u8) << 4) | mantissa;
    !encoded
}

/// Decompress 8-bit μ-law to 16-bit PCM sample (ITU-T G.711)
pub fn mulaw_to_pcm(encoded: u8) -> i16 {
    let u = !(encoded as i32) & 0xFF;
    let sign = (u >> 7) & 1;
    let segment = (u >> 4) & 0x07;
    let mantissa = u & 0x0F;

    let mag = (mantissa << (segment + 3)) + (1 << (segment + 2));

    let sample = if sign != 0 { BIAS - mag } else { mag - BIAS };
    (sample as i16).clamp(-32768_i16, 32767_i16)
}

/// Compress 16-bit PCM sample to 8-bit A-law (ITU-T G.711)
pub fn pcm_to_alaw(sample: i16) -> u8 {
    let sign = (sample >> 8) & 0x80;
    let mut mag = sample.unsigned_abs() as i32;
    mag = mag.min(CLIP);

    let encoded = if mag <= 0x0F {
        mag as u8
    } else {
        let segment = if mag >= 0x800 {
            7
        } else if mag >= 0x400 {
            6
        } else if mag >= 0x200 {
            5
        } else if mag >= 0x100 {
            4
        } else if mag >= 0x80 {
            3
        } else if mag >= 0x40 {
            2
        } else if mag >= 0x20 {
            1
        } else {
            0
        };

        let mantissa = ((mag >> (segment + 3)) & 0x0F) as u8;
        ((segment as u8) << 4) | mantissa
    };

    (sign as u8) | encoded
}

/// Decompress 8-bit A-law to 16-bit PCM sample (ITU-T G.711)
pub fn alaw_to_pcm(encoded: u8) -> i16 {
    let sign = (encoded & 0x80) as i16;
    let segment = ((encoded >> 4) & 0x07) as i32;
    let mantissa = (encoded & 0x0F) as i32;

    let mag = (mantissa << (segment + 3)) + (1 << (segment + 2));

    if sign != 0 { -(mag as i16) } else { mag as i16 }
}

/// RTP header structure
#[derive(Debug, Clone)]
struct RtpHeader {
    version: u8,
    padding: bool,
    extension: bool,
    csrc_count: u8,
    marker: bool,
    payload_type: u8,
    sequence_number: u16,
    timestamp: u32,
    ssrc: u32,
}

impl RtpHeader {
    fn new(payload_type: u8, sequence_number: u16, timestamp: u32, ssrc: u32) -> Self {
        Self {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type,
            sequence_number,
            timestamp,
            ssrc,
        }
    }

    fn to_bytes(&self) -> [u8; 12] {
        let mut bytes = [0u8; 12];

        // Byte 0: V (2) + P (1) + X (1) + CC (4)
        bytes[0] = (self.version << 6)
            | ((self.padding as u8) << 5)
            | ((self.extension as u8) << 4)
            | self.csrc_count;

        // Byte 1: M (1) + PT (7)
        bytes[1] = ((self.marker as u8) << 7) | self.payload_type;

        // Bytes 2-3: Sequence number
        bytes[2..4].copy_from_slice(&self.sequence_number.to_be_bytes());

        // Bytes 4-7: Timestamp
        bytes[4..8].copy_from_slice(&self.timestamp.to_be_bytes());

        // Bytes 8-11: SSRC
        bytes[8..12].copy_from_slice(&self.ssrc.to_be_bytes());

        bytes
    }
}

/// RTP audio packetizer for G.711 μ-law/A-law
#[derive(Debug, Clone)]
pub struct RtpAudioPacketizer {
    sequence_number: u16,
}

impl RtpAudioPacketizer {
    /// Create a new RTP audio packetizer
    pub fn new() -> Self {
        Self { sequence_number: 0 }
    }

    /// Reset sequence number
    pub fn reset(&mut self) {
        self.sequence_number = 0;
    }

    /// Packetize PCM samples into RTP packets with μ-law encoding
    ///
    /// # Arguments
    /// * `pcm_samples` - Raw 16-bit PCM samples
    /// * `sample_rate` - Sample rate in Hz (8000 for G.711 standard)
    /// * `channels` - Number of audio channels (1 for mono, 2 for stereo)
    /// * `timestamp` - Initial RTP timestamp
    /// * `ssrc` - Synchronization source identifier
    ///
    /// # Returns
    /// Vector of RTP packets (header + payload)
    ///
    /// # Note
    /// G.711 is designed for 8kHz audio. For other sample rates, this function
    /// will document the limitation but will still packetize (may require resampling).
    pub fn packetize_pcm_ulaw(
        &mut self,
        pcm_samples: &[i16],
        sample_rate: u32,
        channels: u16,
        timestamp: u32,
        ssrc: u32,
    ) -> Vec<Vec<u8>> {
        self.packetize_pcm(pcm_samples, sample_rate, channels, timestamp, ssrc, 0) // Payload type 0 = PCMU
    }

    /// Packetize PCM samples into RTP packets with A-law encoding
    ///
    /// Same parameters as `packetize_pcm_ulaw` but uses A-law (payload type 8)
    pub fn packetize_pcm_alaw(
        &mut self,
        pcm_samples: &[i16],
        sample_rate: u32,
        channels: u16,
        timestamp: u32,
        ssrc: u32,
    ) -> Vec<Vec<u8>> {
        self.packetize_pcm(pcm_samples, sample_rate, channels, timestamp, ssrc, 8) // Payload type 8 = PCMA
    }

    fn packetize_pcm(
        &mut self,
        pcm_samples: &[i16],
        sample_rate: u32,
        channels: u16,
        timestamp: u32,
        ssrc: u32,
        payload_type: u8,
    ) -> Vec<Vec<u8>> {
        // G.711 standard is 8kHz. Document limitation for other rates.
        if sample_rate != 8000 {
            tracing::warn!(
                "G.711 is designed for 8kHz audio. Current rate: {}Hz. May require resampling.",
                sample_rate
            );
        }

        // Calculate samples per packet (20ms at 8kHz = 160 samples)
        let samples_per_packet = (sample_rate / 50) as usize; // 20ms = 1/50 second
        let _frames_per_packet = samples_per_packet / channels as usize;

        let mut packets = Vec::new();
        let mut sample_offset = 0;

        while sample_offset < pcm_samples.len() {
            let end_offset = (sample_offset + samples_per_packet).min(pcm_samples.len());
            let chunk = &pcm_samples[sample_offset..end_offset];

            // Encode to μ-law or A-law
            let encoded: Vec<u8> = if payload_type == 0 {
                chunk.iter().map(|&s| pcm_to_mulaw(s)).collect()
            } else {
                chunk.iter().map(|&s| pcm_to_alaw(s)).collect()
            };

            // Create RTP header
            let current_timestamp = timestamp + (sample_offset as u32 * 8000 / sample_rate); // Scale to 8kHz clock
            let header =
                RtpHeader::new(payload_type, self.sequence_number, current_timestamp, ssrc);

            // Build packet: header (12 bytes) + payload
            let mut packet = Vec::with_capacity(12 + encoded.len());
            packet.extend_from_slice(&header.to_bytes());
            packet.extend_from_slice(&encoded);

            packets.push(packet);

            self.sequence_number = self.sequence_number.wrapping_add(1);
            sample_offset = end_offset;
        }

        packets
    }
}

impl Default for RtpAudioPacketizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pcm_to_mulaw_range() {
        // Test that μ-law encoding always produces 0-255
        for sample in -32768i16..32767 {
            let _ = pcm_to_mulaw(sample);
        }
    }

    #[test]
    fn test_pcm_to_alaw_range() {
        // Test that A-law encoding always produces 0-255
        for sample in -32768i16..32767 {
            let _ = pcm_to_alaw(sample);
        }
    }

    #[test]
    fn test_mulaw_roundtrip_within_tolerance() {
        // Test round-trip: PCM → μ-law → PCM
        let tolerance = 792; // μ-law has ~12-bit dynamic range, some loss expected

        for sample in [-10000, -5000, -1000, 0, 1000, 5000, 10000].iter() {
            let encoded = pcm_to_mulaw(*sample);
            let decoded = mulaw_to_pcm(encoded);
            let diff = (decoded - sample).abs();
            assert!(
                diff <= tolerance,
                "Round-trip failed: original={}, decoded={}, diff={}",
                sample,
                decoded,
                diff
            );
        }
    }

    #[test]
    fn test_alaw_roundtrip_within_tolerance() {
        // Test round-trip: PCM → A-law → PCM
        let tolerance = 800; // A-law tolerance

        for sample in [-10000, -5000, -1000, 0, 1000, 5000, 10000].iter() {
            let encoded = pcm_to_alaw(*sample);
            let decoded = alaw_to_pcm(encoded);
            let diff = (decoded - sample).abs();
            assert!(
                diff <= tolerance,
                "Round-trip failed: original={}, decoded={}, diff={}",
                sample,
                decoded,
                diff
            );
        }
    }

    #[test]
    fn test_mulaw_zero_point() {
        // Zero should encode/decode reasonably close
        let encoded = pcm_to_mulaw(0);
        let decoded = mulaw_to_pcm(encoded);
        assert!(decoded.abs() < 100, "Zero point drift: {}", decoded);
    }

    #[test]
    fn test_alaw_zero_point() {
        // Zero should encode/decode reasonably close
        let encoded = pcm_to_alaw(0);
        let decoded = alaw_to_pcm(encoded);
        assert!(decoded.abs() < 800, "Zero point drift: {}", decoded);
    }

    #[test]
    fn test_rtp_audio_packetizer_ulaw_payload_type() {
        let mut packetizer = RtpAudioPacketizer::new();
        let samples = vec![0i16; 160];
        let packets = packetizer.packetize_pcm_ulaw(&samples, 8000, 1, 0, 12345);

        assert_eq!(packets.len(), 1, "Should produce 1 packet for 160 samples");

        // Check payload type (byte 1, lower 7 bits)
        let payload_type = packets[0][1] & 0x7F;
        assert_eq!(payload_type, 0, "Payload type should be 0 for PCMU");
    }

    #[test]
    fn test_rtp_audio_packetizer_alaw_payload_type() {
        let mut packetizer = RtpAudioPacketizer::new();
        let samples = vec![0i16; 160];
        let packets = packetizer.packetize_pcm_alaw(&samples, 8000, 1, 0, 12345);

        assert_eq!(packets.len(), 1, "Should produce 1 packet for 160 samples");

        // Check payload type (byte 1, lower 7 bits)
        let payload_type = packets[0][1] & 0x7F;
        assert_eq!(payload_type, 8, "Payload type should be 8 for PCMA");
    }

    #[test]
    fn test_rtp_audio_packetizer_packet_count() {
        let mut packetizer = RtpAudioPacketizer::new();
        let samples = vec![0i16; 320]; // 320 samples = 2 packets at 8kHz/20ms
        let packets = packetizer.packetize_pcm_ulaw(&samples, 8000, 1, 0, 12345);

        assert_eq!(packets.len(), 2, "Should produce 2 packets for 320 samples");
    }

    #[test]
    fn test_rtp_audio_packetizer_header_structure() {
        let mut packetizer = RtpAudioPacketizer::new();
        let samples = vec![0i16; 160];
        let packets = packetizer.packetize_pcm_ulaw(&samples, 8000, 1, 1000, 0xABCD1234);

        assert!(packets.len() >= 1, "Should produce at least 1 packet");
        let packet = &packets[0];

        // Verify packet is at least header (12 bytes) + payload
        assert!(packet.len() >= 12, "Packet too short: {}", packet.len());

        // Check version (bits 6-7 of byte 0)
        let version = packet[0] >> 6;
        assert_eq!(version, 2, "RTP version should be 2");

        // Check SSRC (bytes 8-11)
        let ssrc_bytes: [u8; 4] = [packet[8], packet[9], packet[10], packet[11]];
        let ssrc = u32::from_be_bytes(ssrc_bytes);
        assert_eq!(ssrc, 0xABCD1234, "SSRC mismatch");
    }

    #[test]
    fn test_rtp_audio_packetizer_sequence_number() {
        let mut packetizer = RtpAudioPacketizer::new();
        let samples = vec![0i16; 320]; // 2 packets
        let packets = packetizer.packetize_pcm_ulaw(&samples, 8000, 1, 0, 12345);

        assert_eq!(packets.len(), 2);

        // Extract sequence numbers (bytes 2-3)
        let seq1 = u16::from_be_bytes([packets[0][2], packets[0][3]]);
        let seq2 = u16::from_be_bytes([packets[1][2], packets[1][3]]);

        assert_eq!(seq1, 0, "First sequence number should be 0");
        assert_eq!(seq2, 1, "Second sequence number should be 1");
    }

    #[test]
    fn test_rtp_audio_packetizer_reset() {
        let mut packetizer = RtpAudioPacketizer::new();
        let samples = vec![0i16; 160];

        packetizer.packetize_pcm_ulaw(&samples, 8000, 1, 0, 12345);
        packetizer.reset();

        let packets = packetizer.packetize_pcm_ulaw(&samples, 8000, 1, 0, 12345);
        let seq = u16::from_be_bytes([packets[0][2], packets[0][3]]);
        assert_eq!(seq, 0, "Sequence number should reset to 0");
    }

    #[test]
    fn test_rtp_audio_packetizer_stereo() {
        let mut packetizer = RtpAudioPacketizer::new();
        let samples = vec![0i16; 320]; // 160 stereo frames = 320 samples
        let packets = packetizer.packetize_pcm_ulaw(&samples, 8000, 2, 0, 12345);

        assert_eq!(
            packets.len(),
            2,
            "Stereo: 320 samples should produce 2 packets"
        );
        assert_eq!(
            packets[0].len(),
            12 + 160,
            "First packet: header + 160 encoded bytes"
        );
    }

    #[test]
    fn test_rtp_audio_packetizer_non_standard_sample_rate() {
        let mut packetizer = RtpAudioPacketizer::new();
        let samples = vec![0i16; 160];
        // 16kHz should work (though non-standard for G.711)
        let _ = packetizer.packetize_pcm_ulaw(&samples, 16000, 1, 0, 12345);

        // 48kHz should also work (may need resampling in real use)
        let _ = packetizer.packetize_pcm_ulaw(&samples, 48000, 1, 0, 12345);
    }
}
