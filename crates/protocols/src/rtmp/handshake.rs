//! RTMP handshake protocol (C0/C1/C2 → S0/S1/S2)
//!
//! RTMP handshake follows this sequence:
//! - Client sends C0 + C1
//! - Server responds with S0 + S1 + S2
//! - Client sends C2
//! - Handshake complete

use anyhow::{Context, Result, bail};
use std::io::{Read, Write};

pub const RTMP_VERSION: u8 = 3;
pub const HANDSHAKE_SIZE: usize = 1536;

/// Perform RTMP handshake as a server
///
/// Sequence:
/// 1. Read C0 (1 byte version check)
/// 2. Read C1 (1536 bytes random)
/// 3. Send S0 (version 3) + S1 (server time + random) + S2 (echo of C1)
/// 4. Read C2 (1536 bytes, should echo S1)
pub fn handle_handshake<R: Read, W: Write>(reader: &mut R, writer: &mut W) -> Result<()> {
    // Read C0
    let mut c0 = [0u8; 1];
    reader.read_exact(&mut c0).context("Failed to read C0")?;
    if c0[0] != RTMP_VERSION {
        bail!("Unsupported RTMP version: {}", c0[0]);
    }
    tracing::debug!("Received C0: version {}", c0[0]);

    // Read C1
    let mut c1 = [0u8; HANDSHAKE_SIZE];
    reader.read_exact(&mut c1).context("Failed to read C1")?;
    let c1_time = u32::from_be_bytes([c1[0], c1[1], c1[2], c1[3]]);
    tracing::debug!(
        "Received C1: time={}, random_len={}",
        c1_time,
        HANDSHAKE_SIZE
    );

    // Generate S1
    let s1 = generate_s1();
    let s1_time = u32::from_be_bytes([s1[0], s1[1], s1[2], s1[3]]);

    // Send S0
    writer
        .write_all(&[RTMP_VERSION])
        .context("Failed to write S0")?;
    tracing::debug!("Sent S0: version {}", RTMP_VERSION);

    // Send S1
    writer.write_all(&s1).context("Failed to write S1")?;
    tracing::debug!("Sent S1: time={}, random_len={}", s1_time, HANDSHAKE_SIZE);

    // Send S2 (echo of C1)
    let s2 = generate_s2(&c1, s1_time);
    writer.write_all(&s2).context("Failed to write S2")?;
    tracing::debug!("Sent S2: echoed C1 time={}", c1_time);

    // Read C2
    let mut c2 = [0u8; HANDSHAKE_SIZE];
    reader.read_exact(&mut c2).context("Failed to read C2")?;
    let c2_time = u32::from_be_bytes([c2[0], c2[1], c2[2], c2[3]]);
    let c2_time2 = u32::from_be_bytes([c2[4], c2[5], c2[6], c2[7]]);

    // Verify C2 echoes S1
    if c2_time != s1_time {
        bail!("C2 time {} does not match S1 time {}", c2_time, s1_time);
    }
    if c2[8..] != s1[8..] {
        bail!("C2 random data does not match S1");
    }
    tracing::debug!(
        "Received C2: time={}, time2={}, verified S1 echo",
        c2_time,
        c2_time2
    );

    Ok(())
}

/// Generate S1 handshake packet
///
/// Format: 4 bytes time + 4 bytes zero + 1528 bytes random
fn generate_s1() -> [u8; HANDSHAKE_SIZE] {
    let mut s1 = [0u8; HANDSHAKE_SIZE];

    // Use current time as epoch (in milliseconds)
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u32;

    s1[0..4].copy_from_slice(&time.to_be_bytes());
    // s1[4..8] is already zeros

    // Fill random bytes
    let mut seed = time as u64;
    for byte in &mut s1[8..] {
        // Simple LCG for randomness (not cryptographically secure, but sufficient for RTMP)
        seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
        *byte = (seed >> 8) as u8;
    }

    s1
}

