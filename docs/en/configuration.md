# Configuration Reference

This document provides a complete reference for the mibee-rec configuration system.

## Overview

Configuration files control all aspects of mibee-rec behavior. The configuration system supports hierarchical precedence, allowing different settings for development, testing, and production environments.

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
advertised_host = "192.168.1.100"
```

**Field Reference:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `port` | u16 | `8443` | HTTPS port for web UI and REST API (must be > 1024) |
| `host` | String | `"0.0.0.0"` | Bind address: `"0.0.0.0"` (all interfaces) or `"127.0.0.1"` (localhost only) |
| `advertised_host` | Option<String> | `None` | Advertised hostname/IP for URLs returned to clients. If None, auto-detected at startup via UDP probe. Eliminates hardcoded localhost in external URLs. |

**Notes:**

- All web traffic uses TLS (HTTPS) via rustls
- Self-signed TLS certificates are auto-generated on first run at `tls/cert.pem` + `tls/key.pem`
- Certificates support hot-reload on file mtime change
- Production environments should supply CA-signed certificates
- Default ports avoid privileged range (< 1024)

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

- RTSP server operates in SERVER mode (clients pull streams from this machine)
- RTSP protocol supports RFC 2326 with Digest authentication and RTP interleaved
- Default port avoids privileged range; no privilege binding required
- Supports H.264 streaming with proper SPS/PPS headers
- External clients (NVRs, VLC) connect to `rtsp://this-host:8554/stream` to pull streams

### [rtmp_push] - RTMP Push Client

Configure the RTMP push client for outbound streaming to external ingest servers (NVRs, live platforms).

**All RTMP operations are OUTBOUND PUSH — this machine pushes to external endpoints. There is no RTMP ingest server.**

```toml
[rtmp_push]
enabled = false
push_url = "rtmp://192.168.1.100:1935/live"
app_name = "live"
stream_name = "stream1"
reconnect_interval_secs = 5
max_reconnect_attempts = 10
```

**Field Reference:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `false` | Master enable toggle (default OFF for all outbound protocols) |
| `push_url` | String | `"rtmp://192.168.1.100:1935/live"` | External RTMP ingest endpoint URL |
| `app_name` | String | `"live"` | RTMP application name |
| `stream_name` | String | `"stream1"` | RTMP stream key/name |
| `reconnect_interval_secs` | u64 | `5` | Reconnection attempt interval in seconds (must be > 0 if enabled) |
| `max_reconnect_attempts` | u32 | `10` | Maximum reconnection attempts (must be > 0 if enabled) |

**Notes:**

- This is OUTBOUND push only — this machine pushes to external RTMP ingest servers
- No RTMP ingest server exists; external clients cannot push to this machine via RTMP
- Hand-written RTMP implementation with enhanced timestamp support
- Automatic reconnection with exponential backoff on connection loss
- Protocol hot-toggle available via Web UI without server restart

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
- Audio capture runs at high priority; never block in callbacks
- This product is LOCAL-ONLY capture — it does NOT discover or pull from remote network cameras

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
| `rate_limit_max` | usize | `20` | Maximum requests per rate limit window (must be > 0) |
| `rate_limit_window_secs` | u64 | `60` | Rate limit time window in seconds |

**Notes:**

- Implements sliding window rate limiting for authentication endpoints using `parking_lot::Mutex`
- Prevents brute force attacks on login system
- Rate limiting is per-IP address basis
- Login failure lockout with exponential backoff after 5 failures
- Mutex is non-poisoning (safe for request handlers)

### [observability] - Monitoring and Logging

Configure OpenTelemetry tracing, application logging, and optional remote log shipping.

```toml
[observability]
otel_endpoint = "http://localhost:4317"
log_level = "info"

[observability.logs]
endpoint = "http://loki:3100/loki/api/v1/push"
batch_size = 100
flush_interval_secs = 5

[observability.logs.labels]
environment = "production"
service = "mibee-rec"
```

