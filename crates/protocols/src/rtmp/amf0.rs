//! AMF0 (Action Message Format 0) parsing and serialization
//!
//! AMF0 supports the following types:
//! - Number (0x00): 8-byte IEEE-754 double
//! - Boolean (0x01): 1 byte (0=false, 1=true)
//! - String (0x02): 2-byte length + UTF-8 bytes
//! - Object (0x03): key-value pairs terminated by 0x00 0x00 0x09
//! - Null (0x05): no data
//! - Undefined (0x06): no data
//! - ECMA Array (0x08): 4-byte count + key-value pairs
//! - Object End (0x09): terminator for objects

use anyhow::{Context, Result, bail};

/// AMF0 type markers
const AMF0_NUMBER: u8 = 0x00;
const AMF0_BOOLEAN: u8 = 0x01;
const AMF0_STRING: u8 = 0x02;
const AMF0_OBJECT: u8 = 0x03;
const AMF0_NULL: u8 = 0x05;
const AMF0_UNDEFINED: u8 = 0x06;
const AMF0_ECMA_ARRAY: u8 = 0x08;
const AMF0_OBJECT_END: u8 = 0x09;
const AMF0_STRICT_ARRAY: u8 = 0x0A;
const AMF0_DATE: u8 = 0x0B;
const AMF0_LONG_STRING: u8 = 0x0C;
const AMF0_XML: u8 = 0x0F;
const AMF0_TYPED_OBJECT: u8 = 0x10;
const AMF0_AMF3: u8 = 0x11;

/// AMF0 value representation
#[derive(Debug, Clone, PartialEq)]
pub enum Amf0Value {
    Number(f64),
    Boolean(bool),
    String(String),
    Object(Vec<(String, Amf0Value)>),
    Null,
    Undefined,
    EcmaArray(u32, Vec<(String, Amf0Value)>), // count, entries
}

