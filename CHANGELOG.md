# Changelog

All notable changes to MiBee-Rec are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added — Authoritative product positioning
- New [`docs/POSITIONING.md`](docs/POSITIONING.md) (bilingual zh/en) — authoritative product scope, target users, deployment model, capture scope, protocol scope, recording scope, security posture, observability, platform roadmap, UX requirements. **Any conflict between POSITIONING.md and other docs → POSITIONING.md wins.**
- `AGENTS.md` rewritten: added Product Scope, Platform Support, UX Requirements, Observability Stack, Security Posture sections; removed stale Architecture Mismatch Notes (those refactors are already done); accurate Project Status reflecting what is wired vs unwired vs missing.
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
- **OpenTelemetry span instrumentation added**: 123 `#[tracing::instrument]` annotations across all Axum HTTP handlers, StreamHub, StreamManager, capture pipeline (VideoCaptureSource/AudioCaptureSource), and protocol handlers (RTSP server, RTMP push, GB28181 SIP, ONVIF WS-Discovery/SOAP). The OTLP export pipeline now produces real trace data.

### Known gaps remaining
- **Hot-toggle of ONVIF/GB28181 without restart**: currently requires server restart. Hot-enable/disable requires protocol lifecycle management (start/stop WS-Discovery and SIP registration in response to Web UI toggle). Targeted for later phase.
- **Frontend type coercion in `app.js`**: backend safely handles legacy string-typed config values via schema coercion, so this is non-blocking. Frontend will be cleaned up when `app.js` is rewritten for i18n + theme + live preview.
- **W3C traceparent propagation**: tracing spans exist within the process but are not propagated across HTTP boundaries (no `traceparent` header injection/extraction in middleware). Targeted for next phase.
- **Browser live preview**: no `<video>` element or MSE/JPEG pipeline yet. UI shows static placeholder.
- **i18n + day/night theme**: UI is hardcoded English, dark-only.

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
