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

    /// Advertised hostname/IP for URLs returned to clients.
    /// If None, auto-detected at startup via UDP socket.
    #[serde(default)]
    pub advertised_host: Option<String>,
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
            advertised_host: None,
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

    /// Optional remote log shipping (Loki-compatible HTTP endpoint).
    /// When None (default), logs go to stdout only.
    #[serde(default)]
    pub logs: Option<RemoteLogConfig>,
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
            logs: None,
        }
    }
}

/// Optional remote log shipping to a Loki-compatible HTTP endpoint.
///
/// When this section is present in config.toml, structured logs are shipped
/// to the configured endpoint in addition to stdout/stderr. The feature is
/// fail-open: if the endpoint is unreachable, a warning is logged and the
/// application continues with stdout-only logging.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteLogConfig {
    /// Loki HTTP endpoint URL (e.g. http://loki:3100).
    #[serde(default)]
    pub endpoint: String,
    /// Number of log entries to batch per flush.
    #[serde(default = "default_remote_log_batch_size")]
    pub batch_size: usize,
    /// Flush interval in seconds.
    #[serde(default = "default_remote_log_flush_interval")]
    pub flush_interval_secs: u64,
    /// Additional labels attached to every log stream.
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
}

fn default_remote_log_batch_size() -> usize {
    100
}
fn default_remote_log_flush_interval() -> u64 {
    5
}

impl Default for RemoteLogConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            batch_size: 100,
            flush_interval_secs: 5,
            labels: std::collections::HashMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Database
// ---------------------------------------------------------------------------

/// SQLite database configuration with XDG-compliant default path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatabaseConfig {
    #[serde(default = "default_database_path")]
    pub path: String,
}

fn default_database_path() -> String {
    // XDG default: ~/.local/share/mibee-rec/mibee_rec.db
    if let Some(data_dir) = dirs::data_dir() {
        data_dir
            .join("mibee-rec")
            .join("mibee_rec.db")
            .to_string_lossy()
            .to_string()
    } else {
        // Fallback to /tmp if XDG data dir is not available
        "/tmp/mibee-rec/mibee_rec.db".to_string()
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: default_database_path(),
        }
    }
}
// ---------------------------------------------------------------------------
// AppConfig — top-level configuration
// ---------------------------------------------------------------------------

// Top-level application configuration loaded from `config.toml`.

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