/// Generate S2 handshake packet (echo of C1)
///
/// Format: 4 bytes time (echo of C1) + 4 bytes time2 (when C1 was read) + 1528 bytes random echo
fn generate_s2(c1: &[u8; HANDSHAKE_SIZE], s1_time: u32) -> [u8; HANDSHAKE_SIZE] {
    let mut s2 = [0u8; HANDSHAKE_SIZE];

    // Echo C1 time
    s2[0..4].copy_from_slice(&c1[0..4]);

    // Time2: when C1 was received (use s1_time as approximation)
    s2[4..8].copy_from_slice(&s1_time.to_be_bytes());

    // Echo C1 random data
    s2[8..].copy_from_slice(&c1[8..]);

    s2
}

/// Generate C1 handshake packet (time + zeros + random)
pub fn generate_c1() -> [u8; HANDSHAKE_SIZE] {
    let mut c1 = [0u8; HANDSHAKE_SIZE];
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u32;
    c1[0..4].copy_from_slice(&time.to_be_bytes());
    // Fill random bytes
    let mut seed = time as u64;
    for byte in &mut c1[8..] {
        seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
        *byte = (seed >> 8) as u8;
    }
    c1
}

/// Generate C2 handshake packet (echo of S1)
pub fn generate_c2(s1: &[u8; HANDSHAKE_SIZE], c1: &[u8; HANDSHAKE_SIZE]) -> [u8; HANDSHAKE_SIZE] {
    let mut c2 = [0u8; HANDSHAKE_SIZE];
    c2[0..4].copy_from_slice(&s1[0..4]);
    c2[4..8].copy_from_slice(&c1[0..4]);
    c2[8..].copy_from_slice(&s1[8..]);
    c2
}

