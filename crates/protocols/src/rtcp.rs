//! RTCP packet construction per RFC 3550.
//!
//! Currently implements only the Sender Report (SR) packet type (PT=200).
//! Receiver reports (RR), SDES, BYE, and APP are not implemented.

/// NTP epoch offset: seconds from 1900-01-01 to 1970-01-01 (Unix epoch).
const NTP_EPOCH_OFFSET: u64 = 2_208_988_800;

/// Convert a `SystemTime` to a 64-bit NTP timestamp.
///
/// NTP timestamps represent seconds since 1900-01-01 00:00:00 UTC in the
/// high 32 bits (integer part) and fractional seconds in the low 32 bits.
///
/// If `now` is before the Unix epoch (pre-1970), returns the epoch (all zeros).
pub fn system_time_to_ntp(now: std::time::SystemTime) -> u64 {
    let dur = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs() + NTP_EPOCH_OFFSET;
    let frac = ((dur.subsec_nanos() as f64) / 1_000_000_000.0) * (1u64 << 32) as f64;
    (secs << 32) | (frac as u64)
}

/// Derive an approximate RTP timestamp from an NTP timestamp.
///
/// Uses the given `clock_rate` (e.g., 90000 for H.264) to compute an RTP
/// timestamp value that corresponds to the same instant as the NTP timestamp.
///
/// This is a convenience helper for building sender reports where the NTP
/// timestamp and RTP timestamp should refer to the same point in time.
pub fn ntp_to_rtp(ntp: u64, clock_rate: u32) -> u32 {
    let secs = ntp >> 32;
    let frac = ntp & 0xFFFF_FFFF;
    // RTP_ts = secs * clock_rate + (frac * clock_rate) >> 32
    let rtp = secs.wrapping_mul(clock_rate as u64)
        + ((frac as u128).wrapping_mul(clock_rate as u128) >> 32) as u64;
    (rtp & 0xFFFF_FFFF) as u32
}