/// Local recording configuration.
///
/// When enabled, captured H.264 frames are muxed into rolling MP4 segments
/// in `path`. Oldest segments are auto-pruned when total size exceeds
/// `max_capacity_mb`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Each sub-section has sensible defaults; only the fields that differ from
/// defaults need to be specified in the TOML file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub web: WebConfig,

    #[serde(default)]
    pub rtsp: RtspConfig,
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

    #[serde(default)]
    pub recording: RecordingConfig,

    #[serde(default)]
    pub database: DatabaseConfig,
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

    /// Validate the configuration at startup.
    ///
    /// Checks:
    /// - Ports must be > 1024 (web.port, rtsp.server_port, gb28181.platform_sip_port)
    /// - Port conflicts (web.port must not equal rtsp.server_port)
    /// - security.rate_limit_max must be > 0
    /// - gb28181.register_interval_secs must be > 0 (if enabled)
    /// - rtmp_push.reconnect_interval_secs must be > 0 (if enabled)
    /// - rtmp_push.max_reconnect_attempts must be > 0 (if enabled)
    pub fn validate(&self) -> anyhow::Result<()> {
        // Web port
        if self.web.port <= 1024 {
            anyhow::bail!("web.port: must be > 1024, got {}", self.web.port);
        }
        // RTSP port
        if self.rtsp.server_port <= 1024 {
            anyhow::bail!(
                "rtsp.server_port: must be > 1024, got {}",
                self.rtsp.server_port
            );
        }
        // Port conflict
        if self.web.port == self.rtsp.server_port {
            anyhow::bail!(
                "web.port ({}) must not equal rtsp.server_port ({})",
                self.web.port,
                self.rtsp.server_port
            );
        }
        // GB28181 SIP port (if enabled)
        if self.gb28181.enabled && self.gb28181.platform_sip_port <= 1024 {
            anyhow::bail!(
                "gb28181.platform_sip_port: must be > 1024, got {}",
                self.gb28181.platform_sip_port
            );
        }
        // Rate limit
        if self.security.rate_limit_max == 0 {
            anyhow::bail!("security.rate_limit_max: must be > 0, got 0");
        }
        // GB28181 register interval
        if self.gb28181.enabled && self.gb28181.register_interval_secs == 0 {
            anyhow::bail!("gb28181.register_interval_secs: must be > 0, got 0");
        }
        // RTMP push reconnect interval
        if self.rtmp_push.enabled && self.rtmp_push.reconnect_interval_secs == 0 {
            anyhow::bail!("rtmp_push.reconnect_interval_secs: must be > 0, got 0");
        }
        // RTMP push max reconnect attempts
        if self.rtmp_push.enabled && self.rtmp_push.max_reconnect_attempts == 0 {
            anyhow::bail!("rtmp_push.max_reconnect_attempts: must be > 0, got 0");
        }
        Ok(())
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
        assert_eq!(cfg.device_name, "mibee-rec");
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
        assert_eq!(cfg.capture.video_device, "/dev/video0");
        assert_eq!(cfg.capture.audio_device, "default");
        assert_eq!(cfg.security.rate_limit_max, 20);
        assert_eq!(cfg.security.rate_limit_window_secs, 60);
        assert_eq!(cfg.observability.otel_endpoint, "http://localhost:4317");
        assert_eq!(cfg.observability.log_level, "info");
        assert!(!cfg.onvif.enabled);
        assert_eq!(cfg.onvif.device_name, "mibee-rec");
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
                advertised_host: None,
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

    // --- Edge case tests ---
    //
    // These verify that empty/missing sections fall back to defaults, that
    // unusual-but-valid values deserialize without panicking, and that
    // validation-adjacent edge cases are handled gracefully.

    #[test]
    fn test_empty_gb28181_section_uses_defaults() {
        let toml_str = "[gb28181]\n";
        let cfg: AppConfig = toml::from_str(toml_str).unwrap();
        assert!(!cfg.gb28181.enabled);
        assert_eq!(cfg.gb28181.platform_sip_address, "192.168.1.100");
        assert_eq!(cfg.gb28181.platform_sip_port, 5060);
        assert_eq!(cfg.gb28181.device_id, "34020000002000000001");
        assert_eq!(cfg.gb28181.register_interval_secs, 60);
    }

    #[test]
    fn test_empty_onvif_section_uses_defaults() {
        let toml_str = "[onvif]\n";
        let cfg: AppConfig = toml::from_str(toml_str).unwrap();
        assert!(!cfg.onvif.enabled);
        assert_eq!(cfg.onvif.device_name, "mibee-rec");
        assert_eq!(cfg.onvif.manufacturer, "MiBee");
        assert_eq!(cfg.onvif.model, "Rec-01");
        assert_eq!(cfg.onvif.firmware_version, "1.0.0");
    }

    #[test]
    fn test_empty_rtmp_push_section_uses_defaults() {
        let toml_str = "[rtmp_push]\n";
        let cfg: AppConfig = toml::from_str(toml_str).unwrap();
        assert!(!cfg.rtmp_push.enabled);
        assert_eq!(cfg.rtmp_push.push_url, "rtmp://192.168.1.100:1935/live");
        assert_eq!(cfg.rtmp_push.app_name, "live");
        assert_eq!(cfg.rtmp_push.stream_name, "stream1");
        assert_eq!(cfg.rtmp_push.reconnect_interval_secs, 5);
        assert_eq!(cfg.rtmp_push.max_reconnect_attempts, 10);
    }

    #[test]
    fn test_gb28181_device_id_empty() {
        let toml_str = r#"
[gb28181]
device_id = ""
"#;
        let cfg: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.gb28181.device_id, "");
        // Other fields keep defaults
        assert_eq!(cfg.gb28181.platform_sip_port, 5060);
        assert_eq!(cfg.gb28181.register_interval_secs, 60);
    }

    #[test]
    fn test_gb28181_device_id_short() {
        let toml_str = r#"
[gb28181]
device_id = "123"
"#;
        let cfg: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.gb28181.device_id, "123");
        assert_eq!(cfg.gb28181.platform_sip_port, 5060);
    }

    #[test]
    fn test_gb28181_device_id_19_chars() {
        let toml_str = r#"
[gb28181]
device_id = "3402000000200000000"
"#;
        let cfg: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.gb28181.device_id.len(), 19);
        assert_eq!(cfg.gb28181.platform_sip_port, 5060);
    }

    #[test]
    fn test_port_zero_edge_case() {
        let toml_str = r#"
[web]
port = 0
"#;
        let cfg: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.web.port, 0);
        // host falls back to default since not specified
        assert_eq!(cfg.web.host, "0.0.0.0");
    }

    #[test]
    fn test_rtmp_push_url_empty_keeps_other_defaults() {
        let toml_str = r#"
[rtmp_push]
push_url = ""
"#;
        let cfg: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.rtmp_push.push_url, "");
        // Other fields keep their own defaults
        assert_eq!(cfg.rtmp_push.app_name, "live");
        assert_eq!(cfg.rtmp_push.stream_name, "stream1");
        assert_eq!(cfg.rtmp_push.reconnect_interval_secs, 5);
        assert_eq!(cfg.rtmp_push.max_reconnect_attempts, 10);
    }

    #[test]
    fn test_all_empty_sections_use_defaults() {
        let toml_str = r#"
[web]
[rtsp]
[capture]
[security]
[observability]
[onvif]
[gb28181]
[rtmp_push]
"#;
        let cfg: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.web.port, 8443, "web.port");
        assert_eq!(cfg.web.host, "0.0.0.0", "web.host");
        assert_eq!(cfg.rtsp.server_port, 8554, "rtsp.server_port");
        assert_eq!(
            cfg.capture.video_device, "/dev/video0",
            "capture.video_device"
        );
        assert_eq!(cfg.capture.audio_device, "default", "capture.audio_device");
        assert_eq!(cfg.security.rate_limit_max, 20, "security.rate_limit_max");
        assert_eq!(
            cfg.security.rate_limit_window_secs, 60,
            "security.rate_limit_window_secs"
        );
        assert_eq!(
            cfg.observability.otel_endpoint, "http://localhost:4317",
            "observability.otel_endpoint"
        );
        assert_eq!(
            cfg.observability.log_level, "info",
            "observability.log_level"
        );
        assert!(!cfg.onvif.enabled);
        assert_eq!(cfg.onvif.device_name, "mibee-rec", "onvif.device_name");
        assert!(!cfg.gb28181.enabled);
        assert_eq!(
            cfg.gb28181.device_id, "34020000002000000001",
            "gb28181.device_id"
        );
        assert!(!cfg.rtmp_push.enabled);
        assert_eq!(
            cfg.rtmp_push.push_url, "rtmp://192.168.1.100:1935/live",
            "rtmp_push.push_url"
        );
    }

    #[test]
    fn test_all_protocols_config_loads() {
        let toml_str = r#"
[onvif]
enabled = true

[gb28181]
enabled = true

[rtmp_push]
enabled = false
"#;
        let cfg: AppConfig = toml::from_str(toml_str).expect("valid TOML with protocols enabled");
        assert!(cfg.onvif.enabled, "ONVIF must be enabled");
        assert!(cfg.gb28181.enabled, "GB28181 must be enabled");
        assert!(!cfg.rtmp_push.enabled, "RTMP push must be disabled");

        // Verify other fields load with defaults
        assert_eq!(cfg.onvif.device_name, "mibee-rec");
        assert_eq!(cfg.gb28181.platform_sip_address, "192.168.1.100");
        assert_eq!(cfg.gb28181.platform_sip_port, 5060);
        assert_eq!(cfg.rtmp_push.push_url, "rtmp://192.168.1.100:1935/live");

        // Verify no port conflicts between protocols
        // ONVIF WS-Discovery uses UDP 3702 (hardcoded in protocols/src/onvif.rs)
        // RTSP server uses config.server_port (default 8554)
        // Web UI uses config.port (default 8443)
        // GB28181 SIP uses config.platform_sip_port (default 5060)
        assert_ne!(
            3702u16, cfg.rtsp.server_port,
            "ONVIF port 3702 conflicts with RTSP"
        );
        assert_ne!(3702u16, cfg.web.port, "ONVIF port 3702 conflicts with Web");
        assert_ne!(
            cfg.rtsp.server_port, cfg.web.port,
            "RTSP port conflicts with Web"
        );
    }

    #[test]
    fn test_main_builds_with_all_protocols() {
        // Verify all protocol config types are constructable with enabled state
        // This ensures main.rs can build with protocol imports
        let onvif = OnvifConfig {
            enabled: true,
            ..OnvifConfig::default()
        };
        assert!(onvif.enabled);
        assert_eq!(onvif.device_name, "mibee-rec");

        let gb28181 = Gb28181Config {
            enabled: true,
            ..Gb28181Config::default()
        };
        assert!(gb28181.enabled);
        assert_eq!(gb28181.platform_sip_address, "192.168.1.100");

        let rtmp_push = RtmpPushConfig {
            enabled: false,
            ..RtmpPushConfig::default()
        };
        assert!(!rtmp_push.enabled);

        // Verify AppConfig can hold all protocol configs (compile check)
        let cfg = AppConfig {
            onvif: OnvifConfig {
                enabled: true,
                ..OnvifConfig::default()
            },
            gb28181: Gb28181Config {
                enabled: true,
                ..Gb28181Config::default()
            },
            rtmp_push: RtmpPushConfig {
                enabled: false,
                ..RtmpPushConfig::default()
            },
            ..AppConfig::default()
        };
        assert!(cfg.onvif.enabled);
        assert!(cfg.gb28181.enabled);
        assert!(!cfg.rtmp_push.enabled);
    }

    // --- Validation tests ---

    #[test]
    fn test_validate_valid_config_passes() {
        let cfg = AppConfig::default();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_validate_web_port_low_rejected() {
        let cfg = AppConfig {
            web: WebConfig {
                port: 80,
                host: "0.0.0.0".into(),
                advertised_host: None,
            },
            ..AppConfig::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("web.port"),
            "error should mention web.port, got: {err}"
        );
    }

    #[test]
    fn test_validate_rtsp_port_low_rejected() {
        let cfg = AppConfig {
            rtsp: RtspConfig { server_port: 554 },
            ..AppConfig::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("rtsp.server_port"),
            "error should mention rtsp.server_port, got: {err}"
        );
    }

    #[test]
    fn test_validate_port_conflict_rejected() {
        let cfg = AppConfig {
            web: WebConfig {
                port: 8554,
                host: "0.0.0.0".into(),
                advertised_host: None,
            },
            rtsp: RtspConfig { server_port: 8554 },
            ..AppConfig::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("web.port"),
            "error should mention port conflict, got: {err}"
        );
        assert!(
            err.contains("rtsp.server_port"),
            "error should mention rtsp.server_port, got: {err}"
        );
    }

    #[test]
    fn test_validate_rate_limit_zero_rejected() {
        let cfg = AppConfig {
            security: SecurityConfig {
                rate_limit_max: 0,
                rate_limit_window_secs: 60,
            },
            ..AppConfig::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("rate_limit_max"),
            "error should mention rate_limit_max, got: {err}"
        );
    }

    #[test]
    fn test_validate_gb28181_interval_zero_rejected() {
        let cfg = AppConfig {
            gb28181: Gb28181Config {
                enabled: true,
                register_interval_secs: 0,
                ..Gb28181Config::default()
            },
            ..AppConfig::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("register_interval_secs"),
            "error should mention register_interval_secs, got: {err}"
        );
    }

    #[test]
    fn test_validate_rtmp_reconnect_interval_zero_rejected() {
        let cfg = AppConfig {
            rtmp_push: RtmpPushConfig {
                enabled: true,
                reconnect_interval_secs: 0,
                ..RtmpPushConfig::default()
            },
            ..AppConfig::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("reconnect_interval_secs"),
            "error should mention reconnect_interval_secs, got: {err}"
        );
    }

    #[test]
    fn test_validate_rtmp_max_reconnect_zero_rejected() {
        let cfg = AppConfig {
            rtmp_push: RtmpPushConfig {
                enabled: true,
                max_reconnect_attempts: 0,
                ..RtmpPushConfig::default()
            },
            ..AppConfig::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("max_reconnect_attempts"),
            "error should mention max_reconnect_attempts, got: {err}"
        );
    }

    #[test]
    fn test_validate_gb28181_sip_port_low_rejected() {
        let cfg = AppConfig {
            gb28181: Gb28181Config {
                enabled: true,
                platform_sip_port: 506,
                ..Gb28181Config::default()
            },
            ..AppConfig::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("platform_sip_port"),
            "error should mention platform_sip_port, got: {err}"
        );
    }
}
