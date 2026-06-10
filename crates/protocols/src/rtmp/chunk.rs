//! RTMP chunk protocol
//!
//! RTMP chunks have the following structure:
//! - Basic header (1-3 bytes): fmt (2 bits) + chunk stream ID (6 bits)
//! - Message header (0/3/7/11 bytes depending on fmt)
//! - Extended timestamp (0 or 4 bytes, if timestamp/delta >= 0xFFFFFF)
//! - Chunk data

use anyhow::{Context, Result, bail};
use std::io::Read;

/// Chunk type (fmt field in basic header)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkType {
    /// Type 0: 11-byte header (absolute timestamp, full message info)
    Type0,
    /// Type 1: 7-byte header (timestamp delta, message length, message type)
    Type1,
    /// Type 2: 3-byte header (timestamp delta only)
    Type2,
    /// Type 3: 0-byte header (continuation, uses previous header values)
    Type3,
}

impl ChunkType {
    /// Create ChunkType from fmt value (0-3)
    pub fn from_u8(val: u8) -> Result<Self> {
        match val {
            0 => Ok(ChunkType::Type0),
            1 => Ok(ChunkType::Type1),
            2 => Ok(ChunkType::Type2),
            3 => Ok(ChunkType::Type3),
            _ => bail!("Invalid chunk type: {}", val),
        }
    }

    /// Get fmt value (0-3)
    pub fn to_u8(self) -> u8 {
        match self {
            ChunkType::Type0 => 0,
            ChunkType::Type1 => 1,
            ChunkType::Type2 => 2,
            ChunkType::Type3 => 3,
        }
    }

    /// Get message header size in bytes
    pub fn header_size(self) -> usize {
        match self {
            ChunkType::Type0 => 11,
            ChunkType::Type1 => 7,
            ChunkType::Type2 => 3,
            ChunkType::Type3 => 0,
        }
    }
}

/// RTMP message types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    SetChunkSize = 1,
    AbortMessage = 2,
    Acknowledgement = 3,
    UserControlMessage = 4,
    WindowAckSize = 5,
    SetPeerBandwidth = 6,
    Audio = 8,
    Video = 9,
    Data = 18,
    SharedObject = 19,
    Command = 20,
    Aggregate = 22,
}

impl MessageType {
    /// Create from u8
    pub fn from_u8(val: u8) -> Result<Self> {
        match val {
            1 => Ok(MessageType::SetChunkSize),
            2 => Ok(MessageType::AbortMessage),
            3 => Ok(MessageType::Acknowledgement),
            4 => Ok(MessageType::UserControlMessage),
            5 => Ok(MessageType::WindowAckSize),
            6 => Ok(MessageType::SetPeerBandwidth),
            8 => Ok(MessageType::Audio),
            9 => Ok(MessageType::Video),
            18 => Ok(MessageType::Data),
            19 => Ok(MessageType::SharedObject),
            20 => Ok(MessageType::Command),
            22 => Ok(MessageType::Aggregate),
            _ => bail!("Unknown message type: {}", val),
        }
    }

    /// Convert to u8
    pub fn to_u8(self) -> u8 {
        self as u8
    }
}

/// Chunk basic header
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkBasicHeader {
    pub chunk_type: ChunkType,
    pub chunk_stream_id: u32,
}

impl ChunkBasicHeader {
    /// Maximum chunk stream ID for 1-byte encoding
    pub const MAX_1BYTE_CSID: u32 = 63;
    /// Maximum chunk stream ID for 2-byte encoding
    pub const MAX_2BYTE_CSID: u32 = 319;

