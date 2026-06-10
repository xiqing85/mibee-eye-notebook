# MiBee Rec

Professional laptop surveillance agent built in Rust.

Captures webcam and microphone, connects to IP cameras and NVRs, and streams via RTSP / RTMP / ONVIF / GB/T 28181. Part of the [MiBee](https://https://github.com/xiqing85) ecosystem.

## Features

- **Local capture** — webcam via V4L2 (Linux) / MSMF (Windows), microphone via ALSA / WASAPI
- **Protocol support** — RTSP client & server, RTMP ingest, ONVIF discovery & PTZ, GB/T 28181 (SIP + RTP)
- **H.264 / H.265** — hand-written NAL unit parser, keyframe detection, SPS/PPS extraction
- **MiBee NVR integration** — REST API client, camera sync, SSE event stream
- **Web UI** — Axum REST API + embedded SPA, TLS via rustls, session-based auth
- **Resource-bounded** — semaphore-guarded concurrency (max 16 streams), per-stream memory budgets
- **Observable** — structured logging, OpenTelemetry export, Prometheus metrics endpoint
- **Low footprint** — targets <5% CPU idle, <200 MB RAM; zero-copy where possible

## Architecture

```
┌──────────────────────────────┐
│  Web UI (Axum + embedded SPA)│
├──────────────────────────────┤
│  Streaming Hub               │
│  Source → fan-out → N Outputs│
├──────────────────────────────┤
│  Protocol Layer              │
│  RTSP · RTMP · ONVIF · 28181│
├──────────────────────────────┤
│  Capture Layer               │
│  Video (nokhwa) · Audio(cpal)│
├──────────────────────────────┤
│  Security · Observability    │
└──────────────────────────────┘
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

## License

This project is licensed under a **non-commercial source-available license**.

You may use, study, and modify the code for non-commercial purposes. Commercial use requires explicit written permission. See [LICENSE](LICENSE) for details.
