# Changelog

All notable changes to MiBee-Rec are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

- **ONVIF Pull-Point events service** (onvif-device-rs 0.7): AI motion
  alarms now also publish as `tns1:VideoSource/MotionAlarm` (Source =
  the camera UUID) while an NVR holds a pull-point subscription — the
  same accepted rising edge that feeds the GB alarm NOTIFY and the
  SPEC v1 §6 `alarm` SSE event. New `protocols.onvif.events_enabled`
  key (default `true`, absent DB rows parse as enabled; protocol
  re-toggle applies). The `alarm` SSE event is now advertised with AI
  active instead of requiring a running GB28181 protocol — each alarm
  channel no-ops independently until its consumer is up/subscribed.

## [0.3.0] - 2026-09-16

Late join to the v0.3.0 train (user decision): the version number aligns
with the raspi twins, whose synchronized v0.3.0 shipped the
GB/T 28181-2022 device-role full coverage. This release brings the
notebook to the same device surface and takes one observation seam
further.

### Added — GB28181 device-surface parity with the raspi twins (v0.3.0 train)

- **FrameMirror (DeviceConfig A.2.3.2.9)**: the platform's runtime mirror
  mode (0-3 per A.2.1.22) now flips every camera's frames — implemented
  as device-level atomic flags shared by all capture loops and
  XOR-composed with each camera's static `hflip`/`vflip` mount
  compensation (two same-axis mirrors cancel). Baked into everything
  downstream: encoder (RTSP/MSE/recordings/GB28181), snapshots and the
  JPEG tap (MJPEG passthrough correctly disabled while mirroring).
  BasicParam keeps the reject posture, matching the raspi twins.
- **SIP-Date drift observation (§9.10.2)**: the platform clock from
  REGISTER response `Date` headers is polled once a minute; drift beyond
  5s WARNs (first excursion and every further 5s move; recovery logs
  once). Observation only — the system clock is never adjusted by the
  app. Ahead of the raspi twins, which expose the library seam but do
  not observe it yet.

## [0.2.0] - 2026-09-16

### Changed — Brand rename to `mibee-eye` (family-wide packaging convention)

The product/binary/service name is now **`mibee-eye`**, aligning with the
MiBee Eye packaging convention (the Raspberry Pi Go twin ships the same
binary name). The repository stays `mibee-eye-notebook`.

- Cargo package and binary: `mibee-rec` → `mibee-eye`
- systemd unit: `mibee-rec.service` → `mibee-eye.service` (binary path
  `/usr/local/bin/mibee-eye`, `User=mibee-eye`, `StateDirectory=mibee-eye`)
- Default database file: `mibee_rec.db` → `mibee_eye.db` (clap default and
  the XDG default `~/.local/share/mibee-eye/mibee_eye.db`)
- Prometheus metric prefix: `mibee_rec_*` → `mibee_eye_*`
  (**breaking for dashboards** — update your queries)
- TLS dev certificate identity: `CN=mibee-rec` / `SAN=mibee-rec.local` →
  `CN=mibee-eye` / `SAN=mibee-eye.local`
- RTSP realm (`mibee-eye RTSP Server`) and `Server` header; RTSP SDP
  session name; GB28181 SIP `User-Agent: mibee-eye/<version>`; ONVIF
  default `device_name`; OpenTelemetry service name; capabilities
  `device.name`
- Docker: paths `/usr/local/share/mibee-eye`, `/var/lib/mibee-eye`, image
  user `mibee-eye`; container migration search path updated accordingly

**Migration steps:**
1. Rename the database before upgrading (stop the service first, WAL
   files included): `mv mibee_rec.db* mibee_eye.db*` — or keep the old
   path by passing `--db-path` explicitly.
2. Update Prometheus/Grafana queries referencing `mibee_rec_*` metrics.
3. Delete `tls/cert.pem` and `tls/key.pem` before restarting (new
   identity is regenerated automatically); old certificates keep working
   until then.
4. If using systemd: install `mibee-eye.service` and
   `systemctl disable --now mibee-rec.service` first.
5. If using Docker: update container name and volume paths from
   `mibee-rec` to `mibee-eye`.

## [0.1.0] - 2026-09-16

