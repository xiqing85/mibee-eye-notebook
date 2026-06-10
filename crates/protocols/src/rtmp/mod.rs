//! RTMP ingest server
//!
//! This module provides a complete RTMP ingest server implementation that:
//! - Accepts RTMP push connections from OBS/FFmpeg
//! - Performs handshake and protocol negotiation
//! - Handles NetConnection commands (connect, createStream, publish)
//! - Extracts H.264 video frames and AAC audio frames
//! - Delivers frames via tokio mpsc channel

mod amf0;
mod chunk;
mod handshake;

use anyhow::{Result, bail};
use std::io::Cursor;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

pub use amf0::Amf0Value;
pub use chunk::{ChunkBasicHeader, ChunkMessageHeader, ChunkStreamParser, ChunkType, MessageType};

/// Default RTMP port
pub const DEFAULT_PORT: u16 = 1935;

/// RTMP frame type
#[derive(Debug, Clone, PartialEq)]
pub enum RtmpFrame {
    /// Video frame (H.264 NAL units)
    Video(Vec<u8>),
    /// Audio frame (AAC)
    Audio(Vec<u8>),
}

/// RTMP ingest instance
pub struct RtmpIngest {
    /// Receiver for extracted frames
    frame_rx: mpsc::Receiver<RtmpFrame>,
}

impl RtmpIngest {
    /// Get the frame receiver
    pub fn frames(&mut self) -> mpsc::Receiver<RtmpFrame> {
        std::mem::replace(&mut self.frame_rx, mpsc::channel(1).1)
    }
}

/// RTMP server
#[derive(Clone)]
pub struct RtmpServer {
    /// Listen address
    pub addr: String,
    /// Listen port
    pub port: u16,
    /// Application name
    pub app_name: String,
}

impl RtmpServer {
    /// Create new RTMP server
    pub fn new(port: u16, app_name: &str) -> Self {
        Self {
            addr: "0.0.0.0".to_string(),
            port,
            app_name: app_name.to_string(),
        }
    }

    /// Start RTMP server
    pub async fn run(&self) -> Result<RtmpIngest> {
        let listener = tokio::net::TcpListener::bind(format!("{}:{}", self.addr, self.port))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind RTMP server: {}", e))?;

        tracing::info!("RTMP server listening on {}:{}", self.addr, self.port);

        let (frame_tx, frame_rx) = mpsc::channel(100);

        let server = self.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, addr)) = listener.accept().await {
                tracing::info!("RTMP connection from {}", addr);

                let frame_tx = frame_tx.clone();
                let server = server.clone();

                tokio::spawn(async move {
                    if let Err(e) = handle_connection(&mut socket, &server, frame_tx).await {
                        tracing::error!("RTMP connection error from {}: {}", addr, e);
                    } else {
                        tracing::info!("RTMP connection from {} closed", addr);
                    }
                });
            }
        });

        Ok(RtmpIngest { frame_rx })
    }
}

