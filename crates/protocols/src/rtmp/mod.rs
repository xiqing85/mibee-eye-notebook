//! RTMP Push Client — pushes local H.264/AAC stream to external RTMP ingest server.
//!
//! This module implements an RTMP push client that:
//! - Connects to an external RTMP server (NVR, CDN, ingest endpoint)
//! - Performs the RTMP handshake (C0+C1↔S0+S1+S2↔C2)
//! - Sends NetConnection commands (connect, createStream)
//! - Sends NetStream commands (publish)
//! - Delivers H.264 video frames and AAC audio frames
//! - Supports reconnection lifecycle

mod amf0;
mod chunk;
mod handshake;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub use amf0::Amf0Value;
pub use chunk::{ChunkBasicHeader, ChunkMessageHeader, ChunkStreamParser, ChunkType, MessageType};

/// Default RTMP port (1935)
pub const DEFAULT_PORT: u16 = 1935;

/// Chunk size negotiated after handshake
const CHUNK_SIZE: u32 = 65536;

// ── Core types ──────────────────────────────────────────────────────────────────────────────

/// RTMP push client
///
/// Manages a single TCP connection to an RTMP ingest server. The typical lifecycle is:
///
/// ```ignore
/// let mut client = RtmpPushClient::new("nvr.example.com", 1935, "live", "mystream");
/// client.connect().await?;
/// client.send_video(&h264_data, 0, true).await?;  // sequence header
/// client.send_video(&nal_data, 40, true).await?;   // keyframe
/// client.send_video(&nal_data, 80, false).await?;  // inter frame
/// client.close().await?;
/// ```
pub struct RtmpPushClient {
    host: String,
    port: u16,
    app_name: String,
    stream_name: String,
    stream: Option<tokio::net::TcpStream>,
    stream_id: u32,
    chunk_size: u32,
    /// Current video timestamp in ms
    pub video_timestamp: u32,
    /// Current audio timestamp in ms
    pub audio_timestamp: u32,
}

impl RtmpPushClient {
    /// Create a new RTMP push client
    ///
    /// Does not connect — call [`connect`](Self::connect) to establish the connection.
    pub fn new(host: &str, port: u16, app_name: &str, stream_name: &str) -> Self {
        Self {
            host: host.to_string(),
            port,
            app_name: app_name.to_string(),
            stream_name: stream_name.to_string(),
            stream: None,
            stream_id: 0,
            chunk_size: 128,
            video_timestamp: 0,
            audio_timestamp: 0,
        }
    }

    /// Returns `true` if the client has an active connection
    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    /// Connect to the RTMP server and negotiate the stream
    ///
    /// Performs, in order:
    /// 1. TCP connect
    /// 2. RTMP handshake
    /// 3. SetChunkSize negotiation
    /// 4. NetConnection.connect
    /// 5. NetConnection.createStream
    /// 6. NetStream.publish
    pub async fn connect(&mut self) -> Result<()> {
        // 1. TCP connect
        let mut socket = tokio::net::TcpStream::connect(format!("{}:{}", self.host, self.port))
            .await
            .context("Failed to connect to RTMP server")?;

        // 2. RTMP handshake — use tokio I/O directly with handshake helpers
        let c1 = handshake::generate_c1();
        let c1_time = u32::from_be_bytes([c1[0], c1[1], c1[2], c1[3]]);

        socket.write_all(&[handshake::RTMP_VERSION]).await?;
        socket.write_all(&c1).await?;
        socket.flush().await?;

        let mut s0 = [0u8; 1];
        socket.read_exact(&mut s0).await?;
        if s0[0] != handshake::RTMP_VERSION {
            bail!("Unsupported RTMP version from server: {}", s0[0]);
        }

        let mut s1 = [0u8; handshake::HANDSHAKE_SIZE];
        socket.read_exact(&mut s1).await?;

        let mut s2 = [0u8; handshake::HANDSHAKE_SIZE];
        socket.read_exact(&mut s2).await?;

        // Verify S2 echoes C1
        let s2_time = u32::from_be_bytes([s2[0], s2[1], s2[2], s2[3]]);
        if s2_time != c1_time {
            bail!("S2 time {} does not match C1 time {}", s2_time, c1_time);
        }
        if s2[8..] != c1[8..] {
            bail!("S2 random data does not match C1");
        }

        let c2 = handshake::generate_c2(&s1, &c1);
        socket.write_all(&c2).await?;
        socket.flush().await?;

        // 3. Bump chunk size for efficient transfer
        Self::write_protocol_message(
            &mut socket,
            MessageType::SetChunkSize,
            &CHUNK_SIZE.to_be_bytes(),
        )
        .await?;
        self.chunk_size = CHUNK_SIZE;

        // 4. NetConnection.connect
        let connect_payload = Self::encode_connect(&self.app_name, &self.host, self.port);
        Self::write_command_chunk(&mut socket, 0, 0, &connect_payload).await?;
        // Read responses: WindowAckSize, SetPeerBandwidth, _result, onStatus
        Self::read_until_command(&mut socket).await?;
        Self::read_until_command(&mut socket).await?;

        // 5. createStream
        let create_payload = Self::encode_create_stream();
        Self::write_command_chunk(&mut socket, 0, 0, &create_payload).await?;
        let stream_id = Self::read_stream_id(&mut socket).await?;
        self.stream_id = stream_id;

        // 6. publish
        let publish_payload = Self::encode_publish(&self.stream_name);
        Self::write_command_chunk(&mut socket, stream_id, 0, &publish_payload).await?;
        Self::read_until_command(&mut socket).await?;

        self.stream = Some(socket);
        Ok(())
    }

