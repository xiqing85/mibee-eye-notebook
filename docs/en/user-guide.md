# User Guide

Everything you need to *use* a running mibee-eye device: the web interface,
the AI features (detection, sound events, zones, voice interaction, chat,
alarm descriptions), and what to do when something does not behave. For
setup and building, see [Installation](installation.md); for every
configuration key, see [Configuration](configuration.md).

## Contents

- [The capability model](#the-capability-model)
- [First login](#first-login)
- [The web interface](#the-web-interface)
  - [Live view](#live-view)
  - [Zones editor](#zones-editor)
  - [Chat panel](#chat-panel)
  - [Cameras / Devices / Status / Settings](#cameras--devices--status--settings)
- [Alarms and notifications](#alarms-and-notifications)
- [Talking to the device (voice)](#talking-to-the-device-voice)
- [Enabling the AI features](#enabling-the-ai-features)
- [Model assets](#model-assets)
- [Verifying a feature offline (self-tests)](#verifying-a-feature-offline-self-tests)
- [Troubleshooting](#troubleshooting)

## The capability model

mibee-eye is a local capture agent with optional on-device intelligence.
Every smart feature is **fail-open**: if its model files are missing, its
build lacks the cargo feature, or the host cannot run it, the device simply
does not advertise the capability — the rest of the product is unaffected
and the UI hides the corresponding controls. Nothing ever shows fake data.

The device publishes its live capability set at `GET /api/capabilities`; the
web UI reads it on load and renders exactly what this device can do:

| Capability | Meaning |
|------------|---------|
| `ai` | Visual object detection (NanoDet) — detection overlay, `/api/detections`, `ai_detection` events |
| `zones` | User-drawn intrusion/tripwire zones with tracking-based `zone_event`s (requires `ai`) |
| `audio_ai` | Sound-event detection (YAMNet) — `alarm` events with `source: "audio"` |
| `audio_records` | Persistent hearing records — the **Records** view and `GET /api/audio/records` |
| `voice` | Wake word + offline speech-to-text — `voice_transcript` events |
| `chat` | Local LLM dialogue — the chat panel and `POST /api/chat` |
| `vlm` | Alarm-frame image descriptions — `alarm_description` events |
| `ocr` | On-device text recognition — `POST /api/ocr` |
| `substream` | A low-resolution secondary stream is available on at least one running camera |

A capability you do not see is a feature that is off, not broken. [Enabling
the AI features](#enabling-the-ai-features) explains how to turn each one on.

## First login

1. Open `https://<device-address>:8443` in a browser. The TLS certificate is
   self-signed on first boot, so the browser shows a warning — accept it
   (Advanced → Proceed) or install your own certificate (see
   [Configuration](configuration.md#tls-certificate-management)).
2. On the very first visit the device answers that it needs setup: choose an
   admin username and password. This account is stored on the device.
3. Afterwards the login page accepts that account. Leaving the username
   field empty logs in as `admin` (single-user convenience).

## The web interface

The top bar has five views — **Live**, **Cameras**, **Settings**, **Status**,
**Devices** — plus the language (zh/en) and theme (day/night) toggles on the
right. All views react to the capability set.

### Live view

The live view shows the selected camera's stream with everything overlaid on
top of the video:

- **Detection overlay** (capability `ai`): green boxes with the object class
  and score, redrawn on every detection.
- **Zone overlay** (capability `zones`): your saved zones are drawn over the
  picture; a zone that fires lights up while the event is active.
- **Stream toolbar**: start/stop, snapshot, stream quality (main/sub — sub is
  the bandwidth-saving low-resolution stream), rotation, and the **zones
  editor** button. With several cameras, each tile carries its own controls.

### Zones editor

Zones are user-drawn regions the AI watches. Two kinds exist:

- **Intrusion** (`intrusion`) — a polygon (≥ 3 points). Alarms when a tracked
  object enters the region and stays for the dwell time.
- **Tripwire** (`line_cross`) — a line segment (exactly 2 points). Alarms
  when a tracked object crosses it; the event says which direction.

To draw one:

1. In the live view, click the **zones** button in the stream toolbar.
2. The current video frame is frozen as a backdrop. Click points on it to
   form a polygon (intrusion) or two points (tripwire). *Undo* removes the
   last point, *Clear* removes all.
3. Name the zone, pick its kind, and set the dwell seconds (intrusion only).
4. **Save** writes the zone list for this camera — it applies immediately,
   no restart.

Zones need the `ai` capability (tracking rides on the detector); zone
crossings arrive as `zone_event` SSE events and as alarm toasts.

### Chat panel

When the device advertises `chat`, a round **chat button** floats at the
bottom-right corner. Open it, type a message, and the on-device LLM answers
in the panel. The panel keeps your recent turns so you can ask follow-ups.
Replies to voice interactions arrive in the same panel (or as a toast, if it
is closed) — see [Talking to the device](#talking-to-the-device-voice).

The LLM runs **on the device** (llama.cpp + Qwen3); nothing is sent to any
cloud service.

### Cameras / Devices / Status / Settings

- **Cameras** — add, edit, start/stop and remove capture devices; per-camera
  configuration (resolution, FPS, substream, rotation, watermark…) lives
  here. Additions take effect without restarting the service.
- **Devices** — enumerate the machine's V4L2 video and ALSA audio devices,
  with the formats each video device supports. Use it to find the right
  device index before adding a camera or microphone.
- **Status** — device identity (name, model, firmware, uptime) and protocol
  runtime state (ONVIF / GB28181 running or not).
- **Settings** — the unified configuration editor: protocol sections
  (ONVIF, GB28181, RTMP push, recording, watermark) apply immediately on
  save; UI preferences (language, theme) persist per browser.

## Alarms and notifications

All alarms ride one SSE channel (`/api/events`) and appear as toasts in the
UI. A visual alarm carries the object class and score; a sound alarm carries
the sound class (e.g. `Dog`, `Glass`, `Smoke detector`); a zone alarm names
the zone and the crossing direction.

With the `vlm` capability enabled, every visual alarm also triggers a
one-sentence **description of the triggering frame** ("what happened"),
generated on-device by a vision-language model. The alarm itself is never
delayed by it — the description arrives seconds later as its own
notification, attributed to the alarm it belongs to.

If GB28181 is enabled and a platform subscribed, the same accepted alarms are
also forwarded as GB Alarm NOTIFY messages (method 5 / type 2 / priority 4).

### Hearing records

Everything the hearing surface recognizes is also written to a **persistent
text record** (capability `audio_records`): every fired sound event (its
class name and score) and every voice interaction (the wake word and the
full transcript). Open the **Records** view in the web UI to browse them —
newest first, filterable by kind, with a clear-all button. Programs can read
the same log via `GET /api/audio/records` (and wipe it with `DELETE`). The
log keeps the most recent 1000 entries; records survive restarts.

Sound-event detection and voice listening only run when you explicitly
enable them in the configuration — the microphone is privacy-sensitive
input and **every listening feature is opt-in**.

## Talking to the device (voice)

With the `voice` capability enabled the device keeps a wake-word listener on
the microphone (nothing is recorded or transmitted while it waits — the
keyword model matches tiny audio fingerprints locally):

1. Say the wake word — **小蜜蜂** ("little bee") by default.
2. The device captures the next few seconds of audio (4 s by default).
3. The captured audio is transcribed **offline** (paraformer, Chinese).
4. The transcript appears as a `voice_transcript` notification. If the `chat`
   capability is also on, the text is answered by the local LLM; with TTS
   configured the reply is spoken through the speakers as well.

Tips:

- Speak *after* the wake word; the capture window starts when the word is
  recognized. Keep the utterance within the capture window (4 s).
- If wake words are never recognized, check the microphone input device and
  level first (see [Troubleshooting](#troubleshooting)).
- The wake-word sensitivity and the keyword list are configuration keys
  (`keywords_threshold`, `keywords_file`).

## Enabling the AI features

All AI engines ship **disabled**. They are configured in `config.local.toml`
(next to the binary or at the path given with `--config`) and require a
service restart; each then appears in `/api/capabilities` automatically.

```toml
[audio_ai]        # sound events (YAMNet) — source: "audio" alarms
enabled = true
classes = ["Dog", "Bark", "Baby cry, infant cry", "Glass", "Siren"]

[voice]           # wake word + offline ASR (voice-feature build)
enabled = true

[llm]             # local LLM chat (llm-feature build)
enabled = true

[tts]             # spoken replies (sherpa-onnx CLI + vits-melo)
enabled = true

[vlm]             # alarm-frame descriptions (llm-feature build)
enabled = true

[ocr]             # text recognition (POST /api/ocr)
enabled = true
```

Feature builds: `voice` needs the `voice` cargo feature (sherpa-onnx static
libraries at build time); `llm`/`vlm` need the `llm` feature and an
**AVX2-class CPU** (any roughly-2013-or-newer x86-64; Sandy Bridge and older
must stay voice-only). Every engine degrades to "off" if its model files are
missing — see [Model assets](#model-assets).

The substream and zones need no TOML: the substream is a per-camera config
key set from the web UI (`config.substream`), and zones are drawn in the UI.

## Model assets

Model files are **not** tracked in git and are downloaded at deploy time
into `models/`. [`models/README.md`](../../models/README.md) is the
authoritative table (sources, sizes, licenses). In short:

| Path | Model | Size |
|------|-------|------|
| `models/nanodet-m.onnx` | visual detection (pre-existing) | ~10 MB |
| `models/audio/yamnet.onnx` + `silero_vad.onnx` | sound events + voice presence | ~18 MB |
| `models/voice/kws/`, `models/voice/paraformer/` | wake word + ASR (sherpa-onnx) | ~250 MB |
| `models/voice/melo/` | TTS voice (vits-melo zh_en) | ~165 MB |
| `models/llm/qwen3-0.6b-q8_0.gguf` | chat LLM | ~640 MB |
| `models/vlm/qwen3-vl-2b-instruct-q4_k_m.gguf` + `mmproj-…gguf` | alarm descriptions | ~1.5 GB |
| `models/ocr/*.onnx` | PP-OCR detector + recognizer | ~21 MB |

Missing files never break the service — the corresponding engine just stays
off.

## Verifying a feature offline (self-tests)

Each engine has a deterministic CLI self-test that runs the full pipeline
once against a local file and prints a JSON result — use these to confirm a
deployment before blaming the service:

```bash
mibee-eye --selftest-audio clip.wav      # sound events: classify a WAV
mibee-eye --selftest-voice sample.wav    # voice: wake word + transcription
mibee-eye --selftest-llm "你好"          # chat: one LLM completion
mibee-eye --selftest-tts "你好，世界"     # TTS: synthesize + play
mibee-eye --selftest-vlm frame.jpg       # VLM: describe one JPEG
mibee-eye --selftest-ocr page.jpg        # OCR: recognize text
```

A self-test passes only when the model loaded and produced sane output; it
fails with the engine's error when files or features are missing.

## Troubleshooting

| Symptom | Likely cause / fix |
|---------|--------------------|
| A feature's controls never appear in the UI | Its capability is off. Check `GET /api/capabilities`; then config `enabled`, the cargo feature the binary was built with, and the model files. |
| Wake word never recognized | Check the input device (`arecord -l`, the **Devices** view) and capture level. Run `--selftest-voice sample.wav` with a recording of the wake word. External USB microphones are far more sensitive than laptop built-ins. |
| TTS is silent | `--selftest-tts "test"` — checks the sherpa-onnx binary, the voice assets and `aplay`. Make sure an output device exists and its volume is up. |
| Visual alarm fires but no description arrives | `vlm` capability off (model missing, config off, or CPU lacks AVX2). Descriptions are also throttled: at most one runs at a time and back-to-back alarms reuse the interval floor instead of stacking. |
| Zone events never fire | Zones require `ai` (tracking). Check that detection boxes appear at all, that the camera is running, and that the zone really intersects where objects move. |
| LLM reply takes very long | Small hosts are slow at GGUF inference; the default Qwen3-0.6B Q8_0 targets a few seconds per short reply on a modern laptop. Lower `max_tokens`, use a Q4_K_M quant, or move the LLM to a faster host. |
| Sound alarms fire constantly | Raise `audio_ai.threshold`, shorten the `classes` list, or increase `cooldown_secs`. |
| Records view shows nothing | Its capability needs an active audio engine (`audio_ai` or `voice`). Records also only accumulate while the engine runs — past moments without a running engine leave no trace. |
| `POST /api/chat` times out while the machine is loaded | Several heavy engines can contend for CPU. Inference threads are capped automatically (`OMP_NUM_THREADS` = cores/2, max 4); close other engines or upgrade the host. |

For everything else, start with the logs (`RUST_LOG=info` or the service
journal) — every engine logs exactly why it is inactive at startup.
