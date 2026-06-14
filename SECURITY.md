# Security Policy

## Supported Versions

MiBee-Rec is in active development. Security fixes are applied to the latest `main` branch.

## Reporting a Vulnerability

If you discover a security vulnerability, please report it responsibly:

1. **DO NOT** open a public GitHub issue.
2. Email: `security@github.com` (replace with actual contact if different — check repo for clues).
3. Include: description, reproduction steps, affected versions, potential impact.
4. You will receive an acknowledgment within 72 hours.

## Security Features

MiBee-Rec is designed with security first:

- **TLS everywhere**: All web UI and API traffic is HTTPS (rustls).
- **Authentication**: Session-based auth with bcrypt password hashing.
- **Cookie security**: `HttpOnly`, `Secure`, `SameSite=Strict`.
- **Rate limiting**: Per-IP throttling on authentication endpoints.
- **HSTS**: `Strict-Transport-Security` header enforced.
- **No anonymous access**: All stream and control surfaces require authentication.

## Hardening Checklist (Production)

- [ ] Replace self-signed TLS certificate with CA-signed certificate.
- [ ] Configure reverse proxy (nginx/caddy) with HSTS and certificate management.
- [ ] Set strong admin password (≥12 characters).
- [ ] Restrict network exposure (firewall, VPN, or reverse proxy ACLs).
- [ ] Monitor `/metrics` endpoint for anomalies.
