use serde::{Deserialize, Serialize};
use std::path::Path;

// ---------------------------------------------------------------------------
// Web
// ---------------------------------------------------------------------------

/// Web UI server configuration (TLS host and port).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebConfig {
    #[serde(default = "default_web_port")]
    pub port: u16,

    #[serde(default = "default_web_host")]
    pub host: String,
}

fn default_web_port() -> u16 {
    8443
}
fn default_web_host() -> String {
    "0.0.0.0".into()
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            port: 8443,
            host: "0.0.0.0".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// RTSP
// ---------------------------------------------------------------------------

/// RTSP server configuration (listening port).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RtspConfig {
    #[serde(default = "default_rtsp_port")]
    pub server_port: u16,
}

fn default_rtsp_port() -> u16 {
    8554
}

impl Default for RtspConfig {
    fn default() -> Self {
        Self { server_port: 8554 }
    }
}

// ---------------------------------------------------------------------------
// RTMP
// ---------------------------------------------------------------------------

/// RTMP ingest server configuration (listening port).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RtmpConfig {
    #[serde(default = "default_rtmp_port")]
    pub ingest_port: u16,
}

fn default_rtmp_port() -> u16 {
    1935
}

impl Default for RtmpConfig {
    fn default() -> Self {
        Self { ingest_port: 1935 }
    }
}
// ---------------------------------------------------------------------------
// ONVIF Device
// ---------------------------------------------------------------------------

/// ONVIF device endpoint configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    "notebook-cam".into()
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
            device_name: "notebook-cam".into(),
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
// Capture
// ---------------------------------------------------------------------------

/// Local video/audio capture device configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureConfig {
    #[serde(default = "default_video_device")]
    pub video_device: String,

    #[serde(default = "default_audio_device")]
    pub audio_device: String,
}

fn default_video_device() -> String {
    "/dev/video0".into()
}
fn default_audio_device() -> String {
    "default".into()
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            video_device: "/dev/video0".into(),
            audio_device: "default".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Security
// ---------------------------------------------------------------------------

/// Authentication rate-limiting configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecurityConfig {
    #[serde(default = "default_rate_limit_max")]
    pub rate_limit_max: usize,

    #[serde(default = "default_rate_limit_window")]
    pub rate_limit_window_secs: u64,
}

fn default_rate_limit_max() -> usize {
    20
}
fn default_rate_limit_window() -> u64 {
    60
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            rate_limit_max: 20,
            rate_limit_window_secs: 60,
        }
    }
}

// ---------------------------------------------------------------------------
// Observability
// ---------------------------------------------------------------------------

/// OpenTelemetry tracing and log level configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    #[serde(default = "default_otel_endpoint")]
    pub otel_endpoint: String,

    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_otel_endpoint() -> String {
    "http://localhost:4317".into()
}
fn default_log_level() -> String {
    "info".into()
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            otel_endpoint: "http://localhost:4317".into(),
            log_level: "info".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// AppConfig — top-level configuration
// ---------------------------------------------------------------------------

/// Top-level application configuration loaded from `config.toml`.
///
/// Each sub-section has sensible defaults; only the fields that differ from
/// defaults need to be specified in the TOML file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub web: WebConfig,

    #[serde(default)]
    pub rtsp: RtspConfig,

    #[serde(default)]
    pub rtmp: RtmpConfig,

    #[serde(default)]
    pub capture: CaptureConfig,

    #[serde(default)]
    pub security: SecurityConfig,

    #[serde(default)]
    pub observability: ObservabilityConfig,

    #[serde(default)]
    pub onvif: OnvifConfig,

    #[serde(default)]
    pub gb28181: Gb28181Config,

    #[serde(default)]
    pub rtmp_push: RtmpPushConfig,
}
impl AppConfig {
    /// Load configuration from a TOML file.
    ///
    /// Reads the file at `path`, parses it as TOML, and returns the deserialized
    /// `AppConfig`. Missing sections/fields fall back to their respective defaults.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: AppConfig = toml::from_str(&contents)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Individual default tests ---

    #[test]
    fn test_web_config_default() {
        let cfg = WebConfig::default();
        assert_eq!(cfg.port, 8443);
        assert_eq!(cfg.host, "0.0.0.0");
    }

    #[test]
    fn test_rtsp_config_default() {
        let cfg = RtspConfig::default();
        assert_eq!(cfg.server_port, 8554);
    }

    #[test]
    fn test_rtmp_config_default() {
        let cfg = RtmpConfig::default();
        assert_eq!(cfg.ingest_port, 1935);
    }

    #[test]
    fn test_capture_config_default() {
        let cfg = CaptureConfig::default();
        assert_eq!(cfg.video_device, "/dev/video0");
        assert_eq!(cfg.audio_device, "default");
    }

    #[test]
    fn test_security_config_default() {
        let cfg = SecurityConfig::default();
        assert_eq!(cfg.rate_limit_max, 20);
        assert_eq!(cfg.rate_limit_window_secs, 60);
    }