**Field Reference:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `otel_endpoint` | String | `"http://localhost:4317"` | OpenTelemetry collector endpoint (OTLP gRPC) |
| `log_level` | String | `"info"` | Log level filter |
| `logs` | Option<RemoteLogConfig> | `None` | Optional remote log shipping configuration |

**[observability.logs] RemoteLogConfig Fields:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `endpoint` | String | `""` | Loki-compatible HTTP endpoint URL for remote log shipping |
| `batch_size` | usize | `100` | Number of log entries to batch per flush |
| `flush_interval_secs` | u64 | `5` | Flush interval in seconds |
| `labels` | HashMap<String, String> | `{}` | Additional labels attached to every log stream |

**Log Level Options:**

- `"trace"` - Most detailed logging, debugging information
- `"debug"` - Debug information, function calls
- `"info"` - General operational information (default)
- `"warn"` - Warning conditions that don't stop operation
- `"error"` - Error conditions that may impact operation

**Notes:**

- OpenTelemetry integration is optional — system works without collector
- OTLP (OpenTelemetry Protocol) over gRPC on port 4317
- Structured JSON logging when OpenTelemetry is unavailable
- Remote log shipping to Loki is optional and fail-open: unreachable endpoint logs a warning, application continues
- W3C TraceContext `traceparent` header extracted on incoming requests, injected on outbound
- Prometheus `/metrics` endpoint available for scraping (no auth, firewall in production)

### [onvif] - ONVIF Device Endpoint

Configure the ONVIF device endpoint for external NVR discovery via WS-Discovery.

**This machine acts as the ONVIF camera — external NVRs discover IT. This is NOT an ONVIF client.**

```toml
[onvif]
enabled = false
device_name = "mibee-rec"
manufacturer = "MiBee"
model = "Rec-01"
serial = "NC00000001"
firmware_version = "1.0.0"
port = 3702
```

**Field Reference:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `false` | Master enable toggle (default OFF for all outbound protocols) |
| `device_name` | String | `"mibee-rec"` | ONVIF device name |
| `manufacturer` | String | `"MiBee"` | Manufacturer name |
| `model` | String | `"Rec-01"` | Device model |
| `serial` | String | `"NC00000001"` | Serial number |
| `firmware_version` | String | `"1.0.0"` | Firmware version |
| `port` | u16 | `3702` | WS-Discovery UDP port (hardcoded) |

**Notes:**

- OUTBOUND protocol — this device broadcasts WS-Discovery messages, external NVRs discover it
- Hand-written WS-Discovery server (701 LOC)
- Protocol hot-toggle available via Web UI without server restart

### [gb28181] - GB/T 28181 Device Registration

Configure GB/T 28181 device registration with a Chinese surveillance platform.

**This device registers WITH the platform via SIP REGISTER; the platform sends INVITE, this device pushes RTP.**

```toml
[gb28181]
enabled = false
platform_sip_address = "192.168.1.100"
platform_sip_port = 5060
device_id = "34020000002000000001"
username = ""
password = ""
sip_domain = "3402000000"
register_interval_secs = 60
```

**Field Reference:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `false` | Master enable toggle (default OFF for all outbound protocols) |
| `platform_sip_address` | String | `"192.168.1.100"` | SIP platform address |
| `platform_sip_port` | u16 | `5060` | SIP platform port (must be > 1024 if enabled) |
| `device_id` | String | `"34020000002000000001"` | 20-character GB28181 device ID |
| `username` | String | `""` | SIP authentication username |
| `password` | String | `""` | SIP authentication password |
| `sip_domain` | String | `"3402000000"` | SIP domain |
| `register_interval_secs` | u64 | `60` | SIP REGISTER interval in seconds (must be > 0 if enabled) |

**Notes:**

- OUTBOUND protocol — this device registers WITH the platform, not the platform role
- Hand-written SIP device client + RTP pusher (2033 LOC)
- Protocol hot-toggle available via Web UI without server restart

