//! Protocol configuration types owned by the web crate.
//!
//! These structs are used both for TOML config loading (via re-export from
//! the root crate's `config` module) and for JSON Schema generation in the
//! protocol config REST API (`routes/protocols.rs`). Keeping them here avoids
//! a circular dependency: the web crate needs the types for schema generation,
//! and the root crate needs them for `AppConfig`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ONVIF Device
// ---------------------------------------------------------------------------

/// ONVIF device endpoint configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OnvifConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_onvif_device_name")]
    pub device_name: String,
    #[serde(default = "default_onvif_manufacturer")]
    pub manufacturer: String,
    #[serde(default = "default_onvif_model")]
    pub model: String,
    #[serde(default = "default_onvif_serial")]
    pub serial: String,
    #[serde(default = "default_onvif_firmware")]
    pub firmware_version: String,
}

fn default_onvif_device_name() -> String {
    "mibee-rec".into()
}
fn default_onvif_manufacturer() -> String {
    "MiBee".into()
}
fn default_onvif_model() -> String {
    "Rec-01".into()
}
fn default_onvif_serial() -> String {
    "NC00000001".into()
}
fn default_onvif_firmware() -> String {
    "1.0.0".into()
}

impl Default for OnvifConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            device_name: "mibee-rec".into(),
            manufacturer: "MiBee".into(),
            model: "Rec-01".into(),
            serial: "NC00000001".into(),
            firmware_version: "1.0.0".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// GB/T 28181 Device
// ---------------------------------------------------------------------------

/// GB28181 device registration configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Gb28181Config {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_gb28181_sip_address")]
    pub platform_sip_address: String,
    #[serde(default = "default_gb28181_sip_port")]
    pub platform_sip_port: u16,
    #[serde(default = "default_gb28181_device_id")]
    pub device_id: String,
    #[serde(default = "default_gb28181_username")]
    pub username: String,
    #[serde(default = "default_gb28181_password")]
    pub password: String,
    #[serde(default = "default_gb28181_sip_domain")]
    pub sip_domain: String,
    #[serde(default = "default_gb28181_register_interval")]
    pub register_interval_secs: u64,
}

fn default_gb28181_sip_address() -> String {
    "192.168.1.100".into()
}
fn default_gb28181_sip_port() -> u16 {
    5060
}
fn default_gb28181_device_id() -> String {
    "34020000002000000001".into()
}
fn default_gb28181_username() -> String {
    String::new()
}
fn default_gb28181_password() -> String {
    String::new()
}
fn default_gb28181_sip_domain() -> String {
    "3402000000".into()
}
fn default_gb28181_register_interval() -> u64 {
    60
}

impl Default for Gb28181Config {
    fn default() -> Self {
        Self {
            enabled: false,
            platform_sip_address: "192.168.1.100".into(),
            platform_sip_port: 5060,
            device_id: "34020000002000000001".into(),
            username: String::new(),
            password: String::new(),
            sip_domain: "3402000000".into(),
            register_interval_secs: 60,
        }
    }
}

// ---------------------------------------------------------------------------
// RTMP Push Client
// ---------------------------------------------------------------------------

/// RTMP push client configuration (pushes local stream to external ingest).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RtmpPushConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_rtmp_push_url")]
    pub push_url: String,
    #[serde(default = "default_rtmp_app_name")]
    pub app_name: String,
    #[serde(default = "default_rtmp_stream_name")]
    pub stream_name: String,
    #[serde(default = "default_rtmp_reconnect_interval")]
    pub reconnect_interval_secs: u64,
    #[serde(default = "default_rtmp_max_reconnect")]
    pub max_reconnect_attempts: u32,
}

fn default_rtmp_push_url() -> String {
    "rtmp://192.168.1.100:1935/live".into()
}
fn default_rtmp_app_name() -> String {
    "live".into()
}
fn default_rtmp_stream_name() -> String {
    "stream1".into()
}
fn default_rtmp_reconnect_interval() -> u64 {
    5
}
fn default_rtmp_max_reconnect() -> u32 {
    10
}

impl Default for RtmpPushConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            push_url: "rtmp://192.168.1.100:1935/live".into(),
            app_name: "live".into(),
            stream_name: "stream1".into(),
            reconnect_interval_secs: 5,
            max_reconnect_attempts: 10,
        }
    }
}

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

/// Local recording configuration.
///
/// When enabled, captured H.264 frames are muxed into rolling MP4 segments
/// in `path`. Oldest segments are auto-pruned when total size exceeds
/// `max_capacity_mb`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RecordingConfig {
    /// Master enable toggle. Individual streams can opt out via Web UI.
    #[serde(default)]
    pub enabled: bool,

    /// Directory where MP4 segment files are written. Must be writable.
    #[serde(default = "default_recording_path")]
    pub path: String,

    /// MP4 segment duration in seconds. Each file covers this much video.
    #[serde(default = "default_segment_duration_secs")]
    pub segment_duration_secs: u64,

    /// Max total capacity in megabytes. 0 = unlimited (no pruning).
    #[serde(default = "default_max_capacity_mb")]
    pub max_capacity_mb: u64,
}

fn default_recording_path() -> String {
    "./recordings".into()
}

fn default_segment_duration_secs() -> u64 {
    900 // 15 minutes
}

fn default_max_capacity_mb() -> u64 {
    10_240 // 10 GB
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: default_recording_path(),
            segment_duration_secs: default_segment_duration_secs(),
            max_capacity_mb: default_max_capacity_mb(),
        }
    }
}