    /// Parse basic header from reader
    pub fn parse<R: Read>(reader: &mut R) -> Result<Self> {
        let mut first_byte = [0u8; 1];
        reader
            .read_exact(&mut first_byte)
            .context("Failed to read first byte of basic header")?;

        let fmt = (first_byte[0] >> 6) & 0x03;
        let chunk_type = ChunkType::from_u8(fmt)?;
        let cs_id = (first_byte[0] & 0x3F) as u32;

        let chunk_stream_id = match cs_id {
            0 => {
                // Format: 0 + 1 byte (cs_id - 64)
                let mut byte = [0u8; 1];
                reader
                    .read_exact(&mut byte)
                    .context("Failed to read 2-byte CSID")?;
                byte[0] as u32 + 64
            }
            1 => {
                // Format: 1 + 2 bytes (lsb, msb)
                let mut bytes = [0u8; 2];
                reader
                    .read_exact(&mut bytes)
                    .context("Failed to read 3-byte CSID")?;
                bytes[0] as u32 + (bytes[1] as u32 * 256) + 64
            }
            2 => {
                // Protocol control message
                2
            }
            id @ 3..=Self::MAX_1BYTE_CSID => id,
            _ => bail!("Invalid chunk stream ID: {}", cs_id),
        };

        Ok(ChunkBasicHeader {
            chunk_type,
            chunk_stream_id,
        })
    }

    /// Serialize basic header
    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        let fmt = self.chunk_type.to_u8();
        let cs_id = self.chunk_stream_id;

        if (2..=Self::MAX_1BYTE_CSID).contains(&cs_id) {
            // 1-byte format
            bytes.push((fmt << 6) | (cs_id as u8));
        } else if (64..=Self::MAX_2BYTE_CSID).contains(&cs_id) {
            // 2-byte format
            bytes.push(fmt << 6);
            bytes.push((cs_id - 64) as u8);
        } else if (320..=65599).contains(&cs_id) {
            // 3-byte format
            bytes.push((fmt << 6) | 1);
            bytes.push(((cs_id - 64) % 256) as u8);
            bytes.push(((cs_id - 64) / 256) as u8);
        } else {
            panic!("Invalid chunk stream ID: {}", cs_id);
        }

        bytes
    }
}

/// Chunk message header
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkMessageHeader {
    /// Absolute timestamp (Type 0) or timestamp delta (Type 1/2)
    pub timestamp: u32,
    /// Message length in bytes (Type 0/1)
    pub message_length: u32,
    /// Message type (Type 0/1)
    pub message_type: MessageType,
    /// Message stream ID (Type 0 only, little-endian)
    pub message_stream_id: Option<u32>,
    /// Whether timestamp field is using extended timestamp
    pub extended: bool,
}

impl ChunkMessageHeader {
    /// Threshold for extended timestamp (0xFFFFFF)
    pub const EXTENDED_TIMESTAMP_THRESHOLD: u32 = 0xFFFFFF;

    /// Parse message header from reader
    pub fn parse<R: Read>(reader: &mut R, chunk_type: ChunkType) -> Result<Self> {
        match chunk_type {
            ChunkType::Type0 => Self::parse_type0(reader),
            ChunkType::Type1 => Self::parse_type1(reader),
            ChunkType::Type2 => Self::parse_type2(reader),
            ChunkType::Type3 => Ok(Self::default()),
        }
    }

    fn parse_type0<R: Read>(reader: &mut R) -> Result<Self> {
        let mut buf = [0u8; 11];
        reader
            .read_exact(&mut buf)
            .context("Failed to read Type 0 header")?;

        // Timestamp (3 bytes, big-endian)
        let timestamp = u32::from_be_bytes([0, buf[0], buf[1], buf[2]]);

        // Message length (3 bytes, big-endian)
        let message_length = u32::from_be_bytes([0, buf[3], buf[4], buf[5]]);

        // Message type (1 byte)
        let message_type = MessageType::from_u8(buf[6])?;

        // Message stream ID (4 bytes, little-endian)
        let message_stream_id = u32::from_le_bytes([buf[7], buf[8], buf[9], buf[10]]);

        // Check for extended timestamp
        let extended = timestamp == Self::EXTENDED_TIMESTAMP_THRESHOLD;
        let timestamp = if extended {
            let mut ext_buf = [0u8; 4];
            reader
                .read_exact(&mut ext_buf)
                .context("Failed to read extended timestamp")?;
            u32::from_be_bytes(ext_buf)
        } else {
            timestamp
        };

        Ok(ChunkMessageHeader {
            timestamp,
            message_length,
            message_type,
            message_stream_id: Some(message_stream_id),
            extended,
        })
    }