    /// Close the RTMP connection and reset state
    pub async fn close(&mut self) -> Result<()> {
        if let Some(mut stream) = self.stream.take() {
            // Send FCUnpublish + deleteStream for a clean shutdown
            let fcunpublish = Self::encode_fcunpublish(&self.stream_name);
            Self::write_command_chunk(&mut stream, 0, 0, &fcunpublish)
                .await
                .ok();
            stream.shutdown().await?;
        }
        self.stream_id = 0;
        self.chunk_size = 128;
        self.video_timestamp = 0;
        self.audio_timestamp = 0;
        Ok(())
    }

    /// Send an H.264 video frame
    ///
    /// `data` must be the raw RTMP video payload including the frame-type +
    /// codec-id byte, CTS bytes, and AVC packet type / NAL data. For typical
    /// use, call [`build_video_sequence_header`] or [`build_video_nalus`] to
    /// construct this payload from raw H.264 data.
    pub async fn send_video(&mut self, data: &[u8], timestamp: u32) -> Result<()> {
        self.video_timestamp = timestamp;
        let stream = self.stream.as_mut().context("Not connected")?;
        Self::write_data_chunk(
            stream,
            5,
            MessageType::Video,
            self.stream_id,
            timestamp,
            self.chunk_size,
            data,
        )
        .await
    }

    /// Send an AAC audio frame
    ///
    /// `data` must be the raw RTMP audio payload including the sound-format
    /// byte and AAC packet type. Use [`build_audio_sequence_header`] or
    /// [`build_audio_raw`] for typical usage.
    pub async fn send_audio(&mut self, data: &[u8], timestamp: u32) -> Result<()> {
        self.audio_timestamp = timestamp;
        let stream = self.stream.as_mut().context("Not connected")?;
        Self::write_data_chunk(
            stream,
            4,
            MessageType::Audio,
            self.stream_id,
            timestamp,
            self.chunk_size,
            data,
        )
        .await
    }

    // ── AMF0 command builders ───────────────────────────────────────────────────────────────

    fn encode_connect(app: &str, host: &str, port: u16) -> Vec<u8> {
        let tc_url = format!("rtmp://{}:{}/{}", host, port, app);
        let mut buf = Vec::new();
        buf.extend_from_slice(&Amf0Value::String("connect".to_string()).serialize());
        buf.extend_from_slice(&Amf0Value::Number(1.0).serialize()); // transaction ID
        buf.extend_from_slice(
            &Amf0Value::Object(vec![
                ("app".to_string(), Amf0Value::String(app.to_string())),
                ("tcUrl".to_string(), Amf0Value::String(tc_url)),
                (
                    "flashVer".to_string(),
                    Amf0Value::String("FMLE/3.0 (compatible; mibeerec)".to_string()),
                ),
                ("swfUrl".to_string(), Amf0Value::String(String::new())),
                ("fpad".to_string(), Amf0Value::Boolean(false)),
                ("capabilities".to_string(), Amf0Value::Number(239.0)),
                ("audioCodecs".to_string(), Amf0Value::Number(3575.0)),
                ("videoCodecs".to_string(), Amf0Value::Number(252.0)),
                ("videoFunction".to_string(), Amf0Value::Number(1.0)),
                ("pageUrl".to_string(), Amf0Value::String(String::new())),
                ("objectEncoding".to_string(), Amf0Value::Number(0.0)),
            ])
            .serialize(),
        );
        // Optional additional info (Wink amet, etc.)
        buf.extend_from_slice(
            &Amf0Value::Object(vec![(
                "level".to_string(),
                Amf0Value::String("status".to_string()),
            )])
            .serialize(),
        );
        buf
    }