/// Build an RTCP Sender Report (SR) packet (RFC 3550 §6.4.1).
///
/// # Arguments
///
/// * `ssrc` - SSRC identifier of this sender.
/// * `ntp_timestamp` - 64-bit NTP timestamp (seconds since 1900-01-01 + fraction).
/// * `rtp_timestamp` - 32-bit RTP timestamp corresponding to the NTP timestamp.
/// * `packet_count` - Total number of RTP data packets sent by this sender.
/// * `octet_count` - Total number of payload octets sent (excluding headers/padding).
///
/// # Returns
///
/// A complete 28-byte RTCP SR packet with no reception reports.
///
/// # Wire format
///
/// ```text
///  0                   1                   2                   3
///  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |V=2|P| RC=0    | PT=200       | length (=6)                   |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                         SSRC of sender                        |
/// +=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+
/// |              NTP timestamp, most significant word              |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |             NTP timestamp, least significant word              |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                         RTP timestamp                          |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                     sender's packet count                      |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                      sender's octet count                      |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// ```
pub fn build_sender_report(
    ssrc: u32,
    ntp_timestamp: u64,
    rtp_timestamp: u32,
    packet_count: u32,
    octet_count: u32,
) -> Vec<u8> {
    // RTCP SR fixed header (8 bytes) + sender info (20 bytes) = 28 bytes total
    // length field = (total 32-bit words) - 1 = (28/4) - 1 = 6
    let mut buf = Vec::with_capacity(28);

    // Byte 0: V=2 (bits 7-6), P=0 (bit 5), RC=0 (bits 4-0)
    buf.push(0x80);
    // Byte 1: PT=200 (0xC8)
    buf.push(200);
    // Bytes 2-3: length (6 in network byte order)
    buf.extend_from_slice(&6u16.to_be_bytes());
    // Bytes 4-7: SSRC
    buf.extend_from_slice(&ssrc.to_be_bytes());
    // Bytes 8-15: NTP timestamp
    buf.extend_from_slice(&ntp_timestamp.to_be_bytes());
    // Bytes 16-19: RTP timestamp
    buf.extend_from_slice(&rtp_timestamp.to_be_bytes());
    // Bytes 20-23: sender's packet count
    buf.extend_from_slice(&packet_count.to_be_bytes());
    // Bytes 24-27: sender's octet count
    buf.extend_from_slice(&octet_count.to_be_bytes());

    debug_assert_eq!(buf.len(), 28, "RTCP SR must be exactly 28 bytes");
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sr_basic_structure() {
        let sr = build_sender_report(0xDEADBEEF, 0, 0, 0, 0);
        // Total length must be 28 bytes
        assert_eq!(sr.len(), 28, "SR must be 28 bytes");

        // Version field: first byte upper 2 bits = 2
        assert_eq!((sr[0] >> 6) & 0x03, 2, "V=2");
        // Padding bit: not set
        assert_eq!((sr[0] >> 5) & 0x01, 0, "P=0");
        // Reception report count: 0
        assert_eq!(sr[0] & 0x1F, 0, "RC=0");
        // Payload type: 200
        assert_eq!(sr[1], 200, "PT=200");
        // Length: 6 (28/4 - 1)
        let length = u16::from_be_bytes([sr[2], sr[3]]);
        assert_eq!(length, 6, "length=6");
        // SSRC
        let ssrc = u32::from_be_bytes([sr[4], sr[5], sr[6], sr[7]]);
        assert_eq!(ssrc, 0xDEADBEEF);
    }

    #[test]
    fn test_sr_with_counts() {
        let ntp = system_time_to_ntp(std::time::SystemTime::UNIX_EPOCH);
        // At Unix epoch, NTP timestamp should be exactly NTP_EPOCH_OFFSET seconds
        let expected_secs = NTP_EPOCH_OFFSET << 32;
        assert_eq!(
            ntp, expected_secs,
            "NTP at Unix epoch should be {NTP_EPOCH_OFFSET}s"
        );

        let sr = build_sender_report(0x12345678, ntp, 12345, 100, 50000);

        // Verify SSRC
        let ssrc = u32::from_be_bytes([sr[4], sr[5], sr[6], sr[7]]);
        assert_eq!(ssrc, 0x12345678);

        // Verify NTP timestamp
        let ntp_high = u32::from_be_bytes([sr[8], sr[9], sr[10], sr[11]]);
        let ntp_low = u32::from_be_bytes([sr[12], sr[13], sr[14], sr[15]]);
        assert_eq!(ntp_high, NTP_EPOCH_OFFSET as u32);
        assert_eq!(ntp_low, 0);

        // Verify RTP timestamp
        let rtp_ts = u32::from_be_bytes([sr[16], sr[17], sr[18], sr[19]]);
        assert_eq!(rtp_ts, 12345);

        // Verify packet count
        let pkt_count = u32::from_be_bytes([sr[20], sr[21], sr[22], sr[23]]);
        assert_eq!(pkt_count, 100);

        // Verify octet count
        let oct_count = u32::from_be_bytes([sr[24], sr[25], sr[26], sr[27]]);
        assert_eq!(oct_count, 50000);
    }

    #[test]
    fn test_ntp_conversion_unix_epoch() {
        // At the Unix epoch (1970-01-01 00:00:00 UTC), NTP seconds = NTP_EPOCH_OFFSET
        let ntp = system_time_to_ntp(std::time::UNIX_EPOCH);
        let expected_secs = NTP_EPOCH_OFFSET << 32;
        assert_eq!(
            ntp, expected_secs,
            "NTP at Unix epoch should be {NTP_EPOCH_OFFSET}s"
        );
    }

    #[test]
    fn test_ntp_roundtrip() {
        // Current time should produce a reasonable NTP timestamp
        let now = std::time::SystemTime::now();
        let ntp = system_time_to_ntp(now);
        let secs = ntp >> 32;

        // As of 2026, NTP seconds should be > 126_ (for year >= 2026)
        // NTP seconds = years_since_1900 * 365.25 * 86400 ≈ 126_ + years * 31_536_000
        assert!(
            secs > NTP_EPOCH_OFFSET,
            "NTP seconds should be past Unix epoch"
        );
    }

    #[test]
    fn test_ntp_to_rtp_known() {
        // At NTP epoch (1900-01-01), RTP timestamp should be 0 for any clock rate
        let rtp = ntp_to_rtp(0, 90000);
        assert_eq!(rtp, 0, "RTP at NTP epoch should be 0");

        // At exactly 1 NTP second, RTP timestamp should equal clock_rate
        let one_second_ntp = 1u64 << 32; // 1 second in NTP format
        let rtp = ntp_to_rtp(one_second_ntp, 90000);
        assert_eq!(rtp, 90000, "RTP at 1 NTP second should be clock_rate");

        // At 0.5 seconds (half of 2^32 in the fraction part)
        let half_second_ntp = 1u64 << 31; // 0.5 seconds
        let rtp = ntp_to_rtp(half_second_ntp, 90000);
        assert_eq!(rtp, 45000, "RTP at 0.5 NTP seconds should be clock_rate/2");
    }

    #[test]
    fn test_sr_wire_format_known() {
        // Construct SR with known values and verify exact byte output
        let sr = build_sender_report(
            0x01020304,
            0x0000000100000002, // NTP: 1 second + 2/2^32 fraction
            0xAABBCCDD,
            0x00000100, // 256 packets
            0x00100000, // 1048576 octets
        );

        let expected: Vec<u8> = vec![
            0x80, // V=2, P=0, RC=0
            200,  // PT=200 (SR)
            0x00, 0x06, // length=6
            0x01, 0x02, 0x03, 0x04, // SSRC
            0x00, 0x00, 0x00, 0x01, // NTP seconds=1
            0x00, 0x00, 0x00, 0x02, // NTP fraction=2
            0xAA, 0xBB, 0xCC, 0xDD, // RTP timestamp
            0x00, 0x00, 0x01, 0x00, // packet_count=256
            0x00, 0x10, 0x00, 0x00, // octet_count=1048576
        ];

        assert_eq!(sr, expected, "RTCP SR wire format mismatch");
    }

    #[test]
    fn test_ntp_to_rtp_wraparound() {
        // Verify wrapping behavior for large NTP values
        let large_ntp = 0xFFFFFFFF00000000u64; // ~136 years of NTP seconds
        let rtp = ntp_to_rtp(large_ntp, 90000);
        // Should not panic; result fits in u32
        let _unused = rtp;
    }
}
