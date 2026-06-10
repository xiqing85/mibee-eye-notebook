# Getting Started

Quick start guide for notebook-cam (MiBee Rec) — laptop surveillance agent built in Rust.

## Prerequisites

**Rust:** Requires Rust 1.85+ with Cargo.

**Linux System Dependencies:**

```bash
# Install required system packages
sudo apt install libv4l-dev libasound2-dev libclang-dev

# Add user to video group for webcam access
sudo usermod -aG video $USER

# Log out and back in for group change to take effect
```

**Windows:** Secondary support. Install via vcpkg when available.

## Build

Build the project using Cargo:

```bash
# Build in debug mode (development)
cargo build

# Build in release mode (production)
cargo build --release
```

The release binary will be available at `target/release/mibee-rec`.

Default ports:
- Web UI: 8443 (HTTPS with self-signed TLS)
- RTSP Server: 8554
- RTMP Ingest: 1935

## First Run

**On first execution**, the server runs in setup mode. You must create an admin user before accessing the web UI.

Start the server:

```bash
cargo run -- --config config.toml
```

The server will detect this is the first run and allow access to the setup endpoint without authentication.

Create your admin user using curl:

```bash
curl -X POST https://localhost:8443/api/auth/setup \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"yourpass123"}'
```

**Requirements:**
- Username cannot be empty
- Password must be at least 8 characters

On success, the server:
- Creates the admin user in the SQLite database
- Generates a self-signed TLS certificate for HTTPS
- Starts the web server on port 8443

## Access Web UI

Open your browser and navigate to:

``
https://localhost:8443
``

**Important:** You will see a security warning about the self-signed certificate. This is expected in development. Click "Advanced" and "Proceed to localhost" to continue.

After setup, the web UI requires authentication using session-based cookies.

## Add a Camera

Once setup is complete, you can add cameras via the API.

**Example: Add an RTSP camera**

```bash
curl -X POST https://localhost:8443/api/cameras \
  -H "Content-Type: application/json" \
  -H "Cookie: session=<your-session-token>" \
  -d '{
    "name": "Front Door Camera",
    "camera_type": "rtsp",
    "config": {
      "url": "rtsp://192.168.1.100:554/stream"
    }
  }'
```

**Supported Camera Types:**
- `usb` - Local webcam/microphone
- `rtsp` - IP camera via RTSP
- `onvif` - ONVIF-compatible cameras
- `gb28181` - GB/T 28181 standard
- `rtmp` - RTMP ingest streams

**Note:** The `login`, `logout`, and `snapshot` endpoints currently return 501 (not implemented). Use the setup endpoint for authentication.

## Next Steps

Continue with these resources:

- [Installation Guide](installation.md) - Detailed installation instructions
- [Configuration Guide](configuration.md) - Advanced configuration options
- [API Reference](api.md) - Complete API documentation

For development and testing:
- `cargo test` - Run all tests
- `cargo ci-clippy` - Run clippy linter
- `cargo fmt-check` - Check formatting

Happy monitoring!

---
*Notebook-cam (MiBee Rec) — Professional laptop surveillance agent built in Rust.*