    fn encode_create_stream() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&Amf0Value::String("createStream".to_string()).serialize());
        buf.extend_from_slice(&Amf0Value::Number(2.0).serialize()); // transaction ID
        buf.extend_from_slice(&Amf0Value::Null.serialize());
        buf
    }

    fn encode_publish(stream_name: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&Amf0Value::String("publish".to_string()).serialize());
        buf.extend_from_slice(&Amf0Value::Number(0.0).serialize()); // transaction ID
        buf.extend_from_slice(&Amf0Value::Null.serialize());
        buf.extend_from_slice(&Amf0Value::String(stream_name.to_string()).serialize());
        buf.extend_from_slice(&Amf0Value::String("live".to_string()).serialize()); // type: live
        buf
    }

    fn encode_fcunpublish(stream_name: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&Amf0Value::String("FCUnpublish".to_string()).serialize());
        buf.extend_from_slice(&Amf0Value::Number(0.0).serialize());
        buf.extend_from_slice(&Amf0Value::Null.serialize());
        buf.extend_from_slice(&Amf0Value::String(stream_name.to_string()).serialize());
        buf
    }

    // ── Chunk writing helpers ───────────────────────────────────────────────────────────────

    /// Write a protocol control message (CSID 2, on the control stream)
    async fn write_protocol_message(
        stream: &mut tokio::net::TcpStream,
        msg_type: MessageType,
        data: &[u8],
    ) -> Result<()> {
        Self::write_chunked(stream, 2, msg_type, 0, 0, 128, data).await
    }

    /// Write an AMF command chunk on CSID 3 (command stream)
    async fn write_command_chunk(
        stream: &mut tokio::net::TcpStream,
        message_stream_id: u32,
        timestamp: u32,
        amf_payload: &[u8],
    ) -> Result<()> {
        Self::write_chunked(
            stream,
            3,
            MessageType::Command,
            message_stream_id,
            timestamp,
            128,
            amf_payload,
        )
        .await
    }

    /// Write a data chunk (audio/video) with proper chunking
    async fn write_data_chunk(
        stream: &mut tokio::net::TcpStream,
        chunk_stream_id: u32,
        msg_type: MessageType,
        message_stream_id: u32,
        timestamp: u32,
        chunk_size: u32,
        data: &[u8],
    ) -> Result<()> {
        Self::write_chunked(
            stream,
            chunk_stream_id,
            msg_type,
            message_stream_id,
            timestamp,
            chunk_size,
            data,
        )
        .await
    }

    /// Core chunk writer — handles Type 0 header + Type 3 continuation chunks
    async fn write_chunked(
        stream: &mut tokio::net::TcpStream,
        chunk_stream_id: u32,
        msg_type: MessageType,
        message_stream_id: u32,
        timestamp: u32,
        chunk_size: u32,
        data: &[u8],
    ) -> Result<()> {
        let extended = timestamp >= 0xFFFFFF;

        // Type 0 header
        let basic = ChunkBasicHeader {
            chunk_type: ChunkType::Type0,
            chunk_stream_id,
        };
        stream.write_all(&basic.serialize()).await?;

        let msg_header = ChunkMessageHeader {
            timestamp,
            message_length: data.len() as u32,
            message_type: msg_type,
            message_stream_id: Some(message_stream_id),
            extended,
        };
        stream
            .write_all(&msg_header.serialize(ChunkType::Type0))
            .await?;

        // Chunked body
        let mut offset = 0;
        while offset < data.len() {
            let end = (offset + chunk_size as usize).min(data.len());
            stream.write_all(&data[offset..end]).await?;
            offset = end;
            if offset < data.len() {
                // Type 3 continuation chunk (no message header)
                let cont = ChunkBasicHeader {
                    chunk_type: ChunkType::Type3,
                    chunk_stream_id,
                };
                stream.write_all(&cont.serialize()).await?;
            }
        }

        stream.flush().await?;
        Ok(())
    }

    // ── Response reading helpers ────────────────────────────────────────────────────────────

    /// Read raw bytes from the socket and assemble complete RTMP messages
    /// by parsing chunks. Returns the first complete AMF command payload
    /// whose first string value is either `_result` or `onStatus`.
    async fn read_until_command(stream: &mut tokio::net::TcpStream) -> Result<Vec<Amf0Value>> {
        use std::io::Cursor;
        let mut parser = ChunkStreamParser::new();
        let mut buf = Vec::new();

        loop {
            // Fill buffer
            let mut tmp = [0u8; 4096];
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                bail!("Connection closed while waiting for RTMP response");
            }
            buf.extend_from_slice(&tmp[..n]);

            // Try to parse complete messages from the buffer
            let mut cursor = Cursor::new(&buf[..]);
            loop {
                match parser.parse_chunk(&mut cursor) {
                    Ok(Some(msg)) => {
                        // Trim consumed bytes from buffer
                        let consumed = cursor.position() as usize;
                        buf.drain(..consumed);
                        cursor = Cursor::new(&buf[..]);

                        // Check for SetChunkSize message
                        if msg.len() == 4 {
                            let maybe_size = u32::from_be_bytes([msg[0], msg[1], msg[2], msg[3]]);
                            if (1..=0xFFFFFF).contains(&maybe_size) {
                                parser.chunk_size = maybe_size;
                                continue;
                            }
                        }

                        // Try to extract AMF command
                        if let Ok(values) = Self::parse_amf_values(&msg) {
                            if let Some(Amf0Value::String(s)) = values.first() {
                                if s == "_result" || s == "onStatus" {
                                    return Ok(values);
                                }
                            }
                        }
                    }
                    Ok(None) => break, // need more data
                    Err(_) => {
                        // Corrupt data — advance past the consumed portion
                        let consumed = cursor.position() as usize;
                        buf.drain(..consumed);
                        break;
                    }
                }
            }
        }
    }

    /// Parse AMF0 values from a byte slice, returning all values that could
    /// be successfully decoded
    fn parse_amf_values(data: &[u8]) -> Result<Vec<Amf0Value>> {
        let mut values = Vec::new();
        let mut pos = 0;
        while pos < data.len() {
            match Amf0Value::parse(&data[pos..]) {
                Ok((v, consumed)) => {
                    values.push(v);
                    pos += consumed;
                }
                Err(_) => break,
            }
        }
        if values.is_empty() {
            bail!("No AMF values found");
        }
        Ok(values)
    }

    /// Read the createStream response and extract the assigned stream ID.
    /// The response format is: `_result`, `Number(transId)`, `Null`, `Number(streamId)`
    async fn read_stream_id(stream: &mut tokio::net::TcpStream) -> Result<u32> {
        let values = Self::read_until_command(stream).await?;
        // Look for the Number value after Null (4th value: [String, Number, Null, Number])
        for (i, v) in values.iter().enumerate() {
            if let Amf0Value::Null = v {
                if let Some(Amf0Value::Number(id)) = values.get(i + 1) {
                    return Ok(*id as u32);
                }
            }
        }
        // Fallback: if there are exactly 4 values and the last is a Number
        if values.len() >= 4 {
            if let Amf0Value::Number(id) = &values[3] {
                return Ok(*id as u32);
            }
        }
        bail!("Could not extract stream ID from createStream response");
    }
}

