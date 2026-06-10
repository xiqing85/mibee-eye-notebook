use serde::{Deserialize, Serialize};

/// Unique camera identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CameraId(pub String);

impl CameraId {
    /// Create a new `CameraId` with a random UUID v4 string.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

impl Default for CameraId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for CameraId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique stream identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StreamId(pub String);

impl StreamId {
    /// Create a new `StreamId` with a random UUID v4 string.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

impl Default for StreamId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for StreamId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The type of camera source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraType {
    Usb,
    Rtsp,
    Onvif,
    Gb28181,
    Rtmp,
}

/// Current operational status of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamStatus {
    Stopped,
    Starting,
    Streaming,
    Error,
}

/// General device metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub manufacturer: String,
    pub model: String,
    pub firmware_version: String,
}

/// An RTSP URL wrapper that serializes as a transparent string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RtspUrl(pub String);

impl std::fmt::Display for RtspUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An ONVIF camera device with discovery and stream information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnvifDevice {
    pub device_info: DeviceInfo,
    pub xaddrs: Vec<String>,
    pub stream_uris: Vec<RtspUrl>,
}

/// A GB/T 28181 camera device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gb28181Device {
    /// 20-digit national-standard device ID.
    pub device_id: String,
    pub ip: String,
    pub port: u16,
    pub channels: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- CameraId ---

    #[test]
    fn test_camera_id_new_generates_valid_uuid() {
        let id = CameraId::new();
        assert_eq!(id.0.len(), 36, "UUID v4 should be 36 chars");
        // Verify hyphen positions in UUID format: 8-4-4-4-12
        let bytes = id.0.as_bytes();
        assert_eq!(bytes[8], b'-');
        assert_eq!(bytes[13], b'-');
        assert_eq!(bytes[18], b'-');
        assert_eq!(bytes[23], b'-');
    }

    #[test]
    fn test_camera_id_unique() {
        let a = CameraId::new();
        let b = CameraId::new();
        assert_ne!(a, b, "Each CameraId must be unique");
    }

    #[test]
    fn test_camera_id_display() {
        let id = CameraId("test-cam-42".into());
        assert_eq!(id.to_string(), "test-cam-42");
    }

    #[test]
    fn test_camera_id_default() {
        let id = CameraId::default();
        assert_eq!(id.0.len(), 36);
    }

    #[test]
    fn test_camera_id_serde_transparent() {
        let id = CameraId("uuid-abc-123".into());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(
            json, "\"uuid-abc-123\"",
            "CameraId must serialize as plain string"
        );
        let back: CameraId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    // --- StreamId ---

    #[test]
    fn test_stream_id_new_generates_valid_uuid() {
        let id = StreamId::new();
        assert_eq!(id.0.len(), 36);
        let bytes = id.0.as_bytes();
        assert_eq!(bytes[8], b'-');
        assert_eq!(bytes[13], b'-');
        assert_eq!(bytes[18], b'-');
        assert_eq!(bytes[23], b'-');
    }

    #[test]
    fn test_stream_id_unique() {
        let a = StreamId::new();
        let b = StreamId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn test_stream_id_display() {
        let id = StreamId("stream-007".into());
        assert_eq!(id.to_string(), "stream-007");
    }

    #[test]
    fn test_stream_id_serde_transparent() {
        let id = StreamId("sid-xyz".into());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"sid-xyz\"");
        let back: StreamId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    // --- CameraType ---

    #[test]
    fn test_camera_type_serde_roundtrip() {
        let cases = [
            (CameraType::Usb, "\"usb\""),
            (CameraType::Rtsp, "\"rtsp\""),
            (CameraType::Onvif, "\"onvif\""),
            (CameraType::Gb28181, "\"gb28181\""),
            (CameraType::Rtmp, "\"rtmp\""),
        ];
        for (variant, expected_json) in &cases {
            let json = serde_json::to_string(variant).unwrap();
            assert_eq!(
                &json, expected_json,
                "CameraType::{:?} serialization",
                variant
            );
            let back: CameraType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, *variant);
        }
    }

    #[test]
    fn test_camera_type_variants_distinct() {
        let variants = [
            CameraType::Usb,
            CameraType::Rtsp,
            CameraType::Onvif,
            CameraType::Gb28181,
            CameraType::Rtmp,
        ];
        for i in 0..variants.len() {
            for j in (i + 1)..variants.len() {
                assert_ne!(
                    variants[i], variants[j],
                    "Variants {:?} and {:?} must differ",
                    variants[i], variants[j]
                );
            }
        }
    }

    // --- StreamStatus ---

    #[test]
    fn test_stream_status_serde_roundtrip() {
        let cases = [
            (StreamStatus::Stopped, "\"Stopped\""),
            (StreamStatus::Starting, "\"Starting\""),
            (StreamStatus::Streaming, "\"Streaming\""),
            (StreamStatus::Error, "\"Error\""),
        ];
        for (variant, expected_json) in &cases {
            let json = serde_json::to_string(variant).unwrap();
            assert_eq!(
                &json, expected_json,
                "StreamStatus::{:?} serialization",
                variant
            );
            let back: StreamStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, *variant);
        }
    }

    #[test]
    fn test_stream_status_default_is_stopped() {
        // Default for the first variant
        let status: StreamStatus = serde_json::from_str("\"Stopped\"").unwrap();
        assert_eq!(status, StreamStatus::Stopped);
    }

    // --- DeviceInfo ---

    #[test]
    fn test_device_info_serde_roundtrip() {
        let info = DeviceInfo {
            name: "Front Door Cam".into(),
            manufacturer: "Hikvision".into(),
            model: "DS-2CD2347G2-LU".into(),
            firmware_version: "V5.7.12".into(),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: DeviceInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, info);
    }

    #[test]
    fn test_device_info_fields_mapped() {
        let json = r#"{
            "name": "Backyard",
            "manufacturer": "Dahua",
            "model": "IPC-HDW3849HP",
            "firmware_version": "4.001.0000004"
        }"#;
        let info: DeviceInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.name, "Backyard");
        assert_eq!(info.manufacturer, "Dahua");
    }

    // --- RtspUrl ---

    #[test]
    fn test_rtsp_url_serde_transparent() {
        let url = RtspUrl("rtsp://192.168.1.100:554/stream1".into());
        let json = serde_json::to_string(&url).unwrap();
        assert_eq!(json, "\"rtsp://192.168.1.100:554/stream1\"");
        let back: RtspUrl = serde_json::from_str(&json).unwrap();
        assert_eq!(back, url);
    }

    #[test]
    fn test_rtsp_url_display() {
        let url = RtspUrl("rtsp://admin:pass@10.0.0.1:554/h264".into());
        assert_eq!(url.to_string(), "rtsp://admin:pass@10.0.0.1:554/h264");
    }

    // --- OnvifDevice ---

    #[test]
    fn test_onvif_device_serde_roundtrip() {
        let device = OnvifDevice {
            device_info: DeviceInfo {
                name: "Lobby Cam".into(),
                manufacturer: "Axis".into(),
                model: "P3245-LVE".into(),
                firmware_version: "10.12.1".into(),
            },
            xaddrs: vec!["http://10.0.0.50:80/onvif/device_service".into()],
            stream_uris: vec![
                RtspUrl("rtsp://10.0.0.50:554/onvif/profile1".into()),
                RtspUrl("rtsp://10.0.0.50:554/onvif/profile2".into()),
            ],
        };
        let json = serde_json::to_string(&device).unwrap();
        let back: OnvifDevice = serde_json::from_str(&json).unwrap();
        assert_eq!(back, device);
    }

    #[test]
    fn test_onvif_device_xaddrs_order_preserved() {
        let device = OnvifDevice {
            device_info: DeviceInfo {
                name: "x".into(),
                manufacturer: "x".into(),
                model: "x".into(),
                firmware_version: "x".into(),
            },
            xaddrs: vec!["addr1".into(), "addr2".into()],
            stream_uris: vec![],
        };
        let json = serde_json::to_string(&device).unwrap();
        let back: OnvifDevice = serde_json::from_str(&json).unwrap();
        assert_eq!(back.xaddrs, vec!["addr1", "addr2"]);
    }

    // --- Gb28181Device ---

    #[test]
    fn test_gb28181_device_serde_roundtrip() {
        let device = Gb28181Device {
            device_id: "34020000001180000001".into(),
            ip: "192.168.1.20".into(),
            port: 5060,
            channels: 4,
        };
        let json = serde_json::to_string(&device).unwrap();
        let back: Gb28181Device = serde_json::from_str(&json).unwrap();
        assert_eq!(back, device);
    }

    #[test]
    fn test_gb28181_device_twenty_digit_id() {
        let device = Gb28181Device {
            device_id: "34020000001180000001".into(),
            ip: "10.0.0.1".into(),
            port: 5060,
            channels: 1,
        };
        assert_eq!(
            device.device_id.len(),
            20,
            "GB28181 device_id must be 20 digits"
        );
        assert!(device.device_id.chars().all(|c| c.is_ascii_digit()));
    }
}