First public release (Apache-2.0), open-sourced as
[mibee-eye-notebook](https://github.com/xiqing85/mibee-eye-notebook) — the
notebook/desktop member of the MiBee Eye camera family. The sections below
cover the final stretch of the pre-open-source line, most recently the
unified SPEC v1 web API with the shared web UI and the GB28181-2022
device-surface parity batch.

### Added — GB28181-2022 device-surface parity with the raspi twins

- **Alarm pipeline**: AI detection rising edges fire the SPEC v1 §6
  `alarm` SSE event and a GB28181 Alarm NOTIFY (priority 4 / method 5 /
  type 2, 2022 standard table) through the gb28181-rs notifier seam;
  per-camera rising-edge cooldown (`alarm_cooldown_secs`, default 30)
  and runtime gate from the platform's DeviceConfig AlarmReport switches
  (`alarm_notify_enabled` initial value).
- **DeviceControl family**: IFrameCmd forces the next OpenH264-encoded
  frame to an IDR; RecordCmd gates local recording (the current MP4
  segment closes on pause, the next keyframe reopens); no-actuator
  commands (PTZ/Guard/TeleBoot/HomePosition/DragZoom) ack as no-ops.
- **Graceful deregistration**: SIGTERM / protocol stop sends REGISTER
  `Expires: 0` (library 401 dance, 2s timeouts) before teardown.
- **MobilePosition**: static coordinates
  (`position_longitude` / `position_latitude`, empty = off) reported on
  the subscription cadence.

### Changed — Unified Web API SPEC v1 + shared web UI

**Breaking** (web API): the REST surface now follows the MiBee camera
unified SPEC v1 (`mibee-webui/SPEC.md`), same contract as the Raspberry Pi
camera projects:

- All JSON responses enveloped: `{"ok":true,"data":…}` /
  `{"ok":false,"error":"<machine code>","message":…}` (middleware; binary
  endpoints and the legacy `/health` alias stay unwrapped)
- `GET/PUT /api/settings` and `GET/PUT /api/protocols/{name}` replaced by
  `GET/PUT /api/config` (partial deep-merge; protocol sections still
  hot-toggle ONVIF/GB28181 immediately). `/api/protocols/runtime-status`
  remains as a device extension
- New: `/api/health` (public enveloped), `/api/status`, SPEC capability
  superset on `/api/capabilities` (host probe kept as `system` /
  `recommended_profiles` extension fields)
- `POST /api/auth/setup` now establishes a session (signs the new admin in)

**Changed** (frontend): the Preact SPA is replaced by the shared
**mibee-webui** vanilla ES Modules build (same UI as the Pi cameras,
capability-driven). Removed: `crates/web/build.rs` (esbuild bundling),
`package.json`, `static/src/`, dead `static/app.js`, Dockerfile
node/npm/esbuild steps — a fresh clone now builds with cargo alone.

### Fixed — GB28181 PS-over-RTP interop

PS muxer added; Gb28181Output rewritten from RFC-6184 to PS-over-RTP (PT=96); SSRC parsing fixed to use `y=` SDP field instead of `a=ssrc:`; Digest auth default changed from SHA-256 to MD5; Contact header fixed to use device IP; INVITE handler now sends 200 OK with device SDP answer.

### Added — GB28181 Catalog/DeviceInfo/Keepalive MESSAGE handling

MANSCDP+xml module; inbound MESSAGE dispatcher; keepalive heartbeat loop; device SDP answer with echoed SSRC on INVITE 200 OK.


### Changed — ffmpeg dependency fully removed (native codec stack)

All five former `ffmpeg` subprocess invocations have been replaced with
in-process Rust crates. **`ffmpeg` is no longer a runtime dependency** —
the Docker image, systemd service, and bare-metal install instructions
no longer require it. This eliminates per-source subprocess overhead
(~64 MB/camera), removes the noisy stderr log spam, and makes the
encoding pipeline crash-recoverable without process supervision.

| Former ffmpeg role | Replacement | Crate / module |
|--------------------|-------------|----------------|
| H.264 video encode (V4L2 → libx264 → Annex B) | OpenH264 (Cisco BSD-2) | `openh264` via `streaming::encoder::h264` |
| MJPEG → pixels decode | `jpeg-decoder` | `streaming::encoder::convert` |
| YUYV → JPEG (preview re-encode) | `jpeg-encoder` | `streaming::encoder::convert` |
| AAC audio encode | G.711 μ-law (default) / FDK-AAC (`aac` feature) | `streaming::encoder::audio` |
| MP4 segment muxing | `muxide` (pure-Rust fMP4) | `streaming::output::file` |
| JPEG snapshot (RTSP → 1 frame) | direct cache read from capture loop | `web::routes::streams::snapshot` |
| MJPEG live preview (RTSP → transcode) | JPEG broadcast subscription | `web::routes::streams::live_preview` |

**Architecture changes:**
- `VideoCaptureSource` re-activates the previously-disconnected
  `capture::video::VideoCapture` (nokhwa) path — the camera is now read
  natively via V4L2 in-process, frames converted to YUV420p, and encoded
  to H.264 via OpenH264. The `v4l2-ctl` capability-detection subprocess
  is also gone (nokhwa's `compatible_camera_formats()` is used instead).
- A JPEG tap is maintained in the capture loop: for MJPG cameras the
  raw JPEG bytes are forwarded zero-cost; for YUYV-only cameras a JPEG
  is re-encoded every Nth frame. The snapshot and live-preview endpoints
  consume this tap directly — no RTSP loopback, no transcoding.
- The `MediaFrame::Video` NAL-per-frame contract is preserved (start
  code stripped, `data[0] & 0x1f` is the NAL type), so the downstream
  protocol stack (RTSP/RTMP/GB28181/ONVIF) required **zero changes**.
- `AudioCaptureSource` now encodes i16 PCM to G.711 μ-law in-process
  (pure Rust, reuses `protocols::audio_codec`). AAC encoding is gated
  behind the new `aac` cargo feature (`fdk-aac`) for RTMP/MP4 audio
  compliance — off by default.
- `FileOutput` now uses `muxide` to write rolling MP4 segments directly,
  with no subprocess. Segment rotation happens at keyframe boundaries.
- Profile/level: Baseline, low complexity, GOP = 1 s — mirrors the old
  `libx264 -preset ultrafast -tune zerolatency -g 30` baseline.

**Cross-compile / deploy:** added `scripts/docker-build.sh`,
`scripts/deploy.sh`, `scripts/service.sh`, and four test scripts
(`test-smoke.sh`, `test-perf.sh`, `test-features.sh`, `soak-report.sh`)
to cross-compile from Windows via Docker and deploy to the two test
devices over SSH. See `scripts/README.md`.

### Fixed — MJPEG live preview corruption + camera view re-entry crash
- **MJPEG live preview corruption resolved** (horizontal tearing, green blocks, mosaic artifacts):
  - Root cause 1: ffmpeg capture command missing `-pix_fmt yuv420p` — pixel format mismatch caused decoder corruption.
  - Root cause 2: `-tune zerolatency` enabled sliced threading, producing 8 slice NALs per frame. Each slice incremented the RTP timestamp → 8× too fast (24000/frame instead of 3000), causing complete frame misalignment.
  - Root cause 3: SPS/PPS re-sent before every P-slice (8× per frame) flooded the broadcast channel, causing IDR frame fragments to be dropped.
  - Fix: added `-pix_fmt yuv420p` + `-threads 1` to force single-slice frames; RTP timestamp now only increments for slice NALs (types 1/5) per RFC 6184 §5.1; broadcast channel capacity increased 64→300.
- **Camera view re-entry crash fixed** ("Cannot set properties of null"): `_k` re-entry guard in router `r()` and `K()` prevents hashchange-triggered double-entry; `setText()` null-safe helper wraps all `textContent` assignments.

### Fixed — production hardening (security + reliability)
- **Mutex poisoning eliminated**: `resource.rs` and `rtsp_server.rs` now use `parking_lot::Mutex` (non-poisoning). Eliminates crash-on-panic cascade.
- **CSRF bypass on MJPEG endpoint fixed**: `server.rs` CSRF middleware now correctly covers all state-changing routes.
- **Crypto RNG hardened**: `thread_rng()` replaced with `OsRng` in all cryptographic code paths (session tokens, CSRF tokens).

### Added — Authoritative product positioning
- New [`docs/POSITIONING.md`](docs/POSITIONING.md) (bilingual zh/en) — authoritative product scope, target users, deployment model, capture scope, protocol scope, recording scope, security posture, observability, platform roadmap, UX requirements. **Any conflict between POSITIONING.md and other docs → POSITIONING.md wins.**
- Engineering guidance docs rewritten: added Product Scope, Platform Support, UX Requirements, Observability Stack, Security Posture sections; removed stale Architecture Mismatch Notes (those refactors are already done); accurate Project Status reflecting what is wired vs unwired vs missing.
- README.md Protocol Support Status table replaced with accurate component-level table: Auth no longer marked as stub (fully implemented); only RTSP is wired into runtime; RTMP/ONVIF/GB28181 marked as code-complete-but-not-wired; browser preview, local recording, i18n, theme, CSRF/CSP, remote log shipping, Windows/macOS all marked missing.

### Decision log (locked-in by product owner)
- **GB/T 28181**: retained as first-class protocol (2033 LOC implementation preserved; surfaced as same-tier toggle as ONVIF/RTMP/RTSP in Web UI).
- **Browser live preview**: in scope for v1 (MSE or JPEG sequence; WebRTC explicitly deferred).
- **NVR integration**: both pull (RTSP server) AND push (RTMP/GB28181) supported, all default-OFF, individually toggled via Web UI.
- **Local recording**: MP4 segment archive, configurable segment duration + total capacity + auto-prune-oldest, per-stream toggle.
- **Web UI security**: strong password + session + rate-limit + CSRF + CSP + login-failure lockout + HTTPS-mandatory. 2FA / mTLS explicitly out of scope for v1.
- **Platform**: Linux Tier 1 (only platform that compiles today); Windows v1.1, macOS v1.2 (planned, not blocking).

### Changed — Phase 1 P0 production blockers (5 of 6 resolved)
- **Hardcoded `localhost` eliminated**: new `[web] advertised_host` config option (empty = auto-detect LAN IP via UDP probe). All URL construction sites (RTSP base, snapshot, ONVIF XAddrs) now use the resolved host. Web UI no longer fabricates `rtsp://localhost:8554/...` fallbacks.
- **Mutex poisoning crash chain fixed**: `crates/security/src/rate_limit.rs` now uses `parking_lot::Mutex` (non-poisoning). `auth.rs` `SystemTime::now().unwrap()` calls replaced with `safe_epoch_secs()` helper that falls back to UNIX_EPOCH + logs error.
- **Rate limit now resets on successful login**: `login_handler` extracts client IP (X-Forwarded-For → X-Real-IP → fallback) and calls `reset_rate_limit` after bcrypt verify, so legitimate users don't get locked out after one typo.
- **Protocol configs persisted to SQLite**: new migration `002_protocol_configs.sql`. `routes/protocols.rs` rewritten to read/write DB (was in-memory `HashMap`, lost on restart). `main.rs` seeds DB from `config.toml` on first run only. Subsequent runs use persisted values — Web UI config changes now survive restart.
- **Protocol config type validation**: PUT handlers schema-validate per-protocol (onvif/gb28181/rtmp_push). Numeric fields (`port`, `register_interval_secs`, etc.) and booleans (`enabled`) sent as strings by legacy frontends are coerced. Unknown fields rejected with descriptive error.
- **RTMP push wired into StreamManager**: `StreamManager::create_stream` now reads `rtmp_push` config from DB; when `enabled=true` and `push_url` non-empty, auto-attaches `RtmpOutput` to the hub. `StreamInfo` exposes `rtmp_url` field. Toggle takes effect on next stream start (no server restart needed).
- **ONVIF + GB28181 startup reads DB**: `main.rs` now checks the `enabled` flag from SQLite (source of truth after first run) instead of `config.toml` only. Web UI toggles for these protocols now take effect on next server restart.

### Resolved — GB28181 RTP push + BYE cleanup + tracing instrumentation
- **GB28181 RTP push fully wired**: `Gb28181Output` is dynamically attached to the camera's `StreamHub` via `add_output_at_runtime` when a SIP INVITE arrives, and detached via `remove_output_from_stream` when BYE is received. The `HubHandle::remove_output` API was added to stop the output task and clean up. The `add_output_to_stream` race condition (take/replace pattern) was fixed by using `as_ref()` with a read lock.
- **OpenTelemetry span instrumentation added**: 132 `#[tracing::instrument]` annotations across all Axum HTTP handlers, StreamHub, StreamManager, capture pipeline (VideoCaptureSource/AudioCaptureSource), and protocol handlers (RTSP server, RTMP push, GB28181 SIP, ONVIF WS-Discovery/SOAP). The OTLP export pipeline now produces real trace data.

### Added — protocol hot-toggle, hot-plug, SSE, i18n, recording, and more
- **Protocol hot-toggle via Web UI**: `ProtocolRuntime` (`crates/web/src/protocol_runtime.rs`) starts/stops ONVIF, GB28181, and RTMP at runtime in response to Web UI toggles — **without server restart**. State persists across restarts via SQLite.
- **Hot-plug camera monitor**: udev netlink ADD/REMOVE listener auto-discovers plugged cameras and marks unplugged cameras offline (flushing their FileOutput gracefully).
- **SSE event bus**: `GET /api/events` (Server-Sent Events) pushes real-time camera add/offline events to the browser.
- **Snapshot endpoint**: `GET /api/cameras/{id}/snapshot` fully implemented — captures a JPEG frame via ffmpeg.
- **Browser live preview**: MJPEG multipart stream at `GET /api/cameras/{id}/live` — working end-to-end with single-slice frame fix.
- **i18n (zh-CN / en-US)**: full `t()` translation dictionary in `app.js`, language toggle persisted to user settings.
- **Day/night theme**: system-preference auto-detect on first run, manual toggle persisted.
- **Local recording**: `FileOutput` MP4 segment archive with configurable duration + capacity + auto-prune-oldest, per-stream toggle.
- **Remote log shipping**: optional `tracing-loki` layer (fail-open) via `[observability.logs]` config.
- **SQLite hybrid access**: sqlx pool (`web::db::init_pool`) for web CRUD + rusqlite (`init_auth_db`) for auth.
- **`advertised_host` config**: auto-detect LAN IP via UDP probe, no more hardcoded `localhost` in URLs.
- **W3C TraceContext propagation**: `traceparent` header extraction (incoming, Axum middleware) + injection (outbound HTTP requests).
- **esbuild build pipeline**: frontend bundled via esbuild for faster, smaller output.
- **4th migration** (`004__add_offline_since`): tracks when cameras went offline.

### Known gaps remaining
- **Windows / macOS compilation**: Do not compile yet (planned Tier 2). Blockers: `libc::getifaddrs` POSIX-only, `#[cfg(unix)]` without Windows fallback, hardcoded `/dev/videoN` paths.

### Changed — Brand rename
- **Rebranded from `notebook-cam` to `mibee-rec`** across all source, config, docs, and UI.
- Prometheus metric prefix changed: `notebook_cam_*` → `mibee_rec_*` (BREAKING for dashboards — update your Grafana queries).
- TLS dev certificate identity changed: `CN=notebook-cam` / `SAN=notebook-cam.local` → `CN=mibee-rec` / `SAN=mibee-rec.local` (delete old `tls/cert.pem` and `tls/key.pem` to regenerate).
- systemd service file renamed: `notebook-cam.service` → `mibee-rec.service`.
- Default `onvif.device_name`: `notebook-cam` → `mibee-rec`.
- RTSP Server realm and Server header: `notebook-cam` → `mibee-rec`.
- SIP User-Agent (GB28181): `notebook-cam/0.1` → `mibee-rec/0.1`.
- OpenTelemetry service name: `notebook-cam` → `mibee-rec`.

### Migration Steps
1. Update any Prometheus/Grafana queries referencing `notebook_cam_*` metrics.
2. Delete `tls/cert.pem` and `tls/key.pem` before restarting (new certs generated automatically).
3. If using systemd: `systemctl disable notebook-cam.service`, install `mibee-rec.service`, `systemctl enable --now mibee-rec.service`.
4. If using Docker: update container name and volume paths from `notebook-cam` to `mibee-rec`.