// ── Video/audio payload helpers ────────────────────────────────────────────────────────────

/// Build an RTMP AVC sequence header payload from SPS and PPS NAL units
pub fn build_video_sequence_header(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    data.push(0x17); // keyframe + AVC codec
    data.extend_from_slice(&[0x00, 0x00, 0x00]); // CTS = 0
    data.push(0x00); // AVC sequence header

    // AVCDecoderConfigurationRecord
    let profile = if sps.len() >= 3 { sps[1] } else { 0x42 };
    let level = if sps.len() >= 3 { sps[2] } else { 0x1E };
    let profile_compat = if sps.len() >= 4 { sps[3] } else { 0x80 };

    data.push(0x01); // configurationVersion
    data.push(profile);
    data.push(profile_compat);
    data.push(level);
    data.push(0xFF); // lengthSizeMinusOne (4-byte NAL lengths)
    data.push(0xE1); // numSPS (1 SPS)
    data.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    data.extend_from_slice(sps);
    data.push(0x01); // numPPS (1 PPS)
    data.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    data.extend_from_slice(pps);
    data
}

/// Build an RTMP AVC NALU payload from raw H.264 NAL units (each with 4-byte length prefix)
pub fn build_video_nalus(nal_data: &[u8], is_keyframe: bool, composition_offset: i32) -> Vec<u8> {
    let frame_type = if is_keyframe { 1 } else { 2 };
    let mut data = Vec::new();
    data.push((frame_type << 4) | 7); // frame_type + AVC codec ID
    data.extend_from_slice(&[0x00, 0x00, 0x00]); // first CTS placeholder
    data.push(0x01); // AVC NALU packet type
    // Composition time offset (3 bytes, big-endian signed)
    let offset = composition_offset.clamp(-8388608_i32, 8388607_i32);
    data.extend_from_slice(&offset.to_be_bytes()[1..4]);
    data.extend_from_slice(nal_data);
    data
}

/// Build an RTMP AAC sequence header payload
pub fn build_audio_sequence_header(audio_specific_config: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    data.push(0xAF); // AAC, 44kHz, 16-bit, stereo
    data.push(0x00); // AAC sequence header
    data.extend_from_slice(audio_specific_config);
    data
}

/// Build an RTMP AAC raw frame payload
pub fn build_audio_raw(aac_data: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    data.push(0xAF); // AAC, 44kHz, 16-bit, stereo
    data.push(0x01); // AAC raw data
    data.extend_from_slice(aac_data);
    data
}

// ── Frame extraction (keep from old code for the reverse direction) ────────────────────────