    #[test]
    fn test_observability_config_default() {
        let cfg = ObservabilityConfig::default();
        assert_eq!(cfg.otel_endpoint, "http://localhost:4317");
        assert_eq!(cfg.log_level, "info");
    }

// --- New protocol config defaults ---

#[test]
fn test_onvif_config_default() {
    let cfg = OnvifConfig::default();
    assert!(!cfg.enabled, "ONVIF must default to disabled");
    assert_eq!(cfg.device_name, "notebook-cam");
    assert_eq!(cfg.manufacturer, "MiBee");
    assert_eq!(cfg.model, "Rec-01");
    assert_eq!(cfg.serial, "NC00000001");
    assert_eq!(cfg.firmware_version, "1.0.0");
}

#[test]
fn test_gb28181_config_default() {
    let cfg = Gb28181Config::default();
    assert!(!cfg.enabled, "GB28181 must default to disabled");
    assert_eq!(cfg.platform_sip_address, "192.168.1.100");
    assert_eq!(cfg.platform_sip_port, 5060);
    assert_eq!(cfg.device_id, "34020000002000000001");
    assert_eq!(cfg.username, "");
    assert_eq!(cfg.password, "");
    assert_eq!(cfg.sip_domain, "3402000000");
    assert_eq!(cfg.register_interval_secs, 60);
}

#[test]
fn test_rtmp_push_config_default() {
    let cfg = RtmpPushConfig::default();
    assert!(!cfg.enabled, "RTMP push must default to disabled");
    assert_eq!(cfg.push_url, "rtmp://192.168.1.100:1935/live");
    assert_eq!(cfg.app_name, "live");
    assert_eq!(cfg.stream_name, "stream1");
    assert_eq!(cfg.reconnect_interval_secs, 5);
    assert_eq!(cfg.max_reconnect_attempts, 10);
}

    // --- AppConfig default ---

    #[test]
    fn test_app_config_default_aggregates() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.web.port, 8443);
        assert_eq!(cfg.web.host, "0.0.0.0");
        assert_eq!(cfg.rtsp.server_port, 8554);
        assert_eq!(cfg.rtmp.ingest_port, 1935);
        assert_eq!(cfg.capture.video_device, "/dev/video0");
        assert_eq!(cfg.capture.audio_device, "default");
        assert_eq!(cfg.security.rate_limit_max, 20);
        assert_eq!(cfg.security.rate_limit_window_secs, 60);
        assert_eq!(cfg.observability.otel_endpoint, "http://localhost:4317");
        assert_eq!(cfg.observability.log_level, "info");
        assert!(!cfg.onvif.enabled);
        assert_eq!(cfg.onvif.device_name, "notebook-cam");
        assert!(!cfg.gb28181.enabled);
        assert_eq!(cfg.gb28181.platform_sip_address, "192.168.1.100");
        assert!(!cfg.rtmp_push.enabled);
        assert_eq!(cfg.rtmp_push.push_url, "rtmp://192.168.1.100:1935/live");
    }

    // --- Serde round-trip ---

    #[test]
    fn test_app_config_toml_roundtrip() {
        let cfg = AppConfig::default();
        let toml_str = toml::to_string(&cfg).expect("serialize to TOML");
        let back: AppConfig = toml::from_str(&toml_str).expect("deserialize from TOML");
        assert_eq!(back, cfg);
    }

    #[test]
    fn test_app_config_json_roundtrip() {
        let cfg = AppConfig::default();
        let json = serde_json::to_string(&cfg).expect("serialize to JSON");
        let back: AppConfig = serde_json::from_str(&json).expect("deserialize from JSON");
        assert_eq!(back, cfg);
    }

    // --- Partial config deserialization (fallback to defaults) ---

    #[test]
    fn test_app_config_partial_toml_uses_defaults() {
        let toml_str = "\
[web]
port = 9090
";
        let cfg: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.web.port, 9090);
        // host should fall back to default
        assert_eq!(cfg.web.host, "0.0.0.0");
        // rtsp should fall back to default
        assert_eq!(cfg.rtsp.server_port, 8554);
    }

    // --- AppConfig::load ---

    #[test]
    fn test_app_config_load_roundtrip() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_nb_cam_config.toml");
        // Clean up if left over from a previous run
        let _ = std::fs::remove_file(&path);

        let cfg = AppConfig {
            web: WebConfig {
                port: 9090,
                host: "127.0.0.1".into(),
            },
            ..AppConfig::default()
        };
        let toml_str = toml::to_string(&cfg).unwrap();
        std::fs::write(&path, &toml_str).unwrap();

        let loaded = AppConfig::load(&path).unwrap();
        assert_eq!(loaded.web.port, 9090);
        assert_eq!(loaded.web.host, "127.0.0.1");
        // Non-overridden fields keep defaults
        assert_eq!(loaded.rtsp.server_port, 8554);
        assert_eq!(loaded.capture.video_device, "/dev/video0");

        // Cleanup
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_app_config_load_missing_file_returns_error() {
        let path = Path::new("/tmp/nonexistent_cfg_xyz123.toml");
        let result = AppConfig::load(path);
        assert!(result.is_err(), "Loading a nonexistent file must fail");
    }

    #[test]
    fn test_app_config_load_invalid_toml_returns_error() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_nb_cam_invalid.toml");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "[[[invalid toml").unwrap();
        let result = AppConfig::load(&path);
        assert!(result.is_err(), "Invalid TOML must fail");
        std::fs::remove_file(&path).ok();
    }
}
