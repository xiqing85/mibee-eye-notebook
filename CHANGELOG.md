# Changelog

All notable changes to MiBee-Rec are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