/// Extract H.264 NAL units from an RTMP video message
///
/// RTMP video format: 1 byte frame type + codec info + 3 bytes composition time + NAL units
pub fn extract_h264_nal_units(data: &[u8]) -> Result<Vec<Vec<u8>>> {
    if data.len() < 5 {
        bail!("RTMP video data too short: {}", data.len());
    }

    let first_byte = data[0];
    let codec_id = first_byte & 0x0F;
    let _frame_type = (first_byte >> 4) & 0x0F;

    if codec_id != 7 {
        bail!("Unsupported video codec: {}", codec_id);
    }

    let avc_packet_type = data[4];
    let payload = &data[5..];

    match avc_packet_type {
        0 => {
            // AVC sequence header
            if payload.len() < 11 {
                bail!("AVC sequence header too short: {}", payload.len());
            }
            let sps_len = u16::from_be_bytes([payload[6], payload[7]]) as usize;
            let sps = payload[8..8 + sps_len].to_vec();
            let pps_offset = 8 + sps_len + 1;
            let pps_len =
                u16::from_be_bytes([payload[pps_offset], payload[pps_offset + 1]]) as usize;
            let pps = payload[pps_offset + 2..pps_offset + 2 + pps_len].to_vec();
            Ok(vec![sps, pps])
        }
        1 => {
            // AVC NALU(s)
            let nal_data = &data[8..];
            let mut nal_units = Vec::new();
            let mut pos = 0;
            while pos + 4 <= nal_data.len() {
                let nal_len = u32::from_be_bytes([
                    nal_data[pos],
                    nal_data[pos + 1],
                    nal_data[pos + 2],
                    nal_data[pos + 3],
                ]) as usize;
                if pos + 4 + nal_len > nal_data.len() {
                    bail!("NAL unit extends beyond data");
                }
                nal_units.push(nal_data[pos + 4..pos + 4 + nal_len].to_vec());
                pos += 4 + nal_len;
            }
            Ok(nal_units)
        }
        2 => Ok(vec![]), // AVC end of sequence
        _ => bail!("Unknown AVC packet type: {}", avc_packet_type),
    }
}

/// Extract AAC audio frame from RTMP audio message
pub fn extract_aac_frame(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 2 {
        bail!("RTMP audio data too short: {}", data.len());
    }
    let first_byte = data[0];
    let sound_format = (first_byte >> 4) & 0x0F;
    if sound_format != 10 {
        bail!("Unsupported audio codec: {}", sound_format);
    }
    Ok(data[2..].to_vec())
}