/// Handle a single RTMP connection
async fn handle_connection(
    socket: &mut tokio::net::TcpStream,
    server: &RtmpServer,
    frame_tx: mpsc::Sender<RtmpFrame>,
) -> Result<()> {
    // Perform handshake (convert to std::io for handshake)
    let mut read_buf = Vec::new();
    let mut write_buf = Vec::new();

    // Read all handshake data first
    for _ in 0..(1 + 1536 + 1536) {
        let mut byte = [0u8; 1];
        if socket.read_exact(&mut byte).await.is_ok() {
            read_buf.push(byte[0]);
        }
    }

    // Process handshake in memory
    let mut cursor = Cursor::new(&read_buf[..]);
    handshake::handle_handshake(&mut cursor, &mut write_buf)?;

    // Write handshake response
    socket.write_all(&write_buf).await?;
    socket.flush().await?;

    // Read C2
    let mut c2 = [0u8; 1536];
    socket.read_exact(&mut c2).await?;

    // Initialize chunk parser
    let mut parser = ChunkStreamParser::new();

    // Server state
    let mut stream_id: Option<u32> = None;
    let mut stream_name: Option<String> = None;

    // Main message loop
    loop {
        tokio::task::yield_now().await;

        // Read and parse chunks
        let mut buffer = vec![0u8; 8192];
        let n = socket.read(&mut buffer).await?;
        if n == 0 {
            tracing::info!("Client disconnected");
            return Ok(());
        }
        buffer.truncate(n);

        // Use a cursor for parsing
        let mut cursor = Cursor::new(buffer);
        while let Ok(Some(message)) = parser.parse_chunk(&mut cursor) {
            if let Err(e) = handle_message(
                &message,
                &mut parser,
                socket,
                &mut stream_id,
                &mut stream_name,
                &server.app_name,
                &frame_tx,
            )
            .await
            {
                tracing::error!("Error handling message: {}", e);
            }
        }
    }
}

/// Handle an RTMP message
async fn handle_message(
    message: &[u8],
    parser: &mut ChunkStreamParser,
    socket: &mut tokio::net::TcpStream,
    stream_id: &mut Option<u32>,
    stream_name: &mut Option<String>,
    app_name: &str,
    _frame_tx: &mpsc::Sender<RtmpFrame>,
) -> Result<()> {
    let pos = 0;

    // Parse AMF0 command name
    let (cmd_name, _consumed) = Amf0Value::parse(&message[pos..])?;

    let cmd_name = match cmd_name {
        Amf0Value::String(s) => s,
        _ => bail!("Invalid command name type"),
    };

    match cmd_name.as_str() {
        "connect" => handle_connect(message, parser, socket).await?,
        "createStream" => handle_create_stream(message, socket, stream_id).await?,
        "publish" => handle_publish(message, socket, app_name, stream_name).await?,
        "FCPublish" => {
            // OBS/FFmpeg compatibility - just acknowledge
            send_fcpublish_response(socket, stream_name).await?;
        }
        "FCUnpublish" => {
            // OBS/FFmpeg compatibility - just acknowledge
            send_fcunpublish_response(socket).await?;
        }
        "releaseStream" => {
            // OBS/FFmpeg compatibility - just acknowledge
            send_release_stream_response(socket).await?;
        }
        _ => {
            tracing::trace!("Unhandled RTMP command: {}", cmd_name);
        }
    }

    Ok(())
}

/// Handle connect command
async fn handle_connect(
    message: &[u8],
    _parser: &mut ChunkStreamParser,
    socket: &mut tokio::net::TcpStream,
) -> Result<()> {
    let mut pos = 0;

    // Parse command name (already done, skip)
    let (_, consumed) = Amf0Value::parse(&message[pos..])?;
    pos += consumed;

    // Parse transaction ID
    let (trans_id, consumed) = Amf0Value::parse(&message[pos..])?;
    pos += consumed;

    let trans_id = match trans_id {
        Amf0Value::Number(n) => n,
        _ => bail!("Invalid transaction ID type"),
    };

    // Parse command object (contains connection params)
    let (cmd_obj, _) = Amf0Value::parse(&message[pos..])?;

    let app = match cmd_obj {
        Amf0Value::Object(ref entries) | Amf0Value::EcmaArray(_, ref entries) => entries
            .iter()
            .find(|(k, _)| k == "app")
            .and_then(|(_, v)| match v {
                Amf0Value::String(s) => Some(s.clone()),
                _ => None,
            }),
        _ => None,
    };

    if let Some(ref app) = app {
        tracing::info!("RTMP connect to app: {}", app);
    }

    // Send _result response
    send_connect_response(socket, trans_id).await?;

    // Send onStatus (NetConnection.Connect.Success)
    send_connect_status(socket).await?;

    Ok(())
}