### [recording] - Local Recording

Configure local MP4 segment recording to disk.

```toml
[recording]
enabled = false
path = "./recordings"
segment_duration_secs = 900
max_capacity_mb = 10240
```

**Field Reference:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `false` | Master enable toggle. Individual streams can opt out via Web UI. |
| `path` | String | `"./recordings"` | Directory where MP4 segment files are written (must be writable) |
| `segment_duration_secs` | u64 | `900` | MP4 segment duration in seconds (must be > 0). Default: 15 minutes. |
| `max_capacity_mb` | u64 | `10240` | Max total capacity in megabytes. 0 = unlimited (no pruning). Default: 10 GB. |

**Notes:**

- Captured H.264 frames are muxed into rolling MP4 segments
- Oldest segments are auto-pruned when total size exceeds `max_capacity_mb`
- Individual streams can enable/disable recording via Web UI

### [database] - SQLite Database

Configure SQLite database path for camera settings, protocol configs, sessions, and users.

```toml
[database]
path = "~/.local/share/mibee-rec/mibee_rec.db"
```

**Field Reference:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `path` | String | `~/.local/share/mibee-rec/mibee_rec.db` | SQLite database file path (XDG-compliant default) |

**Notes:**

- Uses XDG data directory for default path: `~/.local/share/mibee-rec/mibee_rec.db`
- Fallback to `/tmp/mibee-rec/mibee_rec.db` if XDG data dir unavailable
- Stores camera configs, protocol configs, sessions, users, and stream session data

## Configuration Validation

Configuration is validated on startup. The following rules apply:

- **Port constraints**: All ports must be > 1024 (web.port, rtsp.server_port, gb28181.platform_sip_port)
- **Port conflicts**: web.port must not equal rtsp.server_port
- **Rate limiting**: security.rate_limit_max must be > 0
- **GB28181 interval**: gb28181.register_interval_secs must be > 0 (if enabled)
- **RTMP push**: rtmp_push.reconnect_interval_secs must be > 0 (if enabled)
- **RTMP push**: rtmp_push.max_reconnect_attempts must be > 0 (if enabled)
- **Log level**: observability.log_level must be one of: trace, debug, info, warn, error
- **Recording path**: recording.path must not be empty
- **Recording segment**: recording.segment_duration_secs must be > 0

## Examples

### Production Configuration with All Protocols Enabled

```toml
# Production configuration with all outbound protocols enabled
[web]
port = 8443
host = "0.0.0.0"
advertised_host = "192.168.1.100"

[rtsp]
server_port = 8554

[rtmp_push]
enabled = true
push_url = "rtmp://your-nvr.example.com:1935/live"
app_name = "live"
stream_name = "webcam-stream"
reconnect_interval_secs = 5
max_reconnect_attempts = 10

[onvif]
enabled = true
device_name = "mibee-rec"
manufacturer = "MiBee"
model = "Rec-01"
serial = "NC00000001"
firmware_version = "1.0.0"
port = 3702

[gb28181]
enabled = true
platform_sip_address = "192.168.1.100"
platform_sip_port = 5060
device_id = "34020000002000000001"
username = "admin"
password = "secret123"
sip_domain = "3402000000"
register_interval_secs = 60

[recording]
enabled = true
path = "/var/lib/mibee-rec/recordings"
segment_duration_secs = 900
max_capacity_mb = 20480

[database]
path = "/var/lib/mibee-rec/mibee_rec.db"

[security]
rate_limit_max = 10
rate_limit_window_secs = 30

[observability]
otel_endpoint = "https://monitoring.example.com:4317"
log_level = "warn"

[observability.logs]
endpoint = "http://loki:3100/loki/api/v1/push"
batch_size = 100
flush_interval_secs = 5

[observability.logs.labels]
environment = "production"
service = "mibee-rec"
```

### Local Development Configuration

