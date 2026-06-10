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