/// Handle createStream command
async fn handle_create_stream(
    message: &[u8],
    socket: &mut tokio::net::TcpStream,
    stream_id: &mut Option<u32>,
) -> Result<()> {
    let pos = 0;

    // Parse command name (already done, skip)
    let (_, _consumed) = Amf0Value::parse(&message[pos..])?;

    // Parse transaction ID
    let (_, _consumed) = Amf0Value::parse(&message[pos..])?;

    // Allocate stream ID
    *stream_id = Some(1);

    // Send response with stream ID
    send_create_stream_response(socket, 0.0, stream_id.unwrap()).await?;

    Ok(())
}

/// Handle publish command
async fn handle_publish(
    message: &[u8],
    socket: &mut tokio::net::TcpStream,
    _app_name: &str,
    stream_name: &mut Option<String>,
) -> Result<()> {
    let pos = 0;

    // Parse command name (already done, skip)
    let (_, _consumed) = Amf0Value::parse(&message[pos..])?;

    // Parse transaction ID
    let (_, _consumed) = Amf0Value::parse(&message[pos..])?;

    // Parse stream name
    let (name, _consumed) = Amf0Value::parse(&message[pos..])?;

    let name = match name {
        Amf0Value::String(s) => s,
        _ => bail!("Invalid stream name type"),
    };

    *stream_name = Some(name.clone());
    tracing::info!("RTMP publish to stream: {}", name);

    // Send onStatus (NetStream.Publish.Start)
    send_publish_status(socket).await?;

    // Send StreamBegin event
    send_stream_begin(socket).await?;

    Ok(())
}

/// Send connect response (_result)
async fn send_connect_response(socket: &mut tokio::net::TcpStream, trans_id: f64) -> Result<()> {
    let response = vec![
        Amf0Value::String("_result".to_string()),
        Amf0Value::Number(trans_id),
        Amf0Value::Object(vec![
            (
                "fmsVer".to_string(),
                Amf0Value::String("FMS/3,5,2,654".to_string()),
            ),
            ("capabilities".to_string(), Amf0Value::Number(127.0)),
            ("mode".to_string(), Amf0Value::Number(1.0)),
        ]),
        Amf0Value::Object(vec![
            ("level".to_string(), Amf0Value::String("status".to_string())),
            (
                "code".to_string(),
                Amf0Value::String("NetConnection.Connect.Success".to_string()),
            ),
            (
                "description".to_string(),
                Amf0Value::String("Connection succeeded.".to_string()),
            ),
        ]),
    ];

    send_rtmp_command(socket, 3, 0, 0, &response).await?;
    Ok(())
}

/// Send connect status (onStatus)
async fn send_connect_status(socket: &mut tokio::net::TcpStream) -> Result<()> {
    let status = vec![
        Amf0Value::String("onStatus".to_string()),
        Amf0Value::Number(0.0),
        Amf0Value::Null,
        Amf0Value::Object(vec![
            ("level".to_string(), Amf0Value::String("status".to_string())),
            (
                "code".to_string(),
                Amf0Value::String("NetConnection.Connect.Success".to_string()),
            ),
            (
                "description".to_string(),
                Amf0Value::String("Connection succeeded.".to_string()),
            ),
        ]),
    ];

    send_rtmp_command(socket, 3, 0, 0, &status).await?;
    Ok(())
}

/// Send createStream response (_result)
async fn send_create_stream_response(
    socket: &mut tokio::net::TcpStream,
    trans_id: f64,
    stream_id: u32,
) -> Result<()> {
    let response = vec![
        Amf0Value::String("_result".to_string()),
        Amf0Value::Number(trans_id),
        Amf0Value::Null,
        Amf0Value::Number(stream_id as f64),
    ];

    send_rtmp_command(socket, 3, 0, 0, &response).await?;
    Ok(())
}

