# API Reference

## Overview

The mibee-rec REST API follows the **MiBee Camera Web API unified SPEC v1**
(`mibee-webui/SPEC.md` in the workspace) — the same contract as the Raspberry
Pi camera projects. All API endpoints use TLS and require session
authentication unless noted.

### Base URL
```
https://localhost:8443
```

### Response envelope (SPEC §0)

All JSON endpoints respond with the unified envelope:

- Success: `{"ok": true, "data": …}`
- Failure: `{"ok": false, "error": "<machine code>", "message": "<human text>"}`
  with a semantic HTTP status. Machine codes: `bad_request`, `unauthorized`,
  `forbidden`, `not_found`, `conflict`, `rate_limited`, `not_implemented`,
  `internal_error`.

Binary endpoints (snapshot / MJPEG / MSE / metrics / static assets) and the
SSE stream are outside the envelope. The legacy `/health` endpoint is also
unwrapped; `/api/health` is the canonical enveloped path.

### Authentication (SPEC §2)

Cookie session + CSRF double-submit:

1. `POST /api/auth/login` `{"username","password"}` → sets
   `session=<token>; HttpOnly; Secure; SameSite=Strict` (24 h) and
   `csrf-token=<token>` (readable by JS) cookies.
2. Every state-changing request (POST/PUT/DELETE/PATCH) must echo the
   `csrf-token` cookie value in the `X-CSRF-Token` header
   (login/setup/logout exempt).
3. First boot: `GET /api/auth/me` answers `503 setup_required`;
   `POST /api/auth/setup` creates the admin and signs in.

Login is rate-limited per IP (20 req/min) with per-username exponential
lockout after repeated failures.

## Endpoints

### Public

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/health` | `{"ok":true,"data":{"status":"ok","uptime":N}}` |
| GET | `/health` | Legacy unwrapped alias |
| GET | `/metrics` | Prometheus text exposition |
| GET | `/`, `/style.css`, `/js/{path}` | Embedded web UI (shared mibee-webui build) |

### Auth

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/auth/me` | `{"username","role"}` / 401 / 503 setup_required |
| POST | `/api/auth/setup` | First-boot admin creation; establishes a session |
| POST | `/api/auth/login` | Session login (rate-limited) |
| POST | `/api/auth/logout` | 204, clears the session |
| POST | `/api/auth/reset` | `{"old_password","new_password"}`; invalidates all sessions |

### Device (SPEC §3)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/status` | device_name / model / vendor / firmware / uptime / cameras |
| GET | `/api/capabilities` | SPEC superset (`multi_camera`, `camera_management`, `camera_control`, `devices`, `mjpeg`, `mse`, `events`, `config_apply`, …) plus the host hardware probe extension fields `system` and `recommended_profiles` |

### Cameras (SPEC §4)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/cameras` | List cameras (enveloped array) |
| POST | `/api/cameras` | Create: `{"name","camera_type","config"}` → 201 |
| GET/PUT/DELETE | `/api/cameras/{id}` | Camera CRUD (partial update) |
| POST | `/api/cameras/{id}/start` / `stop` | Start/stop capture (409 when already running) |
| GET | `/api/cameras/{id}/snapshot` | JPEG snapshot |
| GET | `/api/cameras/{id}/live` | MJPEG multipart stream |
| GET | `/api/cameras/{id}/stream.mse` | Chunked fMP4 stream for MSE playback |

### Configuration (SPEC §5)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/config` | `{"settings": {…nested dotted keys…}, "protocols": {"onvif": …, "gb28181": …, "rtmp": …, "recording": …, "webrtc": …}}` |
| PUT | `/api/config` | Partial deep-merge of the document above; protocol sections hot-toggle ONVIF/GB28181 immediately (`config_apply.default = "immediate"`) |

This replaces the former `GET/PUT /api/settings` and the per-protocol
`/api/protocols/{name}` GET/PUT endpoints.

### Events (SPEC §6)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/events` | SSE stream (`text/event-stream`, 15 s keepalive). Events: `camera_added`, `camera_offlined`, `ai_detection`, `ai_model_changed`, `alarm` (SPEC §6: `camera_id`, `active: true`, `source: "ai"`, `targets`, `timestamp` epoch-ms) |

### Devices (SPEC §4.8)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/devices/video` | V4L2 video device enumeration |
| GET | `/api/devices/video/{index}/formats` | Supported capture formats |
| GET | `/api/devices/audio` | ALSA audio device enumeration |

### Protocol runtime (device extension)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/protocols/runtime-status` | `{"onvif":{"running":b},"gb28181":…,"rtmp":…}` |

## Web UI

The embedded frontend is the shared **mibee-webui** build (ES Modules,
zero build step) — the same UI as the Raspberry Pi camera projects,
rendered capability-driven. Source of truth: `mibee-webui/` in the
workspace (`make sync-notebook` copies it in).
