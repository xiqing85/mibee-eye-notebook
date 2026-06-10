/// Unified error type for notebook-cam.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// I/O errors (file system, network sockets, etc.)
    #[error("I/O error: {0}")]
    Io(tokio::io::Error),

    /// Configuration parsing or validation errors.
    #[error("Configuration error: {0}")]
    Config(String),

    /// Camera/audio capture errors.
    #[error("Capture error: {0}")]
    Capture(String),

    /// Protocol errors (RTSP, ONVIF, GB28181, RTMP).
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// Authentication/authorization errors.
    #[error("Authentication error: {0}")]
    Auth(String),

    /// Database errors.
    #[error("Database error: {0}")]
    Database(String),

    /// Resource not found.
    #[error("Not found: {0}")]
    NotFound(String),
}

impl From<tokio::io::Error> for Error {
    fn from(err: tokio::io::Error) -> Self {
        Error::Io(err)
    }
}

impl From<toml::de::Error> for Error {
    fn from(err: toml::de::Error) -> Self {
        Error::Config(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_io() {
        let err = Error::Io(tokio::io::Error::new(
            tokio::io::ErrorKind::NotFound,
            "file not found",
        ));
        assert_eq!(err.to_string(), "I/O error: file not found");
    }

    #[test]
    fn test_display_config() {
        let err = Error::Config("missing key".into());
        assert_eq!(err.to_string(), "Configuration error: missing key");
    }

    #[test]
    fn test_display_capture() {
        let err = Error::Capture("device busy".into());
        assert_eq!(err.to_string(), "Capture error: device busy");
    }

    #[test]
    fn test_display_protocol() {
        let err = Error::Protocol("timeout".into());
        assert_eq!(err.to_string(), "Protocol error: timeout");
    }

    #[test]
    fn test_display_auth() {
        let err = Error::Auth("invalid credentials".into());
        assert_eq!(err.to_string(), "Authentication error: invalid credentials");
    }

    #[test]
    fn test_display_database() {
        let err = Error::Database("connection failed".into());
        assert_eq!(err.to_string(), "Database error: connection failed");
    }

    #[test]
    fn test_display_not_found() {
        let err = Error::NotFound("camera".into());
        assert_eq!(err.to_string(), "Not found: camera");
    }

    #[test]
    fn test_from_tokio_io_error() {
        let io_err = tokio::io::Error::new(tokio::io::ErrorKind::PermissionDenied, "access denied");
        let err: Error = io_err.into();
        assert!(matches!(err, Error::Io(_)));
        assert_eq!(err.to_string(), "I/O error: access denied");
    }

    #[test]
    fn test_from_std_io_error() {
        // std::io::Error converts via tokio::io::Error::from()
        let std_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "std io error");
        let err = Error::Io(std_err);
        assert!(matches!(err, Error::Io(_)));
        assert_eq!(err.to_string(), "I/O error: std io error");
    }

    #[test]
    fn test_from_toml_de_error() {
        let result: Result<toml::Value, _> = toml::from_str("key = [invalid");
        let toml_err = result.unwrap_err();
        let err: Error = toml_err.into();
        assert!(matches!(err, Error::Config(_)));
        assert!(err.to_string().starts_with("Configuration error:"));
    }
}