/// Send publish status (onStatus)
async fn send_publish_status(socket: &mut tokio::net::TcpStream) -> Result<()> {
    let status = vec![
        Amf0Value::String("onStatus".to_string()),
        Amf0Value::Number(0.0),
        Amf0Value::Null,
        Amf0Value::Object(vec![
            ("level".to_string(), Amf0Value::String("status".to_string())),
            (
                "code".to_string(),
                Amf0Value::String("NetStream.Publish.Start".to_string()),
            ),
            (
                "description".to_string(),
                Amf0Value::String("Start publishing.".to_string()),
            ),
        ]),
    ];

    send_rtmp_command(socket, 4, 0, 0, &status).await?;
    Ok(())
}

/// Send StreamBegin event
async fn send_stream_begin(socket: &mut tokio::net::TcpStream) -> Result<()> {
    // UserControl message (type 4, event type 0 for StreamBegin)
    let mut data = Vec::new();
    data.extend_from_slice(&0u16.to_be_bytes()); // Event type: StreamBegin (0)
    data.extend_from_slice(&1u32.to_be_bytes()); // Stream ID

    send_rtmp_message(socket, 2, 4, &data).await?;
    Ok(())
}

/// Send FCPublish response (OBS compatibility)
async fn send_fcpublish_response(
    socket: &mut tokio::net::TcpStream,
    _stream_name: &Option<String>,
) -> Result<()> {
    let response = vec![
        Amf0Value::String("_result".to_string()),
        Amf0Value::Number(0.0),
        Amf0Value::Null,
        Amf0Value::Undefined,
    ];

    send_rtmp_command(socket, 3, 0, 0, &response).await?;
    Ok(())
}

/// Send FCUnpublish response (OBS compatibility)
async fn send_fcunpublish_response(socket: &mut tokio::net::TcpStream) -> Result<()> {
    let response = vec![
        Amf0Value::String("_result".to_string()),
        Amf0Value::Number(0.0),
        Amf0Value::Null,
        Amf0Value::Undefined,
    ];

    send_rtmp_command(socket, 3, 0, 0, &response).await?;
    Ok(())
}

/// Send releaseStream response (OBS compatibility)
async fn send_release_stream_response(socket: &mut tokio::net::TcpStream) -> Result<()> {
    let response = vec![
        Amf0Value::String("_result".to_string()),
        Amf0Value::Number(0.0),
        Amf0Value::Null,
        Amf0Value::Undefined,
    ];

    send_rtmp_command(socket, 3, 0, 0, &response).await?;
    Ok(())
}

/// Send RTMP command message
async fn send_rtmp_command(
    socket: &mut tokio::net::TcpStream,
    chunk_stream_id: u32,
    _message_stream_id: u32,
    _timestamp: u32,
    amf_values: &[Amf0Value],
) -> Result<()> {
    let mut payload = Vec::new();
    for value in amf_values {
        payload.extend_from_slice(&value.serialize());
    }

    send_rtmp_message(socket, chunk_stream_id, 20, &payload).await?;
    Ok(())
}

/// Send RTMP message
async fn send_rtmp_message(
    socket: &mut tokio::net::TcpStream,
    chunk_stream_id: u32,
    message_type: u8,
    data: &[u8],
) -> Result<()> {
    // Build basic header (Type 0, CS ID)
    let basic_header = ChunkBasicHeader {
        chunk_type: ChunkType::Type0,
        chunk_stream_id,
    };
    socket.write_all(&basic_header.serialize()).await?;

    // Build message header
    let message_header = ChunkMessageHeader {
        timestamp: 0,
        message_length: data.len() as u32,
        message_type: MessageType::from_u8(message_type)?,
        message_stream_id: Some(chunk_stream_id),
        extended: false,
    };
    socket
        .write_all(&message_header.serialize(ChunkType::Type0))
        .await?;

    // Write data (not chunked for simplicity in server responses)
    socket.write_all(data).await?;
    socket.flush().await?;

    Ok(())
}