    fn parse_type1<R: Read>(reader: &mut R) -> Result<Self> {
        let mut buf = [0u8; 7];
        reader
            .read_exact(&mut buf)
            .context("Failed to read Type 1 header")?;

        // Timestamp delta (3 bytes, big-endian)
        let timestamp = u32::from_be_bytes([0, buf[0], buf[1], buf[2]]);

        // Message length (3 bytes, big-endian)
        let message_length = u32::from_be_bytes([0, buf[3], buf[4], buf[5]]);

        // Message type (1 byte)
        let message_type = MessageType::from_u8(buf[6])?;

        // Check for extended timestamp
        let extended = timestamp == Self::EXTENDED_TIMESTAMP_THRESHOLD;
        let timestamp = if extended {
            let mut ext_buf = [0u8; 4];
            reader
                .read_exact(&mut ext_buf)
                .context("Failed to read extended timestamp")?;
            u32::from_be_bytes(ext_buf)
        } else {
            timestamp
        };

        Ok(ChunkMessageHeader {
            timestamp,
            message_length,
            message_type,
            message_stream_id: None,
            extended,
        })
    }

    fn parse_type2<R: Read>(reader: &mut R) -> Result<Self> {
        let mut buf = [0u8; 3];
        reader
            .read_exact(&mut buf)
            .context("Failed to read Type 2 header")?;

        // Timestamp delta (3 bytes, big-endian)
        let timestamp = u32::from_be_bytes([0, buf[0], buf[1], buf[2]]);

        // Check for extended timestamp
        let extended = timestamp == Self::EXTENDED_TIMESTAMP_THRESHOLD;
        let timestamp = if extended {
            let mut ext_buf = [0u8; 4];
            reader
                .read_exact(&mut ext_buf)
                .context("Failed to read extended timestamp")?;
            u32::from_be_bytes(ext_buf)
        } else {
            timestamp
        };

        Ok(ChunkMessageHeader {
            timestamp,
            message_length: 0,
            message_type: MessageType::Command, // Placeholder
            message_stream_id: None,
            extended,
        })
    }

    /// Serialize message header
    pub fn serialize(&self, chunk_type: ChunkType) -> Vec<u8> {
        let mut bytes = Vec::new();

        match chunk_type {
            ChunkType::Type0 => {
                let ts = if self.extended {
                    Self::EXTENDED_TIMESTAMP_THRESHOLD
                } else {
                    self.timestamp.min(Self::EXTENDED_TIMESTAMP_THRESHOLD)
                };
                bytes.extend_from_slice(&ts.to_be_bytes()[1..4]);

                let len = self.message_length.min(Self::EXTENDED_TIMESTAMP_THRESHOLD);
                bytes.extend_from_slice(&len.to_be_bytes()[1..4]);

                bytes.push(self.message_type.to_u8());

                if let Some(msid) = self.message_stream_id {
                    bytes.extend_from_slice(&msid.to_le_bytes());
                }

                if self.extended {
                    bytes.extend_from_slice(&self.timestamp.to_be_bytes());
                }
            }
            ChunkType::Type1 => {
                let ts = if self.extended {
                    Self::EXTENDED_TIMESTAMP_THRESHOLD
                } else {
                    self.timestamp.min(Self::EXTENDED_TIMESTAMP_THRESHOLD)
                };
                bytes.extend_from_slice(&ts.to_be_bytes()[1..4]);

                let len = self.message_length.min(Self::EXTENDED_TIMESTAMP_THRESHOLD);
                bytes.extend_from_slice(&len.to_be_bytes()[1..4]);

                bytes.push(self.message_type.to_u8());

                if self.extended {
                    bytes.extend_from_slice(&self.timestamp.to_be_bytes());
                }
            }
            ChunkType::Type2 => {
                let ts = if self.extended {
                    Self::EXTENDED_TIMESTAMP_THRESHOLD
                } else {
                    self.timestamp.min(Self::EXTENDED_TIMESTAMP_THRESHOLD)
                };
                bytes.extend_from_slice(&ts.to_be_bytes()[1..4]);

                if self.extended {
                    bytes.extend_from_slice(&self.timestamp.to_be_bytes());
                }
            }
            ChunkType::Type3 => {
                // No message header
            }
        }

        bytes
    }