// ── Tests ──────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ── Client lifecycle tests ──────────────────────────────────────────────────────────────

    #[test]
    fn test_push_client_new() {
        let client = RtmpPushClient::new("nvr.example.com", 1935, "live", "mystream");
        assert_eq!(client.host, "nvr.example.com");
        assert_eq!(client.port, 1935);
        assert_eq!(client.app_name, "live");
        assert_eq!(client.stream_name, "mystream");
        assert!(!client.is_connected());
        assert_eq!(client.stream_id, 0);
    }

    // ── Handshake tests ─────────────────────────────────────────────────────────────────────

    #[test]
    fn test_generate_c1_structure() {
        let c1 = handshake::generate_c1();
        assert_eq!(c1.len(), handshake::HANDSHAKE_SIZE);
        let time = u32::from_be_bytes([c1[0], c1[1], c1[2], c1[3]]);
        assert!(time > 0);
        assert_eq!(&c1[4..8], &[0, 0, 0, 0]);
        let random_sum: u64 = c1[8..].iter().map(|&b| b as u64).sum();
        assert!(random_sum > 0);
    }

    #[test]
    fn test_generate_c2_echoes_s1() {
        let mut s1 = [0u8; handshake::HANDSHAKE_SIZE];
        s1[0..4].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        s1[8..12].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        s1[1535] = 0xFF;

        let mut c1 = [0u8; handshake::HANDSHAKE_SIZE];
        c1[0..4].copy_from_slice(&[0x55, 0x66, 0x77, 0x88]);

        let c2 = handshake::generate_c2(&s1, &c1);
        assert_eq!(&c2[0..4], &s1[0..4]); // echoes S1 time
        assert_eq!(&c2[4..8], &c1[0..4]); // our time
        assert_eq!(&c2[8..], &s1[8..]); // echoes S1 random
    }

    #[test]
    fn test_client_handshake_sequence() {
        // Prepare a deterministic C1 so we can build the mock server response
        let mut c1 = [0u8; handshake::HANDSHAKE_SIZE];
        c1[0..4].copy_from_slice(&[0x00, 0x00, 0x00, 0x01]); // time = 1
        c1[8] = 0xDE;
        c1[9] = 0xAD;
        c1[10] = 0xBE;
        c1[11] = 0xEF;
        c1[1535] = 0xFF;

        // Build server response: S0 + S1 + S2
        let mut server_data = Vec::new();
        server_data.push(handshake::RTMP_VERSION); // S0

        let mut s1 = [0u8; handshake::HANDSHAKE_SIZE];
        s1[0..4].copy_from_slice(&[0x00, 0x00, 0x00, 0x02]); // server time = 2
        s1[8] = 0xCA;
        s1[9] = 0xFE;
        s1[10] = 0xBA;
        s1[11] = 0xBE;
        s1[1535] = 0xEE;
        server_data.extend_from_slice(&s1);

        // S2 must echo C1's time and random
        let mut s2 = [0u8; handshake::HANDSHAKE_SIZE];
        s2[0..4].copy_from_slice(&c1[0..4]); // echo C1 time
        s2[4..8].copy_from_slice(&[0x00, 0x00, 0x00, 0x05]); // time2 (arbitrary)
        s2[8..].copy_from_slice(&c1[8..]); // echo C1 random
        server_data.extend_from_slice(&s2);

        // Perform client handshake
        let mut reader = Cursor::new(server_data);
        let mut writer = Vec::new();
        let result = handshake::perform_client_handshake(&mut reader, &mut writer, &c1);
        assert!(
            result.is_ok(),
            "Client handshake failed: {:?}",
            result.err()
        );

        // Verify client wrote C0 + C1 + C2
        assert!(writer.len() >= 1 + handshake::HANDSHAKE_SIZE + handshake::HANDSHAKE_SIZE);
        assert_eq!(writer[0], handshake::RTMP_VERSION); // C0

        let written_c1 = &writer[1..1 + handshake::HANDSHAKE_SIZE];
        assert_eq!(&written_c1[0..4], &c1[0..4]); // same time
        assert_eq!(&written_c1[8..12], &c1[8..12]); // same random

        // C2 echoes S1
        let written_c2 = &writer[1 + handshake::HANDSHAKE_SIZE..];
        assert_eq!(&written_c2[0..4], &s1[0..4]); // echoes S1 time
        assert_eq!(&written_c2[8..12], &s1[8..12]); // echoes S1 random
    }

    // ── AMF0 encoding tests ────────────────────────────────────────────────────────────────

    #[test]
    fn test_amf0_connect_encoding() {
        let encoded = RtmpPushClient::encode_connect("myApp", "localhost", 1935);
        let (first, consumed) = Amf0Value::parse(&encoded).unwrap();
        assert_eq!(first, Amf0Value::String("connect".to_string()));

        // Verify transaction ID = 1.0
        let (second, c) = Amf0Value::parse(&encoded[consumed..]).unwrap();
        assert_eq!(second, Amf0Value::Number(1.0));

        // Verify app field in the command object
        let pos = consumed + c;
        let (obj, _) = Amf0Value::parse(&encoded[pos..]).unwrap();
        match obj {
            Amf0Value::Object(ref entries) => {
                let app_val = entries.iter().find(|(k, _)| k == "app").map(|(_, v)| v);
                assert_eq!(app_val, Some(&Amf0Value::String("myApp".to_string())));
            }
            other => panic!("Expected Object, got {:?}", other),
        }
    }

    #[test]
    fn test_amf0_create_stream_encoding() {
        let encoded = RtmpPushClient::encode_create_stream();

        let (first, consumed) = Amf0Value::parse(&encoded).unwrap();
        assert_eq!(first, Amf0Value::String("createStream".to_string()));
        let (second, c) = Amf0Value::parse(&encoded[consumed..]).unwrap();
        assert_eq!(second, Amf0Value::Number(2.0));

        let pos = consumed + c;
        let (third, _) = Amf0Value::parse(&encoded[pos..]).unwrap();
        assert_eq!(third, Amf0Value::Null);
    }

    #[test]
    fn test_amf0_publish_encoding() {
        let encoded = RtmpPushClient::encode_publish("mystream");

        let mut pos = 0;
        let (first, c) = Amf0Value::parse(&encoded).unwrap();
        assert_eq!(first, Amf0Value::String("publish".to_string()));
        pos += c;

        let (second, c) = Amf0Value::parse(&encoded[pos..]).unwrap();
        assert_eq!(second, Amf0Value::Number(0.0));
        pos += c;

        let (third, c) = Amf0Value::parse(&encoded[pos..]).unwrap();
        assert_eq!(third, Amf0Value::Null);
        pos += c;

        let (fourth, _) = Amf0Value::parse(&encoded[pos..]).unwrap();
        assert_eq!(fourth, Amf0Value::String("mystream".to_string()));
    }

    // ── Chunk protocol tests ────────────────────────────────────────────────────────────────

    #[test]
    fn test_video_chunk_structure() {
        let video_payload = build_video_nalus(
            &[0x00, 0x00, 0x00, 0x05, 0x67, 0x42, 0x80, 0x0A, 0xFF],
            true,
            0,
        );

        let basic = ChunkBasicHeader {
            chunk_type: ChunkType::Type0,
            chunk_stream_id: 5,
        };
        let bh = basic.serialize();

        let msg_header = ChunkMessageHeader {
            timestamp: 1000,
            message_length: video_payload.len() as u32,
            message_type: MessageType::Video,
            message_stream_id: Some(1),
            extended: false,
        };
        let mh = msg_header.serialize(ChunkType::Type0);

        // CSID 5 uses 1-byte basic header
        assert_eq!(bh.len(), 1);
        assert_eq!(bh[0] & 0x3F, 5);

        // Type 0 message header is 11 bytes
        assert_eq!(mh.len(), 11);

        // Message type at byte 6 of message header
        assert_eq!(mh[6], MessageType::Video.to_u8());

        // Message stream ID at bytes 7-10 (LE)
        assert_eq!(u32::from_le_bytes([mh[7], mh[8], mh[9], mh[10]]), 1);

        // Message length at bytes 3-5 of message header (3 bytes BE)
        let msg_len = u32::from_be_bytes([0, mh[3], mh[4], mh[5]]);
        assert_eq!(msg_len as usize, video_payload.len());

        // Build full chunk as the client would write it
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&bh);
        chunk.extend_from_slice(&mh);
        chunk.extend_from_slice(&video_payload);

        // Verify round-trip: parse back with ChunkStreamParser
        let mut parser = ChunkStreamParser::new();
        let mut cursor = Cursor::new(&chunk);
        let parsed = parser.parse_chunk(&mut cursor).unwrap().unwrap();
        assert_eq!(parsed, video_payload);
    }

    #[test]
    fn test_audio_chunk_structure() {
        let audio_payload = build_audio_raw(&[0x11, 0x90]);

        let basic = ChunkBasicHeader {
            chunk_type: ChunkType::Type0,
            chunk_stream_id: 4,
        };
        let bh = basic.serialize();

        let msg_header = ChunkMessageHeader {
            timestamp: 500,
            message_length: audio_payload.len() as u32,
            message_type: MessageType::Audio,
            message_stream_id: Some(1),
            extended: false,
        };
        let mh = msg_header.serialize(ChunkType::Type0);

        assert_eq!(bh[0] & 0x3F, 4);
        assert_eq!(mh[6], MessageType::Audio.to_u8());

        // Round-trip
        let mut parser = ChunkStreamParser::new();
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&bh);
        chunk.extend_from_slice(&mh);
        chunk.extend_from_slice(&audio_payload);

        let mut cursor = Cursor::new(&chunk);
        let parsed = parser.parse_chunk(&mut cursor).unwrap().unwrap();
        assert_eq!(parsed, audio_payload);
    }

    #[test]
    fn test_chunked_large_message() {
        // Simulate a large video frame that spans multiple 128-byte chunks
        let payload_size = 300;
        let video_payload: Vec<u8> = (0..payload_size).map(|i| (i % 256) as u8).collect();

        // Write as chunked message (chunk_size = 128)
        let mut buf = Vec::new();

        let basic0 = ChunkBasicHeader {
            chunk_type: ChunkType::Type0,
            chunk_stream_id: 5,
        };
        buf.extend_from_slice(&basic0.serialize());

        let mh = ChunkMessageHeader {
            timestamp: 2000,
            message_length: video_payload.len() as u32,
            message_type: MessageType::Video,
            message_stream_id: Some(1),
            extended: false,
        };
        buf.extend_from_slice(&mh.serialize(ChunkType::Type0));

        // First 128 bytes
        buf.extend_from_slice(&video_payload[..128]);

        // Continuation chunk 1 (Type 3)
        let cont1 = ChunkBasicHeader {
            chunk_type: ChunkType::Type3,
            chunk_stream_id: 5,
        };
        buf.extend_from_slice(&cont1.serialize());
        buf.extend_from_slice(&video_payload[128..256]);

        // Continuation chunk 2 (Type 3)
        let cont2 = ChunkBasicHeader {
            chunk_type: ChunkType::Type3,
            chunk_stream_id: 5,
        };
        buf.extend_from_slice(&cont2.serialize());
        buf.extend_from_slice(&video_payload[256..]);

        // Parse back — must loop to reassemble chunked message
        let mut parser = ChunkStreamParser::new();
        let mut cursor = Cursor::new(&buf);
        let parsed = loop {
            match parser.parse_chunk(&mut cursor) {
                Ok(Some(msg)) => break msg,
                Ok(None) => continue,
                Err(e) => panic!("Parse error: {}", e),
            }
        };

        assert_eq!(parsed.len(), payload_size);
        assert_eq!(parsed, video_payload);
    }

    // ── Video/audio helper tests ────────────────────────────────────────────────────────────

    #[test]
    fn test_build_video_sequence_header() {
        let sps = vec![0x67, 0x42, 0x80, 0x1E, 0x00];
        let pps = vec![0x68, 0xCE, 0x3C, 0x80];
        let result = build_video_sequence_header(&sps, &pps);

        assert_eq!(result[0], 0x17); // keyframe + AVC
        assert_eq!(result[4], 0x00); // AVC seq header
        // Verify it round-trips through extract
        let nal_units = extract_h264_nal_units(&result).unwrap();
        assert_eq!(nal_units.len(), 2);
        assert_eq!(nal_units[0], sps);
        assert_eq!(nal_units[1], pps);
    }

    #[test]
    fn test_build_video_nalus() {
        let nal = vec![0x00, 0x00, 0x00, 0x05, 0x67, 0x42, 0x80, 0x0A, 0xFF];
        let result = build_video_nalus(&nal, true, 0);

        assert_eq!(result[0] >> 4, 1); // keyframe
        assert_eq!(result[0] & 0x0F, 7); // AVC
        assert_eq!(result[4], 0x01); // AVC NALU packet

        let nal_units = extract_h264_nal_units(&result).unwrap();
        assert_eq!(nal_units.len(), 1);
        assert_eq!(nal_units[0], vec![0x67, 0x42, 0x80, 0x0A, 0xFF]);
    }

    #[test]
    fn test_build_audio_sequence_header() {
        let config = vec![0x11, 0x90];
        let result = build_audio_sequence_header(&config);
        assert_eq!(result[0] >> 4, 10); // AAC
        assert_eq!(result[1], 0x00); // seq header
        assert_eq!(&result[2..], &config);
    }

    #[test]
    fn test_build_audio_raw() {
        let aac = vec![0xFF, 0xF1, 0x4C, 0x80, 0x00];
        let result = build_audio_raw(&aac);
        assert_eq!(result[0] >> 4, 10); // AAC
        assert_eq!(result[1], 0x01); // raw data
        assert_eq!(&result[2..], &aac);
    }

    // ── Frame extraction tests (preserved from original) ────────────────────────────────────

    #[test]
    fn test_extract_h264_nal_units_sequence_header() {
        let mut data = vec![0x17];
        data.extend_from_slice(&[0x00, 0x00, 0x00]);
        data.push(0x00);
        data.push(0x01);
        data.push(0x42);
        data.push(0x80);
        data.push(0x1e);
        data.push(0xff);
        data.push(0xe1);
        data.extend_from_slice(&[0x00, 0x04]);
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        data.push(0x01);
        data.extend_from_slice(&[0x00, 0x04]);
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]);
        let nal_units = extract_h264_nal_units(&data).unwrap();
        assert_eq!(nal_units.len(), 2);
        assert_eq!(nal_units[0], vec![0x00, 0x00, 0x00, 0x01]);
        assert_eq!(nal_units[1], vec![0x00, 0x00, 0x00, 0x02]);
    }

    #[test]
    fn test_extract_h264_nal_units_with_length_prefix() {
        let mut data = vec![0x17];
        data.extend_from_slice(&[0x00, 0x00, 0x00]);
        data.push(0x01);
        data.extend_from_slice(&[0x00, 0x00, 0x00]);
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]);
        data.extend_from_slice(&[0x67, 0x42, 0x80, 0x0A, 0xFF]);
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x04]);
        data.extend_from_slice(&[0x68, 0xCE, 0x3C, 0x80]);
        let nal_units = extract_h264_nal_units(&data).unwrap();
        assert_eq!(nal_units.len(), 2);
        assert_eq!(nal_units[0], vec![0x67, 0x42, 0x80, 0x0A, 0xFF]);
        assert_eq!(nal_units[1], vec![0x68, 0xCE, 0x3C, 0x80]);
    }

    #[test]
    fn test_extract_aac_sequence_header() {
        let mut data = vec![0xAF];
        data.push(0x00);
        data.extend_from_slice(&[0x11, 0x90]);
        let aac_frame = extract_aac_frame(&data).unwrap();
        assert_eq!(aac_frame, vec![0x11, 0x90]);
    }

    #[test]
    fn test_extract_aac_raw_data() {
        let mut data = vec![0xAF];
        data.push(0x01);
        data.extend_from_slice(&[0xFF, 0xF1, 0x4C, 0x80, 0x00]);
        let aac_frame = extract_aac_frame(&data).unwrap();
        assert_eq!(aac_frame, vec![0xFF, 0xF1, 0x4C, 0x80, 0x00]);
    }

    #[test]
    fn test_extract_h264_unsupported_codec() {
        let mut data = vec![0x14];
        data.extend_from_slice(&[0x00, 0x00, 0x00]);
        data.push(0x00);
        let result = extract_h264_nal_units(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_aac_unsupported_codec() {
        let data = vec![0x20, 0x00];
        let result = extract_aac_frame(&data);
        assert!(result.is_err());
    }

    // ── Re-connection tests ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_push_client_reconnect_state() {
        let mut client = RtmpPushClient::new("nvr.example.com", 1935, "live", "mystream");
        assert!(!client.is_connected());

        // Simulate close when not connected (should be safe)
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            client.close().await.unwrap();
        });
        assert!(!client.is_connected());
    }

    #[test]
    fn test_push_client_new_defaults() {
        let client = RtmpPushClient::new("host", 1935, "app", "stream");
        assert_eq!(client.video_timestamp, 0);
        assert_eq!(client.audio_timestamp, 0);
        assert_eq!(client.chunk_size, 128);
        assert_eq!(client.stream_id, 0);
    }
}