```toml
# Local development with minimal security constraints
[web]
host = "127.0.0.1"
advertised_host = "127.0.0.1"

[security]
rate_limit_max = 100

[observability]
log_level = "debug"
```

### Minimal Configuration

```toml
# Minimal configuration — most fields use defaults
[web]
port = 8443

[capture]
video_device = "/dev/video0"
audio_device = "default"

[recording]
enabled = true
path = "./recordings"
```

### Resource-Constrained Environment

```toml
# Optimized for low-resource environments
[web]
host = "127.0.0.1"

[observability]
otel_endpoint = ""
log_level = "error"

[security]
rate_limit_max = 5

[recording]
enabled = false
```

### Network-Specific Configuration

```toml
# Configuration for different network environments
[web]
host = "192.168.1.100"
advertised_host = "192.168.1.100"

[rtsp]
server_port = 8554

[capture]
video_device = "/dev/video2"
audio_device = "hw:1"

[rtmp_push]
enabled = true
push_url = "rtmp://streaming.example.com:1935/live"
```

### TLS Certificate Management

```toml
# Production with custom TLS certificates
[web]
port = 8443
# Supply your own CA-signed certificates at tls/cert.pem and tls/key.pem
# Certificates auto-reload on file mtime change
```

**TLS Certificate Notes:**

- Self-signed certificates are auto-generated on first run at `tls/cert.pem` + `tls/key.pem`
- Certificates support hot-reload on file mtime change
- Production environments should supply CA-signed certificates
- CommonName: CN=mibee-rec, SAN: mibee-rec.local
- Validity: ~30 days for auto-generated dev certificates

## Configuration Best Practices

1. **Never commit secrets**: Store sensitive configurations in `config.local.toml` which is gitignored
2. **Use descriptive file names**: `config.prod.toml`, `config.test.toml` for different environments
3. **Validate configuration**: Test configuration files before deployment (run app with --config flag)
4. **Document changes**: Keep configuration documentation updated with new options
5. **Monitor performance**: Use observability to track resource usage with different configurations
6. **Protocol defaults**: All outbound protocols (RTMP push, ONVIF, GB28181) default to OFF for security
7. **Port constraints**: Use ports > 1024 to avoid privilege requirements
8. **Advertised host**: Set `web.advertised_host` for multi-homed networks or behind NAT

## Troubleshooting

### Common Issues

1. **Port already in use**: Try different ports or ensure previous instances are stopped. Check port conflicts in validation.
2. **Device not found**: Verify device paths and permissions (Linux: `video` group membership). This is local-only capture — does not discover remote cameras.
3. **Rate limiting issues**: Check `security` section configuration. Ensure `rate_limit_max` > 0.
4. **TLS errors**: Verify certificate configuration and endpoints. Check `tls/cert.pem` and `tls/key.pem` existence.
5. **OpenTelemetry failures**: System continues to function without collector, but metrics may be lost. Validation passes.
6. **Port conflicts**: web.port must not equal rtsp.server_port. Validation will reject.
7. **GB28181 registration fails**: Check platform_sip_address, platform_sip_port, username, password. Platform must be reachable.
8. **RTMP push fails**: Verify push_url, app_name, stream_name. External ingest server must be accepting connections.
9. **Recording path not writable**: Ensure recording.path exists and is writable. Auto-pruning requires read/write access.

### Validation

Validate configuration files by starting the application with `--config`:

```bash
# Test configuration by starting the application
cargo run -- --config test-config.toml

# If configuration is invalid, startup fails with descriptive error
# Example error: "web.port: must be > 1024, got 80"
# Example error: "rtsp.server_port: must not equal web.port"
```

### Getting Help

For configuration issues:
1. Check this reference document
2. Review the source code in `src/config.rs` and `crates/web/src/config.rs` for authoritative defaults
3. Examine the default `config.toml` file
4. Check GitHub issues and discussions
5. Validate with startup: `cargo run -- --config your-config.toml`