    fn default() -> Self {
        ChunkMessageHeader {
            timestamp: 0,
            message_length: 0,
            message_type: MessageType::Command,
            message_stream_id: None,
            extended: false,
        }
    }
}

/// Chunk stream state for parsing
#[derive(Debug, Clone, Default)]
pub struct ChunkStreamState {
    pub last_header: Option<ChunkMessageHeader>,
    pub received_bytes: usize,
}

/// Chunk stream parser
#[derive(Debug, Default)]
pub struct ChunkStreamParser {
    /// Per-chunk-stream state
    pub streams: std::collections::HashMap<u32, ChunkStreamState>,
    /// Current chunk size (default 128 bytes)
    pub chunk_size: u32,
    /// Accumulated message for current chunk
    pub current_message: Vec<u8>,
}

impl ChunkStreamParser {
    /// Default chunk size
    pub const DEFAULT_CHUNK_SIZE: u32 = 128;

    pub fn new() -> Self {
        Self {
            chunk_size: Self::DEFAULT_CHUNK_SIZE,
            ..Default::default()
        }
    }

    /// Parse a chunk from reader
    pub fn parse_chunk<R: Read>(&mut self, reader: &mut R) -> Result<Option<Vec<u8>>> {
        // Parse basic header
        let basic_header = ChunkBasicHeader::parse(reader)?;
        tracing::trace!(
            "Chunk: type={:?}, cs_id={}",
            basic_header.chunk_type,
            basic_header.chunk_stream_id
        );

        // Get or create stream state
        let state = self
            .streams
            .entry(basic_header.chunk_stream_id)
            .or_default();

        // Parse message header (Type 3 uses previous header)
        let header = match basic_header.chunk_type {
            ChunkType::Type3 => {
                if let Some(ref last_header) = state.last_header {
                    last_header.clone()
                } else {
                    bail!(
                        "Type 3 chunk without previous header for cs_id={}",
                        basic_header.chunk_stream_id
                    );
                }
            }
            _ => {
                let header = ChunkMessageHeader::parse(reader, basic_header.chunk_type)?;
                state.last_header = Some(header.clone());
                header
            }
        };

        // Calculate data size for this chunk
        let remaining = (header.message_length as usize).saturating_sub(state.received_bytes);
        let chunk_data_size = remaining.min(self.chunk_size as usize);

        // Read chunk data
        let mut chunk_data = vec![0u8; chunk_data_size];
        reader
            .read_exact(&mut chunk_data)
            .context("Failed to read chunk data")?;

        // Accumulate data
        state.received_bytes += chunk_data_size;
        self.current_message.extend_from_slice(&chunk_data);

        // Check if extended timestamp is present
        if header.extended {
            let mut ext_ts = [0u8; 4];
            reader
                .read_exact(&mut ext_ts)
                .context("Failed to read extended timestamp")?;
        }

        // Check if message is complete
        if state.received_bytes >= header.message_length as usize {
            let message = std::mem::take(&mut self.current_message);
            state.received_bytes = 0;
            tracing::debug!(
                "Complete message: type={:?}, length={}, cs_id={}",
                header.message_type,
                message.len(),
                basic_header.chunk_stream_id
            );
            Ok(Some(message))
        } else {
            Ok(None)
        }
    }

    /// Handle SetChunkSize control message
    pub fn handle_set_chunk_size(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < 4 {
            bail!("SetChunkSize data too short: {}", data.len());
        }

        let chunk_size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let chunk_size = chunk_size & 0x7FFFFFFF; // Clear MSB

        if !(1..=0xFFFFFF).contains(&chunk_size) {
            bail!("Invalid chunk size: {}", chunk_size);
        }

        self.chunk_size = chunk_size;
        tracing::info!("Chunk size updated to {}", chunk_size);
        Ok(())
    }

