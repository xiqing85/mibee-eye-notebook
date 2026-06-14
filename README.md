# MiBee Rec

[![License: Non-Commercial](https://img.shields.io/badge/License-Non--Commercial-blue.svg)](LICENSE)
[![Rust: 1.85+](https://img.shields.io/badge/Rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)
[![Platform: Linux](https://img.shields.io/badge/Platform-Linux%20%7C%20Windows-green.svg)](#)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](docs/en/contributing.md)

[中文文档](README.zh-CN.md) · [Documentation](docs/en/)

Captures webcam and microphone from the host machine, encodes to H.264/AAC, and serves streams to external NVRs via RTSP Server / RTMP Push / ONVIF Device / GB/T 28181 Device. Part of the [MiBee](https://https://github.com/xiqing85) ecosystem.

## Features

- **Local capture** — webcam via V4L2 (Linux) / MSMF (Windows), microphone via ALSA / WASAPI
- **Outbound protocols** — RTSP server (clients pull), RTMP push, ONVIF device endpoint, GB/T 28181 device registration
- **H.264 / H.265** — hand-written NAL unit parser, keyframe detection, SPS/PPS extraction
- **MiBee NVR integration** — REST API client, camera sync, SSE event stream
- **Web UI** — Axum REST API + embedded SPA, TLS via rustls, session-based auth
- **Resource-bounded** — semaphore-guarded concurrency (max 16 streams), per-stream memory budgets
- **Observable** — structured logging, OpenTelemetry export, Prometheus metrics endpoint
- **Low footprint** — targets <5% CPU idle, <200 MB RAM; zero-copy where possible

### Crate Responsibilities

| Crate | LOC | Role |
|-------|-----|------|
| `protocols` | ~11.4k | RTSP, RTMP, ONVIF, GB28181, RTP, H.264 — hand-written codec and protocol implementations |
| `streaming` | ~4.1k | StreamHub fan-out orchestrator, source/output adapters, MiBee NVR client |
| `web` | ~2.5k | Axum REST API + embedded SPA + TLS via rustls |
| `security` | ~1.9k | Session-based auth, rate limiting, encryption |
| `capture` | ~800 | Video (nokhwa) + Audio (cpal) device wrappers |
| `observability` | ~423 | Structured tracing, OpenTelemetry export, Prometheus metrics |
## Architecture

```
┌──────────────────────────────────┐
│        Web UI (Axum + SPA)       │
│  REST API · TLS · Auth Session   │
├──────────────────────────────────┤
│       Streaming Hub              │
│  Source → BufferPool → fan-out   │
│  ResourceController (max 16)     │
├──────────────────────────────────┤
│        Protocol Layer            │
│  RTSP · RTMP · ONVIF · GB28181   │
│  RTP · H.264 NAL Parser          │
├──────────────────────────────────┤
│        Capture Layer             │
│  Video (nokhwa) · Audio (cpal)   │
├──────────────────────────────────┤
│   Security · Observability       │
│  Auth · TLS · Tracing · Metrics  │
└──────────────────────────────────┘
```

## Workspace Layout

```
mibee-rec/
├─ src/                # Binary entry, config, types, error
├─ crates/
│  ├─ protocols/       # RTSP, RTMP, ONVIF, GB28181, RTP, H.264
│  ├─ streaming/       # StreamHub fan-out, source/output adapters, MiBee client
│  ├─ web/             # Axum REST API + embedded SPA + TLS
│  ├─ security/        # Auth, TLS, encryption, rate limiting
│  ├─ capture/         # Video + Audio capture wrappers
│  └─ observability/   # tracing + OTel + Prometheus
├─ migrations/         # SQLite schema
└─ config.toml         # Default runtime config
```

## Protocol Support Status

| Protocol | Component | Implementation | Status |
|----------|-----------|----------------|--------|
| RTSP | Server | Hand-written (`RtspServer`) — external clients connect to pull streams | ✅ |
| RTMP | Push client | Push local stream to external NVR ingest | ✅ |
| ONVIF | Device endpoint | Serve device info, let external NVR discover this host | ✅ |
| GB/T 28181 | Device | Register with external platform, push RTP on INVITE | ✅ |
| GB/T 28181 | SIP + RTP | Wrapper via [gmv](https://crates.io/crates/gmv) (`Gb28181Client`) | ✅ |
| H.264 | NAL unit parser | Hand-written (`H264Parser`) | ✅ |
| H.265 | Decoding | Browser fallback to H.264 | ⚠️ |
| CaptureSource | Capture→streaming adapter | `crates/streaming/src/capture_source.rs` | ✅ |
| Streaming → Root | Wiring | root `main.rs` → streaming crate | ✅ |
| Auth Login/Logout | Session management | Returns 501 | 🚧 Stub |

**Legend**: ✅ Implemented · ⚠️ Partial / Fallback · ❌ Missing · 🚧 Stub

## Resource Targets

| Metric | Target | Mechanism |
|--------|--------|-----------|
| CPU (idle) | <5% | Zero-copy I/O, async everywhere, no busy loops |
| Memory | <200 MB | `BufferPool` (10 MB pool), per-stream budgets, `ResourceController` |
| Concurrent streams | ≤16 | `tokio::sync::Semaphore`-guarded in `ResourceController` |
| First frame latency | <500 ms | Minimal buffering, eager keyframe detection |

## Quick Start

```bash
# Clone and enter
git clone https://github.com/xiqing85/mibee-eye-notebook.git
cd mibee-rec

# Install system dependencies (Linux)
sudo apt install libv4l-dev libasound2-dev libclang-dev
sudo usermod -aG video $USER
# Log out and back in for group change to take effect

# Build
cargo build --release

# Configure
cp config.toml config.local.toml
# Edit config.local.toml as needed

# Run
cargo run --release -- --config config.local.toml
```

## Building

**Prerequisites (Linux):**

```bash
# Install system dependencies
sudo apt install libv4l-dev libasound2-dev libclang-dev

# Add user to video group for webcam access
sudo usermod -aG video $USER
# Log out and back in for group change to take effect
```

**Build & Run:**

```bash
cargo build                                # Debug build
cargo build --release                      # Release build
cargo run -- --config config.toml          # Run with config
cargo run -- --reset-password              # Password reset CLI
```

**Development:**

```bash
cargo test                                 # Run all tests
cargo test -p protocols                    # Test single crate
cargo ci-clippy                            # Lint (clippy -D warnings)
cargo fmt-check                            # Format check
```

## Configuration

Copy and edit the default config:

```bash
cp config.toml config.local.toml
```

`config.local.toml` is gitignored — put local overrides there.

Default ports: web UI `8443` (TLS), RTSP `8554`, RTMP `1935`.

## Documentation

Full documentation is available under [docs/en/](docs/en/):

- [Getting Started](docs/en/getting-started.md)
- [Installation](docs/en/installation.md)
- [Configuration](docs/en/configuration.md)
- [API Reference](docs/en/api.md)
- [Architecture](docs/en/architecture.md)
- [Contributing](docs/en/contributing.md)

Chinese documentation: [README.zh-CN.md](README.zh-CN.md) | [docs/zh/](docs/zh/)

---


## License

This project is licensed under a **non-commercial source-available license**.

You may use, study, and modify the code for non-commercial purposes. Commercial use requires explicit written permission. See [LICENSE](LICENSE) for details.