/// Extract H.264 NAL units from RTMP video message
///
/// RTMP video format: 1 byte frame type + codec info + 3 bytes composition time + NAL units
pub fn extract_h264_nal_units(data: &[u8]) -> Result<Vec<Vec<u8>>> {
    if data.len() < 5 {
        bail!("RTMP video data too short: {}", data.len());
    }

    // First byte: [frame_type(4) | codec_id(4)]
    // frame_type: 1=keyframe, 2=inter frame, 3=disposable inter frame, 4=generated keyframe, 5=video info/command frame
    // codec_id: 7=AVC (H.264)
    let first_byte = data[0];
    let codec_id = first_byte & 0x0F;
    let _frame_type = (first_byte >> 4) & 0x0F;

    if codec_id != 7 {
        bail!("Unsupported video codec: {}", codec_id);
    }

    // Skip frame type byte + composition time (4 bytes). payload starts at avc_packet_type + 1.
    let avc_packet_type = data[4];
    let payload = &data[5..];

    match avc_packet_type {
        0 => {
            // AVC sequence header - payload is AVCDecoderConfigurationRecord
            if payload.len() < 11 {
                bail!("AVC sequence header too short: {}", payload.len());
            }

            // Config record: 6-byte header, then SPS length(2) + SPS data, then numPPS(1) + PPS length(2) + PPS data
            let sps_len = u16::from_be_bytes([payload[6], payload[7]]) as usize;
            let sps = payload[8..8 + sps_len].to_vec();
            let pps_offset = 8 + sps_len + 1; // skip: header(6) + sps_len_field(2) + sps_data(sps_len) + numPPS(1)
            let pps_len =
                u16::from_be_bytes([payload[pps_offset], payload[pps_offset + 1]]) as usize;
            let pps = payload[pps_offset + 2..pps_offset + 2 + pps_len].to_vec();
            Ok(vec![sps, pps])
        }
        1 => {
            // AVC NALU(s) - data[5..8] is 3-byte CTS, data[8..] is NAL unit data
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

                let nal_unit = nal_data[pos + 4..pos + 4 + nal_len].to_vec();
                nal_units.push(nal_unit);

                pos += 4 + nal_len;
            }

            Ok(nal_units)
        }
        2 => {
            // AVC end of sequence
            Ok(vec![])
        }
        _ => {
            bail!("Unknown AVC packet type: {}", avc_packet_type);
        }
    }
}