    /// Handle WindowAckSize control message
    pub fn handle_window_ack_size(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < 4 {
            bail!("WindowAckSize data too short: {}", data.len());
        }

        let window_size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        tracing::info!("Window acknowledgement size: {}", window_size);
        // TODO: Implement acknowledgment logic
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_type_conversion() {
        assert_eq!(ChunkType::from_u8(0).unwrap(), ChunkType::Type0);
        assert_eq!(ChunkType::from_u8(1).unwrap(), ChunkType::Type1);
        assert_eq!(ChunkType::from_u8(2).unwrap(), ChunkType::Type2);
        assert_eq!(ChunkType::from_u8(3).unwrap(), ChunkType::Type3);
        assert!(ChunkType::from_u8(4).is_err());

        assert_eq!(ChunkType::Type0.to_u8(), 0);
        assert_eq!(ChunkType::Type1.to_u8(), 1);
        assert_eq!(ChunkType::Type2.to_u8(), 2);
        assert_eq!(ChunkType::Type3.to_u8(), 3);

        assert_eq!(ChunkType::Type0.header_size(), 11);
        assert_eq!(ChunkType::Type1.header_size(), 7);
        assert_eq!(ChunkType::Type2.header_size(), 3);
        assert_eq!(ChunkType::Type3.header_size(), 0);
    }

    #[test]
    fn test_message_type_conversion() {
        assert_eq!(MessageType::from_u8(1).unwrap(), MessageType::SetChunkSize);
        assert_eq!(MessageType::from_u8(8).unwrap(), MessageType::Audio);
        assert_eq!(MessageType::from_u8(9).unwrap(), MessageType::Video);
        assert_eq!(MessageType::from_u8(20).unwrap(), MessageType::Command);
        assert!(MessageType::from_u8(99).is_err());

        assert_eq!(MessageType::Audio.to_u8(), 8);
        assert_eq!(MessageType::Video.to_u8(), 9);
        assert_eq!(MessageType::Command.to_u8(), 20);
    }

    #[test]
    fn test_basic_header_1byte() {
        let header = ChunkBasicHeader {
            chunk_type: ChunkType::Type0,
            chunk_stream_id: 5,
        };

        let bytes = header.serialize();
        assert_eq!(bytes.len(), 1);
        assert_eq!(bytes[0], 0x05); // (0 << 6) | 5

        let parsed = ChunkBasicHeader::parse(&mut bytes.as_slice()).unwrap();
        assert_eq!(parsed, header);
    }

    #[test]
    fn test_basic_header_2byte() {
        let header = ChunkBasicHeader {
            chunk_type: ChunkType::Type1,
            chunk_stream_id: 100,
        };

        let bytes = header.serialize();
        assert_eq!(bytes.len(), 2);
        assert_eq!(bytes[0], 0x40); // (1 << 6) | 0
        assert_eq!(bytes[1], 100 - 64);

        let parsed = ChunkBasicHeader::parse(&mut bytes.as_slice()).unwrap();
        assert_eq!(parsed, header);
    }

    #[test]
    fn test_basic_header_3byte() {
        let header = ChunkBasicHeader {
            chunk_type: ChunkType::Type2,
            chunk_stream_id: 500,
        };

        let bytes = header.serialize();
        assert_eq!(bytes.len(), 3);
        assert_eq!(bytes[0], 0x81); // (2 << 6) | 1
        // 500 - 64 = 436 = 0x01B4
        assert_eq!(bytes[1], 0xB4); // LSB
        assert_eq!(bytes[2], 0x01); // MSB

        let parsed = ChunkBasicHeader::parse(&mut bytes.as_slice()).unwrap();
        assert_eq!(parsed, header);
    }

    #[test]
    fn test_message_header_type0() {
        let header = ChunkMessageHeader {
            timestamp: 1000,
            message_length: 500,
            message_type: MessageType::Command,
            message_stream_id: Some(12345),
            extended: false,
        };

        let bytes = header.serialize(ChunkType::Type0);
        assert_eq!(bytes.len(), 11);

        let parsed = ChunkMessageHeader::parse(&mut bytes.as_slice(), ChunkType::Type0).unwrap();
        assert_eq!(parsed.timestamp, 1000);
        assert_eq!(parsed.message_length, 500);
        assert_eq!(parsed.message_type, MessageType::Command);
        assert_eq!(parsed.message_stream_id, Some(12345));
    }

    #[test]
    fn test_message_header_type1() {
        let header = ChunkMessageHeader {
            timestamp: 100,
            message_length: 200,
            message_type: MessageType::Data,
            message_stream_id: None,
            extended: false,
        };

        let bytes = header.serialize(ChunkType::Type1);
        assert_eq!(bytes.len(), 7);

        let parsed = ChunkMessageHeader::parse(&mut bytes.as_slice(), ChunkType::Type1).unwrap();
        assert_eq!(parsed.timestamp, 100);
        assert_eq!(parsed.message_length, 200);
        assert_eq!(parsed.message_type, MessageType::Data);
    }

    #[test]
    fn test_message_header_type2() {
        let header = ChunkMessageHeader {
            timestamp: 50,
            message_length: 0,
            message_type: MessageType::Command,
            message_stream_id: None,
            extended: false,
        };

        let bytes = header.serialize(ChunkType::Type2);
        assert_eq!(bytes.len(), 3);

        let parsed = ChunkMessageHeader::parse(&mut bytes.as_slice(), ChunkType::Type2).unwrap();
        assert_eq!(parsed.timestamp, 50);
    }

    #[test]
    fn test_chunk_parser_single_chunk() {
        // Construct a single chunk message
        let mut data = Vec::new();

        // Basic header: Type 0, CS ID 3
        data.push(0x03);

        // Message header: timestamp=100, length=50, type=Command, stream_id=1
        let ts = 100u32.to_be_bytes();
        data.extend_from_slice(&ts[1..4]); // 3 bytes
        let len = 50u32.to_be_bytes();
        data.extend_from_slice(&len[1..4]); // 3 bytes
        data.push(MessageType::Command.to_u8()); // 1 byte
        data.extend_from_slice(&1u32.to_le_bytes()); // 4 bytes stream ID

        // Chunk data (50 bytes)
        data.extend_from_slice(&vec![0xAA; 50]);

        let mut parser = ChunkStreamParser::new();
        let mut reader = data.as_slice();

        let result = parser.parse_chunk(&mut reader).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 50);
    }