/// Perform RTMP handshake as a client
///
/// Sends C0+C1, receives S0+S1+S2, verifies S2 echoes C1, sends C2.
pub fn perform_client_handshake<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    c1: &[u8; HANDSHAKE_SIZE],
) -> Result<()> {
    let c1_time = u32::from_be_bytes([c1[0], c1[1], c1[2], c1[3]]);

    // Send C0 + C1
    writer.write_all(&[RTMP_VERSION])?;
    writer.write_all(c1)?;
    writer.flush()?;

    // Read S0
    let mut s0 = [0u8; 1];
    reader.read_exact(&mut s0)?;
    if s0[0] != RTMP_VERSION {
        bail!("Unsupported RTMP version from server: {}", s0[0]);
    }

    // Read S1
    let mut s1 = [0u8; HANDSHAKE_SIZE];
    reader.read_exact(&mut s1)?;

    // Read S2
    let mut s2 = [0u8; HANDSHAKE_SIZE];
    reader.read_exact(&mut s2)?;

    // Verify S2 echoes C1
    let s2_time = u32::from_be_bytes([s2[0], s2[1], s2[2], s2[3]]);
    if s2_time != c1_time {
        bail!("S2 time {} does not match C1 time {}", s2_time, c1_time);
    }
    if s2[8..] != c1[8..] {
        bail!("S2 random data does not match C1");
    }

    // Send C2 (echo of S1)
    let c2 = generate_c2(&s1, c1);
    writer.write_all(&c2)?;
    writer.flush()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_s1_structure() {
        let s1 = generate_s1();

        // Verify size
        assert_eq!(s1.len(), HANDSHAKE_SIZE);

        // Verify structure: first 4 bytes are time (non-zero), next 4 bytes are zero
        let time = u32::from_be_bytes([s1[0], s1[1], s1[2], s1[3]]);
        assert!(time > 0);
        assert_eq!(&s1[4..8], &[0, 0, 0, 0]);

        // Verify random bytes are not all zero
        let random_sum: u64 = s1[8..].iter().map(|&b| b as u64).sum();
        assert!(random_sum > 0);
    }

    #[test]
    fn test_generate_s2_echoes_c1() {
        let mut c1 = [0u8; HANDSHAKE_SIZE];
        c1[0] = 0x12; // time byte 0
        c1[1] = 0x34; // time byte 1
        c1[2] = 0x56; // time byte 2
        c1[3] = 0x78; // time byte 3
        c1[4] = 0x00; // zero byte 0
        c1[5] = 0x00; // zero byte 1
        c1[6] = 0x00; // zero byte 2
        c1[7] = 0x00; // zero byte 3
        c1[8] = 0xAA; // random byte 0
        c1[9] = 0xBB; // random byte 1
        c1[10] = 0xCC; // random byte 2
        c1[1535] = 0xDD; // last random byte

        let s1_time = 0x11223344;
        let s2 = generate_s2(&c1, s1_time);

        // Verify S2 echoes C1 time
        assert_eq!(&s2[0..4], &c1[0..4]);

        // Verify S2 contains time2
        assert_eq!(u32::from_be_bytes([s2[4], s2[5], s2[6], s2[7]]), s1_time);

        // Verify S2 echoes C1 random data
        assert_eq!(&s2[8..], &c1[8..]);
        assert_eq!(s2[8], 0xAA);
        assert_eq!(s2[9], 0xBB);
        assert_eq!(s2[10], 0xCC);
        assert_eq!(s2[1535], 0xDD);
    }

    #[test]
    fn test_handshake_simulation() {
        let mut client_to_server: Vec<u8> = Vec::new();

        // Client sends C0 + C1
        client_to_server.push(RTMP_VERSION);
        let mut c1 = [0u8; HANDSHAKE_SIZE];
        c1[0] = 0x01;
        c1[1535] = 0xFF;
        client_to_server.extend_from_slice(&c1);

        // Add a dummy C2 (all zeros, will fail verification)
        let c2_dummy = [0u8; HANDSHAKE_SIZE];
        client_to_server.extend_from_slice(&c2_dummy);

        // Server processes C0 + C1 + C2 and sends S0 + S1 + S2
        let mut reader = client_to_server.as_slice();
        let mut writer = Vec::new();
        let result = handle_handshake(&mut reader, &mut writer);
        // C2 verification will fail (dummy zeros don't match S1 time),
        // but S0+S1+S2 should have been written before C2 check
        assert!(result.is_err(), "C2 verification should fail for dummy C2");
        assert!(
            writer.len() >= 3073,
            "S0+S1+S2 not written: writer has {} bytes",
            writer.len()
        );

        // Verify S0
        assert_eq!(writer[0], RTMP_VERSION);

        // Verify S1
        let s1: [u8; HANDSHAKE_SIZE] = writer[1..1537].try_into().unwrap();
        assert_eq!(&s1[4..8], &[0, 0, 0, 0]);

        // Verify S2 echoes C1
        let s2: [u8; HANDSHAKE_SIZE] = writer[1537..3073].try_into().unwrap();
        assert_eq!(&s2[0..4], &c1[0..4]);
        assert_eq!(&s2[8..], &c1[8..]);

        // Now build C2 from S1 and verify it in a second handshake
        let mut c2_data: Vec<u8> = Vec::new();
        // Add a dummy C0 + C1 (won't be verified against, just consumed)
        c2_data.push(RTMP_VERSION);
        let mut dummy_c1 = [0u8; HANDSHAKE_SIZE];
        dummy_c1[0] = 0x01;
        c2_data.extend_from_slice(&dummy_c1);
        // Now add C2
        let mut c2 = [0u8; HANDSHAKE_SIZE];
        c2[0..4].copy_from_slice(&s1[0..4]);
        c2[8..].copy_from_slice(&s1[8..]);
        c2_data.extend_from_slice(&c2);

        let mut reader2 = c2_data.as_slice();
        let mut writer2 = Vec::new();
        assert!(handle_handshake(&mut reader2, &mut writer2).is_ok());
    }
}
