# mibee-rec — Build, Deploy & Test Scripts

Cross-compile from Windows (via Docker) and deploy to the two Linux test
devices over SSH. All scripts are Git-Bash-compatible on Windows and
POSIX-shell-compatible on Linux.

## Target devices

| Alias (`~/.ssh/config`) | IP | OS | CPU | Role |
|---|---|---|---|---|
| `device-1` | `192.168.1.41` | Pop!_OS 24.04 | i5-1135G7 (8C) | Device 1 — already runs mibee-rec as user systemd service |
| `device-2` | `192.168.1.40` | EndeavourOS (Arch) | i5-6200U (4C) | Device 2 — primary deploy target, weakest CPU |

## Workflow

```bash
# 1. Build (cross-compile inside Docker, extract to target/linux-x86_64/)
./scripts/docker-build.sh

# 2. Deploy to a device (copies binary + migrations + config)
./scripts/deploy.sh device-1
./scripts/deploy.sh device-2

# 3. Install/start the user-mode systemd service
./scripts/service.sh device-1 start
./scripts/service.sh device-2 start

# 4. Test
./scripts/test-smoke.sh device-1        # functional smoke test
./scripts/test-perf.sh device-1         # 60s CPU/memory benchmark
./scripts/test-features.sh device-1     # full feature matrix

# 5. Overnight soak — leave running, then review next day:
./scripts/soak-report.sh device-1 "8 hours ago"
```

## Script reference

| Script | Purpose |
|---|---|
| `docker-build.sh` | Cross-compile via the multi-stage Dockerfile; auto-starts Docker Desktop on Windows. Output: `target/linux-x86_64/{mibee-rec,config.toml,migrations/}`. Accepts `--no-cache`. |
| `deploy.sh <host>` | scp the built binary + migrations + config to `~/mibee-rec/` on the device. Atomic binary swap (`.new` → rename). Verifies camera device access + group membership. |
| `service.sh <host> <cmd>` | Manage the **user-mode** systemd service (`~/.config/systemd/user/mibee-rec.service`). Commands: `install`, `start`, `stop`, `status`, `logs`, `restart`, `disable`. Enables `loginctl linger` for boot-time auto-start. |
| `test-smoke.sh <host>` | Service active, `/health` 200, `/metrics` clean, RTSP DESCRIBE returns H.264 SDP, snapshot is JPEG, live-preview multipart works, **zero ffmpeg refs in journal** (removal regression guard). |
| `test-perf.sh <host>` | 60s benchmark: CPU% avg/peak, RSS avg/peak/growth, encoder counter. Compares against the old ~64 MB/ffmpeg baseline. |
| `test-features.sh <host>` | Hot-plug (udev trigger), MP4 recording + ffprobe validation, RTMP push, ONVIF WS-Discovery, GB28181 SIP REGISTER, audio path. Skips are non-fatal. |
| `soak-report.sh <host> [since]` | Post-overnight health review: panics, ERROR entries (OTel noise excluded), memory growth, encoder stalls, ffmpeg regression, recording pruning, CPU p99. |

## Notes

- **glibc forward-compat:** the builder stage is `rust:1.85-slim` (Debian
  bookworm, glibc 2.36). The resulting binary runs on Pop!_OS 24.04 (2.39)
  and Arch rolling (≥2.36) — newer glibc is always backward-compatible.
- **User-mode systemd:** the service runs as `your-user` (not root), reading
  `~/mibee-rec/config.local.toml`. `loginctl enable-linger` is needed for
  the service to survive logout / start at boot.
- **Camera access:** `your-user` must be in the `video` and `audio` groups.
  Run `sudo usermod -aG video,audio $USER` on the device and re-login if
  not (the deploy script checks and warns).
- **No more ffmpeg:** none of these scripts or the runtime require ffmpeg.
  The Dockerfile runtime stage no longer installs it.