impl Amf0Value {
    /// Parse a single AMF0 value from bytes
    pub fn parse(data: &[u8]) -> Result<(Self, usize)> {
        if data.is_empty() {
            bail!("Cannot parse AMF0 value from empty data");
        }

        let marker = data[0];
        let offset = 1;

        match marker {
            AMF0_NUMBER => {
                if data.len() < offset + 8 {
                    bail!("Insufficient data for AMF0 number");
                }
                let bytes = [
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                    data[offset + 4],
                    data[offset + 5],
                    data[offset + 6],
                    data[offset + 7],
                ];
                let value = f64::from_be_bytes(bytes);
                Ok((Amf0Value::Number(value), offset + 8))
            }
            AMF0_BOOLEAN => {
                if data.len() < offset + 1 {
                    bail!("Insufficient data for AMF0 boolean");
                }
                let value = data[offset] != 0;
                Ok((Amf0Value::Boolean(value), offset + 1))
            }
            AMF0_STRING => {
                if data.len() < offset + 2 {
                    bail!("Insufficient data for AMF0 string length");
                }
                let len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
                if data.len() < offset + 2 + len {
                    bail!("Insufficient data for AMF0 string value");
                }
                let value = String::from_utf8(data[offset + 2..offset + 2 + len].to_vec())
                    .context("Invalid UTF-8 in AMF0 string")?;
                Ok((Amf0Value::String(value), offset + 2 + len))
            }
            AMF0_OBJECT => Self::parse_object(data, offset),
            AMF0_NULL => Ok((Amf0Value::Null, offset)),
            AMF0_UNDEFINED => Ok((Amf0Value::Undefined, offset)),
            AMF0_ECMA_ARRAY => {
                if data.len() < offset + 4 {
                    bail!("Insufficient data for AMF0 ECMA array count");
                }
                let count = u32::from_be_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ]);
                let (entries, consumed) = Self::parse_object_entries(data, offset + 4)?;
                Ok((Amf0Value::EcmaArray(count, entries), offset + 4 + consumed))
            }
            AMF0_STRICT_ARRAY => Self::parse_strict_array(data, offset),
            AMF0_DATE => Self::parse_date(data, offset),
            AMF0_LONG_STRING => Self::parse_long_string(data, offset),
            AMF0_XML => Self::parse_xml(data, offset),
            AMF0_TYPED_OBJECT => Self::parse_typed_object(data, offset),
            AMF0_AMF3 => bail!("AMF3 encoding not supported"),
            _ => bail!("Unknown AMF0 type marker: 0x{:02x}", marker),
        }
    }

    fn parse_object(data: &[u8], offset: usize) -> Result<(Self, usize)> {
        let (entries, consumed) = Self::parse_object_entries(data, offset)?;

        // parse_object_entries already validates and includes the end marker
        Ok((Amf0Value::Object(entries), offset + consumed))
    }

    fn parse_object_entries(
        data: &[u8],
        offset: usize,
    ) -> Result<(Vec<(String, Amf0Value)>, usize)> {
        let mut entries = Vec::new();
        let mut pos = offset;

        loop {
            // Read key (string without type marker)
            if data.len() < pos + 2 {
                bail!("Insufficient data for AMF0 object key length");
            }

            let key_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
            pos += 2;

            if key_len == 0 {
                // Empty string signals object end
                if data.len() < pos + 1 {
                    bail!("Insufficient data for AMF0 object end marker");
                }
                if data[pos] == AMF0_OBJECT_END {
                    pos += 1;
                    break;
                }
            }

            if data.len() < pos + key_len {
                bail!("Insufficient data for AMF0 object key value");
            }

            let key = String::from_utf8(data[pos..pos + key_len].to_vec())
                .context("Invalid UTF-8 in AMF0 object key")?;
            pos += key_len;

            // Read value (with type marker)
            let (value, consumed) = Self::parse(&data[pos..])?;
            entries.push((key, value));
            pos += consumed;
        }

        Ok((entries, pos - offset))
    }

    fn parse_strict_array(data: &[u8], offset: usize) -> Result<(Self, usize)> {
        if data.len() < offset + 4 {
            bail!("Insufficient data for AMF0 strict array count");
        }

        let count = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        let mut pos = offset + 4;
        let mut entries = Vec::new();

        for _ in 0..count {
            let (value, consumed) = Self::parse(&data[pos..])?;
            entries.push((format!("{}", entries.len()), value));
            pos += consumed;
        }

        // Convert strict array to EcmaArray for simplicity
        Ok((Amf0Value::EcmaArray(count as u32, entries), pos))
    }

    fn parse_date(data: &[u8], offset: usize) -> Result<(Self, usize)> {
        if data.len() < offset + 10 {
            bail!("Insufficient data for AMF0 date");
        }

        let bytes = [
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ];
        let _ms = f64::from_be_bytes(bytes);
        let _tz = u16::from_be_bytes([data[offset + 8], data[offset + 9]]);

        // Convert to EcmaArray with metadata for simplicity
        let entries = vec![
            ("ms".to_string(), Amf0Value::Number(_ms)),
            ("tz".to_string(), Amf0Value::Number(_tz as f64)),
        ];
        Ok((Amf0Value::EcmaArray(2, entries), offset + 10))
    }

    fn parse_long_string(data: &[u8], offset: usize) -> Result<(Self, usize)> {
        if data.len() < offset + 4 {
            bail!("Insufficient data for AMF0 long string length");
        }

        let len = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        if data.len() < offset + 4 + len {
            bail!("Insufficient data for AMF0 long string value");
        }

        let value = String::from_utf8(data[offset + 4..offset + 4 + len].to_vec())
            .context("Invalid UTF-8 in AMF0 long string")?;
        Ok((Amf0Value::String(value), offset + 4 + len))
    }

    fn parse_xml(data: &[u8], offset: usize) -> Result<(Self, usize)> {
        // XML is encoded like a long string
        Self::parse_long_string(data, offset)
    }

    fn parse_typed_object(data: &[u8], offset: usize) -> Result<(Self, usize)> {
        // Read class name (string without type marker)
        if data.len() < offset + 2 {
            bail!("Insufficient data for AMF0 typed object class name length");
        }

        let class_name_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        let pos = offset + 2;

        if data.len() < pos + class_name_len {
            bail!("Insufficient data for AMF0 typed object class name");
        }

        let _class_name = String::from_utf8(data[pos..pos + class_name_len].to_vec())
            .context("Invalid UTF-8 in AMF0 typed object class name")?;
        let pos = pos + class_name_len;

        // Parse as regular object
        let (entries, consumed) = Self::parse_object_entries(data, pos)?;

        // Verify object end marker
        if data.len() < pos + consumed + 3 {
            bail!("Insufficient data for AMF0 typed object end marker");
        }
        if data[pos + consumed..pos + consumed + 3] != [0x00, 0x00, AMF0_OBJECT_END] {
            bail!("Invalid AMF0 typed object end marker");
        }

        Ok((Amf0Value::Object(entries), pos + consumed + 3))
    }

    /// Serialize AMF0 value to bytes
    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        match self {
            Amf0Value::Number(n) => {
                bytes.push(AMF0_NUMBER);
                bytes.extend_from_slice(&n.to_be_bytes());
            }
            Amf0Value::Boolean(b) => {
                bytes.push(AMF0_BOOLEAN);
                bytes.push(if *b { 1 } else { 0 });
            }
            Amf0Value::String(s) => {
                bytes.push(AMF0_STRING);
                let s_bytes = s.as_bytes();
                let len = (s_bytes.len() as u16).to_be_bytes();
                bytes.extend_from_slice(&len);
                bytes.extend_from_slice(s_bytes);
            }
            Amf0Value::Object(entries) => {
                bytes.push(AMF0_OBJECT);
                Self::serialize_object_entries(&mut bytes, entries);
                bytes.extend_from_slice(&[0x00, 0x00, AMF0_OBJECT_END]);
            }
            Amf0Value::Null => {
                bytes.push(AMF0_NULL);
            }
            Amf0Value::Undefined => {
                bytes.push(AMF0_UNDEFINED);
            }
            Amf0Value::EcmaArray(count, entries) => {
                bytes.push(AMF0_ECMA_ARRAY);
                bytes.extend_from_slice(&count.to_be_bytes());
                Self::serialize_object_entries(&mut bytes, entries);
                bytes.extend_from_slice(&[0x00, 0x00, AMF0_OBJECT_END]);
            }
        }

        bytes
    }

    fn serialize_object_entries(bytes: &mut Vec<u8>, entries: &[(String, Amf0Value)]) {
        for (key, value) in entries {
            // Key (string without type marker)
            let key_bytes = key.as_bytes();
            bytes.extend_from_slice(&(key_bytes.len() as u16).to_be_bytes());
            bytes.extend_from_slice(key_bytes);

            // Value (with type marker)
            bytes.extend_from_slice(&value.serialize());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_number() {
        let data = [AMF0_NUMBER, 0x40, 0x09, 0x21, 0xFB, 0x54, 0x44, 0x2D, 0x18]; // 3.141592653589793
        let (value, consumed) = Amf0Value::parse(&data).unwrap();

        assert_eq!(consumed, 9);
        match value {
            Amf0Value::Number(n) => {
                assert!((n - std::f64::consts::PI).abs() < 1e-10);
            }
            _ => panic!("Expected number"),
        }
    }

    #[test]
    fn test_serialize_number() {
        let value = Amf0Value::Number(42.5);
        let bytes = value.serialize();

        assert_eq!(bytes.len(), 9);
        assert_eq!(bytes[0], AMF0_NUMBER);
        let parsed = f64::from_be_bytes([
            bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
        ]);
        assert!((parsed - 42.5).abs() < 1e-10);
    }

    #[test]
    fn test_parse_boolean() {
        let data_true = [AMF0_BOOLEAN, 0x01];
        let (value, consumed) = Amf0Value::parse(&data_true).unwrap();
        assert_eq!(consumed, 2);
        assert_eq!(value, Amf0Value::Boolean(true));

        let data_false = [AMF0_BOOLEAN, 0x00];
        let (value, consumed) = Amf0Value::parse(&data_false).unwrap();
        assert_eq!(consumed, 2);
        assert_eq!(value, Amf0Value::Boolean(false));
    }

    #[test]
    fn test_serialize_boolean() {
        let value_true = Amf0Value::Boolean(true);
        let bytes = value_true.serialize();
        assert_eq!(bytes, vec![AMF0_BOOLEAN, 0x01]);

        let value_false = Amf0Value::Boolean(false);
        let bytes = value_false.serialize();
        assert_eq!(bytes, vec![AMF0_BOOLEAN, 0x00]);
    }

    #[test]
    fn test_parse_string() {
        let mut data = Vec::new();
        data.push(AMF0_STRING);
        data.extend_from_slice(&5u16.to_be_bytes());
        data.extend_from_slice(b"hello");

        let (value, consumed) = Amf0Value::parse(&data).unwrap();
        assert_eq!(consumed, 8);
        assert_eq!(value, Amf0Value::String("hello".to_string()));
    }

    #[test]
    fn test_serialize_string() {
        let value = Amf0Value::String("world".to_string());
        let bytes = value.serialize();

        assert_eq!(bytes.len(), 8);
        assert_eq!(bytes[0], AMF0_STRING);
        assert_eq!(&bytes[1..3], &5u16.to_be_bytes()[..]);
        assert_eq!(&bytes[3..], b"world");
    }

    #[test]
    fn test_parse_null() {
        let data = [AMF0_NULL];
        let (value, consumed) = Amf0Value::parse(&data).unwrap();
        assert_eq!(consumed, 1);
        assert_eq!(value, Amf0Value::Null);
    }

    #[test]
    fn test_serialize_null() {
        let value = Amf0Value::Null;
        let bytes = value.serialize();
        assert_eq!(bytes, vec![AMF0_NULL]);
    }

    #[test]
    fn test_parse_undefined() {
        let data = [AMF0_UNDEFINED];
        let (value, consumed) = Amf0Value::parse(&data).unwrap();
        assert_eq!(consumed, 1);
        assert_eq!(value, Amf0Value::Undefined);
    }

    #[test]
    fn test_parse_object() {
        let mut data = Vec::new();
        data.push(AMF0_OBJECT);

        // Key "app", value "live"
        data.extend_from_slice(&3u16.to_be_bytes());
        data.extend_from_slice(b"app");
        data.push(AMF0_STRING);
        data.extend_from_slice(&4u16.to_be_bytes());
        data.extend_from_slice(b"live");

        // Object end
        data.extend_from_slice(&[0x00, 0x00, AMF0_OBJECT_END]);

        let (value, consumed) = Amf0Value::parse(&data).unwrap();
        assert_eq!(consumed, data.len());

        match value {
            Amf0Value::Object(entries) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].0, "app");
                assert_eq!(entries[0].1, Amf0Value::String("live".to_string()));
            }
            _ => panic!("Expected object"),
        }
    }

    #[test]
    fn test_serialize_object() {
        let value = Amf0Value::Object(vec![
            ("app".to_string(), Amf0Value::String("live".to_string())),
            (
                "type".to_string(),
                Amf0Value::String("nonprivate".to_string()),
            ),
        ]);

        let bytes = value.serialize();
        assert_eq!(bytes[0], AMF0_OBJECT);

        let (parsed, _) = Amf0Value::parse(&bytes).unwrap();
        assert_eq!(parsed, value);
    }

    #[test]
    fn test_parse_ecma_array() {
        let mut data = Vec::new();
        data.push(AMF0_ECMA_ARRAY);
        data.extend_from_slice(&2u32.to_be_bytes());

        // Key "width", value 1920
        data.extend_from_slice(&5u16.to_be_bytes());
        data.extend_from_slice(b"width");
        data.push(AMF0_NUMBER);
        data.extend_from_slice(&1920f64.to_be_bytes());

        // Key "height", value 1080
        data.extend_from_slice(&6u16.to_be_bytes());
        data.extend_from_slice(b"height");
        data.push(AMF0_NUMBER);
        data.extend_from_slice(&1080f64.to_be_bytes());

        // Object end
        data.extend_from_slice(&[0x00, 0x00, AMF0_OBJECT_END]);

        let (value, consumed) = Amf0Value::parse(&data).unwrap();
        assert_eq!(consumed, data.len());

        match value {
            Amf0Value::EcmaArray(count, entries) => {
                assert_eq!(count, 2);
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].0, "width");
                assert_eq!(entries[0].1, Amf0Value::Number(1920.0));
                assert_eq!(entries[1].0, "height");
                assert_eq!(entries[1].1, Amf0Value::Number(1080.0));
            }
            _ => panic!("Expected ECMA array"),
        }
    }

    #[test]
    fn test_serialize_ecma_array() {
        let value = Amf0Value::EcmaArray(
            2,
            vec![
                ("key1".to_string(), Amf0Value::Number(100.0)),
                ("key2".to_string(), Amf0Value::String("value2".to_string())),
            ],
        );

        let bytes = value.serialize();
        assert_eq!(bytes[0], AMF0_ECMA_ARRAY);

        let (parsed, _) = Amf0Value::parse(&bytes).unwrap();
        assert_eq!(parsed, value);
    }

    #[test]
    fn test_round_trip_number() {
        let original = Amf0Value::Number(-123.456);
        let bytes = original.serialize();
        let (parsed, _) = Amf0Value::parse(&bytes).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_round_trip_string() {
        let original = Amf0Value::String("Test String 🚀".to_string());
        let bytes = original.serialize();
        let (parsed, _) = Amf0Value::parse(&bytes).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_round_trip_complex_object() {
        let original = Amf0Value::Object(vec![
            ("name".to_string(), Amf0Value::String("test".to_string())),
            ("count".to_string(), Amf0Value::Number(42.0)),
            ("enabled".to_string(), Amf0Value::Boolean(true)),
            ("optional".to_string(), Amf0Value::Null),
        ]);

        let bytes = original.serialize();
        let (parsed, _) = Amf0Value::parse(&bytes).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_multiple_values_in_sequence() {
        let mut data = Vec::new();

        // Value 1: Number 1
        data.extend_from_slice(&Amf0Value::Number(1.0).serialize());

        // Value 2: String "hello"
        data.extend_from_slice(&Amf0Value::String("hello".to_string()).serialize());

        // Value 3: Boolean true
        data.extend_from_slice(&Amf0Value::Boolean(true).serialize());

        let mut pos = 0;

        let (v1, consumed) = Amf0Value::parse(&data[pos..]).unwrap();
        pos += consumed;
        assert_eq!(v1, Amf0Value::Number(1.0));

        let (v2, consumed) = Amf0Value::parse(&data[pos..]).unwrap();
        pos += consumed;
        assert_eq!(v2, Amf0Value::String("hello".to_string()));

        let (v3, consumed) = Amf0Value::parse(&data[pos..]).unwrap();
        pos += consumed;
        assert_eq!(v3, Amf0Value::Boolean(true));

        assert_eq!(pos, data.len());
    }
}
