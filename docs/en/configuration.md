# Configuration Reference

This document provides a complete reference for the notebook-cam configuration system.

## Overview

Configuration files control all aspects of notebook-cam behavior. The configuration system supports hierarchical precedence, allowing different settings for development, testing, and production environments.

### Configuration File Locations

1. **Default configuration**: `config.toml` (project root)
2. **Local override**: `config.local.toml` (project root)
3. **CLI override**: `--config <path>` command-line argument

### Precedence Rules

Configuration values are loaded in order of precedence (highest wins):

1. Command-line `--config <path>` (absolute highest priority)
2. `config.local.toml` (gitignored, for local development)
3. `config.toml` (default configuration)

When a file doesn't exist, the system falls back to compiled-in defaults. Individual sections and fields not specified in a configuration file will use their default values.

## Configuration Reference

### [web] - Web UI Server

Configure the HTTPS web server hosting the user interface and REST API.

```toml
[web]
port = 8443
host = "0.0.0.0"
```

**Field Reference:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `port` | u16 | `8443` | HTTPS port for web UI and REST API |
| `host` | String | `"0.0.0.0"` | Bind address: `"0.0.0.0"` (all interfaces) or `"127.0.0.1"` (localhost only) |

**Notes:**
- All web traffic uses TLS (HTTPS) via rustls
- Default port requires privilege binding or `setcap 'cap_net_bind_service=+ep'`
- `"127.0.0.1"` restricts access to local connections only

### [rtsp] - RTSP Server

Configure the RTSP streaming server for camera feeds and client connections.

```toml
[rtsp]
server_port = 8554
```

**Field Reference:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `server_port` | u16 | `8554` | RTSP server listening port |

**Notes:**
- RTSP protocol supports both client (connect to IP cameras) and server (serve streams) modes
- Default port requires privilege binding if <1024
- Supports H.264/H.265 streaming with proper SPS/PPS headers

### [rtmp] - RTMP Ingest Server

Configure the RTMP ingest server for external stream sources.

```toml
[rtmp]
ingest_port = 1935
```

**Field Reference:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `ingest_port` | u16 | `1935` | RTMP ingest server listening port |

**Notes:**
- Used for ingesting streams from external sources (e.g., OBS, FFmpeg, other cameras)
- Hand-written RTMP implementation with enhanced timestamp support
- Default port is standard RTMP port, no privilege binding required

### [capture] - Local Capture Devices

Configure local webcam and microphone capture.

```toml
[capture]
video_device = "/dev/video0"
audio_device = "default"
```

**Field Reference:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `video_device` | String | `"/dev/video0"` | Video capture device path |
| `audio_device` | String | `"default"` | Audio capture device identifier |

**Notes:**
- **Linux**: Video device typically `/dev/video0`, `/dev/video1`, etc.
- **Linux**: Audio device `"default"` uses ALSA default device
- **Windows**: Video devices use MSMF (Media Foundation) device names
- **Windows**: Audio devices use WASAPI device names
- Video capture requires `libv4l-dev` and user in `video` group
- Audio capture runs at high priority, never block in callbacks

### [security] - Authentication and Rate Limiting

Configure security policies for authentication and API protection.

```toml
[security]
rate_limit_max = 20
rate_limit_window_secs = 60
```

**Field Reference:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `rate_limit_max` | usize | `20` | Maximum requests per rate limit window |
| `rate_limit_window_secs` | u64 | `60` | Rate limit time window in seconds |

**Notes:**
- Implements sliding window rate limiting for authentication endpoints
- Prevents brute force attacks on login system
- Rate limiting uses per-IP address basis

### [observability] - Monitoring and Logging

Configure OpenTelemetry tracing and application logging.

```toml
[observability]
otel_endpoint = "http://localhost:4317"
log_level = "info"
```

**Field Reference:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `otel_endpoint` | String | `"http://localhost:4317"` | OpenTelemetry collector endpoint |
| `log_level` | String | `"info"` | Log level filter |

**Log Level Options:**
- `"trace"` - Most detailed logging, debugging information
- `"debug"` - Debug information, function calls
- `"info"` - General operational information (default)
- `"warn"` - Warning conditions that don't stop operation
- `"error"` - Error conditions that may impact operation

**Notes:**
- OpenTelemetry integration is optional - system works without collector
- OTLP (OpenTelemetry Protocol) over gRPC on port 4317
- Structured JSON logging when OpenTelemetry is unavailable
- Metrics endpoint available for Prometheus scraping (if configured)

## Examples

### Local Development Configuration

```toml
# Local development with minimal security constraints
[web]
host = "127.0.0.1"  # Only accessible from local machine

[security]
rate_limit_max = 100  # More lenient for local development
```

### Production Deployment Configuration

```toml
# Production configuration with security hardening
[web]
host = "0.0.0.0"  # Accessible from all network interfaces

[security]
rate_limit_max = 10  # Strict rate limiting for production
rate_limit_window_secs = 30  # Shorter window

[observability]
otel_endpoint = "https://monitoring.example.com:4317"
log_level = "warn"  # Reduce noise in production
```

### Resource-Constrained Environment

```toml
# Optimized for low-resource environments
[web]
host = "127.0.0.1"  # Restrict to localhost only

[observability]
otel_endpoint = ""  # Disable OpenTelemetry to reduce overhead
log_level = "error"  # Only log errors

[security]
rate_limit_max = 5  # Very strict rate limiting
```

### Network-Specific Configuration

```toml
# Configuration for different network environments
[web]
host = "192.168.1.100"  # Bind to specific interface

[rtsp]
server_port = 8554

[rtmp]
ingest_port = 1935

[capture]
video_device = "/dev/video2"  # Secondary camera
audio_device = "hw:1"  # ALSA hardware device 1
```

### Development with Debug Logging

```toml
# Development with extensive logging
[observability]
otel_endpoint = "http://localhost:4317"
log_level = "debug"

[security]
rate_limit_max = 1000  # Disable rate limiting for development
```

## Configuration Best Practices

1. **Never commit secrets**: Store sensitive configurations in `config.local.toml` which is gitignored
2. **Use descriptive file names**: `config.prod.toml`, `config.test.toml` for different environments
3. **Validate configuration**: Test configuration files before deployment
4. **Document changes**: Keep configuration documentation updated with new options
5. **Monitor performance**: Use observability to track resource usage with different configurations

## Troubleshooting

### Common Issues

1. **Port already in use**: Try different ports or ensure previous instances are stopped
2. **Device not found**: Verify device paths and permissions (Linux: `video` group membership)
3. **Rate limiting issues**: Check `security` section configuration
4. **TLS errors**: Verify certificate configuration and endpoints
5. **OpenTelemetry failures**: System continues to function without collector, but metrics may be lost

### Validation

Validate configuration files using the built-in validation:

```bash
# Create test configuration
cat > test-config.toml << EOF
[web]
port = 8443
host = "127.0.0.1"
EOF

# Test loading the configuration
cargo run -- --config test-config.toml --help
```

### Getting Help

For configuration issues:
1. Check this reference document
2. Review the source code in `src/config.rs` for authoritative defaults
3. Examine the default `config.toml` file
4. Check GitHub issues and discussions