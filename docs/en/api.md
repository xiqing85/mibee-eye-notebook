# API Reference

## Overview

The mibee-eye REST API follows the **MiBee Camera Web API unified SPEC v1**
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
| GET | `/api/capabilities` | SPEC superset (`multi_camera`, `camera_management`, `camera_control`, `devices`, `mjpeg`, `mse`, `events`, `config_apply`, …) plus the device-intelligence booleans `ai` / `zones` / `audio_ai` / `voice` / `chat` / `vlm` / `ocr` / `substream`, the host hardware probe extension fields `system` and `recommended_profiles`, and `events` extended with the capability-gated names |

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
| GET | `/api/cameras/{id}/stream.sub.mse` | Low-resolution substream (per-camera `config.substream`; advertised via `capabilities.substream`) |
| GET | `/api/cameras/{id}/zones` | Saved zones: `{"zones":[{name, kind:"intrusion"\|"line_cross", points:[[x,y]…], dwell_secs}], "applied":"immediate"}` (capability `zones`) |
| PUT | `/api/cameras/{id}/zones` | Replace the zone list — body is a **bare array** of zones; structural validation (intrusion ≥ 3 points, line exactly 2) |

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
| GET | `/api/events` | SSE stream (`text/event-stream`, 15 s keepalive). Events: `camera_added`, `camera_offlined`, `ai_detection`, `ai_model_changed`, `alarm` (SPEC §6: `camera_id`, `active: true`, `source: "ai"` or `"audio"`, `targets` / `class` + `score`, `timestamp` epoch-ms), `zone_event` (`{camera_id, zone, event, track_id, label, timestamp}`), `voice_transcript` (`{keyword, transcript, speaker, timestamp}` — `speaker` is the best-matching enrolled voiceprint, "" when unknown), `chat_reply` (`{source, reply, timestamp}`), `voice_decision` (`{camera_id, transcript, choice, confidence, act_probability, timestamp}` — the intent decision over a voice transcript), `alarm_description` (`{camera_id, alarm_timestamp, description, elapsed_s}`), `meeting_state` (`{camera_id:"all", meeting_id, status:"recording"\|"processing"\|"done"\|"failed", timestamp}` — meeting lifecycle, SPEC appendix A #27). Each capability-gated event is only emitted while its engine is active. |

### On-device intelligence (device extension)

All endpoints below are capability-gated: the engine answers honestly when
inactive (`{"enabled": false}` shapes / `not_implemented`-class errors)
instead of pretending.

| Method | Path | Description |
|--------|------|-------------|
| POST | `/api/chat` | Local LLM dialogue: `{"text","history":[{role,content}]}` → `{"reply"}` (capability `chat`; voice-loop replies also arrive as `chat_reply` SSE) |
| GET | `/api/audio/records` | Hearing records (capability `audio_records`): `{"records":[{id, kind:"sound"\|"voice", text, score, keyword, speaker, timestamp_ms}]}`, newest first; `?limit=N` (default 100, max 500), `?kind=sound\|voice` |
| DELETE | `/api/audio/records` | Clear every record → `{"applied":"immediate","removed":N}` |
| GET | `/api/voice/speakers` | Voiceprint profiles (capability `voice_speakers`, **no side effects**) → `{"speakers":[{id,name,dim,count,created_at}], "enrollment":{name,collected,needed}\|null, "capable":bool}` |
| POST | `/api/voice/speakers` | Begin enrollment: body `{"name", "utterances"?(default 3, 1..=10)}` — the next `utterances` wake words each collect one embedding sample (poll GET for progress); already-enrolled name → 400 |
| POST | `/api/voice/speakers/commit` | Persist a completed session → `{"enrolled","samples","dim"}`; incomplete → 400 |
| POST | `/api/voice/speakers/cancel` | Abandon the in-flight enrollment |
| DELETE | `/api/voice/speakers/{name}` | Delete a profile (memory + DB; unknown name → 404) |
| POST | `/api/ocr` | Body = raw JPEG bytes → `{"items":[{text, score, bbox}]}` (capability `ocr`) |
| POST | `/api/meetings/start` | Start a meeting recording (capability `meeting`) → `201 {"id","started_at_ms"}`; already recording → 409; engine inactive → 501. Emits `meeting_state` SSE `{camera_id:"all", meeting_id, status:"recording", timestamp}` |
| POST | `/api/meetings/{id}/stop` | Stop and trigger the **async** pipeline (diarize → transcribe → punctuate → name → persist) → `{"id","status":"processing"}`; id not the running session → 409; completion arrives via `meeting_state` SSE (`done`/`failed`) |
| GET | `/api/meetings` | Meeting list (newest first) → `{"meetings":[{id, started_at_ms, ended_at_ms, duration_ms, status:"recording"\|"processing"\|"done"\|"failed", num_speakers, num_segments, audio_path, error}]}` |
| GET | `/api/meetings/{id}` | Meeting detail → `{"meeting":{…}, "segments":[{start_ms, end_ms, speaker_index, speaker, text}]}` (ordered by start_ms; `speaker` is the matched voiceprint name, "" when anonymous — render "Speaker N" from `speaker_index`) |
| DELETE | `/api/meetings/{id}` | Delete the meeting, its segments (and any retained audio); unknown id → 404 |
| GET/PUT | `/api/cameras/{id}/zones` | See the Cameras table above |
| GET | `/api/ai/models` | Detection model store: available/active models |
| POST | `/api/ai/models/{id}/activate` | Activate an uploaded model |
| POST | `/api/ai/models` | Upload a model archive |
| DELETE | `/api/ai/models/{id}` | Delete an uploaded model |

### Conversation traces (SPEC §3.3)

Per-conversation model call-chain records: every model invoked on a
dialogue's answer path (decision triage, VLM, cloud LLM, local LLM, TTS)
produces a span with call order, duration, process-CPU delta and token
counts. The chain is also exported over OTLP when `otel_endpoint` is
configured.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/traces/conversations?limit=` | Recent conversation summaries (default 50, cap 200), newest first |
| GET | `/api/traces/conversations/{id}` | Full span list for one conversation; unknown id → 404 |

List item: `{"id","origin":"chat"|"voice","started_at_ms","duration_ms","turns","models":[...],"status":"ok"|"partial"|"error","open"}`.
Span object: `{"span_id","parent_id","model","variant","label","start_ms","duration_ms","cpu_ms","status","tokens_prompt","tokens_completion","attributes"}`.

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