    #[test]
    fn test_chunk_parser_multiple_chunks() {
        // Construct a message split into 2 chunks (default chunk size 128)
        let mut data = Vec::new();

        // Chunk 1: Type 0 header + 128 bytes data
        data.push(0x03); // Basic header
        let ts = 100u32.to_be_bytes();
        data.extend_from_slice(&ts[1..4]);
        let len = 200u32.to_be_bytes();
        data.extend_from_slice(&len[1..4]);
        data.push(MessageType::Command.to_u8());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&vec![0xAA; 128]);

        // Chunk 2: Type 3 continuation + 72 bytes data
        data.push(0xC3); // Basic header: Type 3, CS ID 3
        data.extend_from_slice(&vec![0xBB; 72]);

        let mut parser = ChunkStreamParser::new();
        let mut reader = data.as_slice();

        // First chunk - not complete
        let result1 = parser.parse_chunk(&mut reader).unwrap();
        assert!(result1.is_none());

        // Second chunk - complete
        let result2 = parser.parse_chunk(&mut reader).unwrap();
        assert!(result2.is_some());
        let message = result2.unwrap();
        assert_eq!(message.len(), 200);
        assert_eq!(&message[0..128], &vec![0xAA; 128][..]);
        assert_eq!(&message[128..200], &vec![0xBB; 72][..]);
    }

    #[test]
    fn test_set_chunk_size() {
        let mut parser = ChunkStreamParser::new();
        assert_eq!(parser.chunk_size, 128);

        // Set chunk size to 4096
        let data = 4096u32.to_be_bytes();
        parser.handle_set_chunk_size(&data).unwrap();
        assert_eq!(parser.chunk_size, 4096);
    }

    #[test]
    fn test_window_ack_size() {
        let mut parser = ChunkStreamParser::new();

        // Set window ack size to 2500000
        let data = 2500000u32.to_be_bytes();
        parser.handle_window_ack_size(&data).unwrap();
        // Just verify it doesn't error
    }
}