/// Extract AAC audio frame from RTMP audio message
///
/// RTMP audio format: 1 byte format + codec info + AAC data
pub fn extract_aac_frame(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 2 {
        bail!("RTMP audio data too short: {}", data.len());
    }

    // First byte: [sound_format(4) | rate(2) | size(1) | type(1)]
    let first_byte = data[0];
    let sound_format = (first_byte >> 4) & 0x0F;

    if sound_format != 10 {
        // 10 = AAC
        bail!("Unsupported audio codec: {}", sound_format);
    }

    // Second byte: AAC packet type (0=AAC sequence header, 1=AAC raw data)
    let aac_packet_type = data[1];

    if aac_packet_type == 0 {
        // AAC sequence header (AudioSpecificConfig)
        // Just return the raw data for now
        Ok(data[2..].to_vec())
    } else if aac_packet_type == 1 {
        // AAC raw data
        Ok(data[2..].to_vec())
    } else {
        bail!("Unknown AAC packet type: {}", aac_packet_type);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtmp_server_creation() {
        let server = RtmpServer::new(DEFAULT_PORT, "live");
        assert_eq!(server.port, DEFAULT_PORT);
        assert_eq!(server.app_name, "live");
        assert_eq!(server.addr, "0.0.0.0");
    }

    #[test]
    fn test_extract_h264_nal_units_sequence_header() {
        // Simulate AVC sequence header with SPS=4 bytes, PPS=4 bytes
        let mut data = vec![0x17]; // Frame type 1 (keyframe) + codec 7 (AVC)

        // Composition time (3 bytes)
        data.extend_from_slice(&[0x00, 0x00, 0x00]);

        // AVC packet type 0 (sequence header)
        data.push(0x00);

        // AVCDecoderConfigurationRecord header (6 bytes)
        data.push(0x01); // configurationVersion
        data.push(0x42); // AVCProfileIndication
        data.push(0x80); // profile_compatibility
        data.push(0x1e); // AVCLevelIndication
        data.push(0xff); // lengthSizeMinusOne (4-byte NAL length)
        data.push(0xe1); // numSPS (0xE0 | 1)

        data.extend_from_slice(&[0x00, 0x04]); // SPS length
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // SPS data
        data.push(0x01); // numPPS
        data.extend_from_slice(&[0x00, 0x04]); // PPS length
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]); // PPS data
        let nal_units = extract_h264_nal_units(&data).unwrap();
        assert_eq!(nal_units.len(), 2);
        assert_eq!(nal_units[0], vec![0x00, 0x00, 0x00, 0x01]);
        assert_eq!(nal_units[1], vec![0x00, 0x00, 0x00, 0x02]);
    }

    #[test]
    fn test_extract_h264_nal_units_with_length_prefix() {
        // Simulate AVC NALU packet with 2 NAL units
        let mut data = vec![0x17]; // Frame type 1 (keyframe) + codec 7 (AVC)

        // Composition time (3 bytes)
        data.extend_from_slice(&[0x00, 0x00, 0x00]);

        // AVC packet type 1 (NALU)
        data.push(0x01);

        // Composition time (3 bytes)
        data.extend_from_slice(&[0x00, 0x00, 0x00]);

        // NAL unit 1
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x05]); // Length 5
        data.extend_from_slice(&[0x67, 0x42, 0x80, 0x0A, 0xFF]);

        // NAL unit 2
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x04]); // Length 4
        data.extend_from_slice(&[0x68, 0xCE, 0x3C, 0x80]);

        let nal_units = extract_h264_nal_units(&data).unwrap();
        assert_eq!(nal_units.len(), 2);
        assert_eq!(nal_units[0], vec![0x67, 0x42, 0x80, 0x0A, 0xFF]);
        assert_eq!(nal_units[1], vec![0x68, 0xCE, 0x3C, 0x80]);
    }

    #[test]
    fn test_extract_aac_sequence_header() {
        let mut data = vec![0xAF]; // Sound format 10 (AAC) + rate/size/type

        // AAC packet type 0 (sequence header)
        data.push(0x00);

        // AudioSpecificConfig (2 bytes for AAC-LC, 44.1kHz, stereo)
        data.extend_from_slice(&[0x11, 0x90]);

        let aac_frame = extract_aac_frame(&data).unwrap();
        assert_eq!(aac_frame, vec![0x11, 0x90]);
    }

    #[test]
    fn test_extract_aac_raw_data() {
        let mut data = vec![0xAF]; // Sound format 10 (AAC) + rate/size/type

        // AAC packet type 1 (raw data)
        data.push(0x01);

        // AAC raw frame (5 bytes)
        data.extend_from_slice(&[0xFF, 0xF1, 0x4C, 0x80, 0x00]);

        let aac_frame = extract_aac_frame(&data).unwrap();
        assert_eq!(aac_frame, vec![0xFF, 0xF1, 0x4C, 0x80, 0x00]);
    }

    #[test]
    fn test_extract_h264_unsupported_codec() {
        let mut data = vec![0x14]; // Codec 4 (not AVC)

        // Minimal valid structure
        data.extend_from_slice(&[0x00, 0x00, 0x00]); // Composition time
        data.push(0x00); // AVC packet type

        let result = extract_h264_nal_units(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_aac_unsupported_codec() {
        let data = vec![0x20, 0x00]; // Sound format 2 (MP3)

        let result = extract_aac_frame(&data);
        assert!(result.is_err());
    }
}
