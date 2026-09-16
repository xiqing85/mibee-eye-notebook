# Security Policy

## Supported Versions

mibee-eye-notebook (binary name `mibee-eye`) is in active development. Security fixes are applied to the latest `main` branch.

## Reporting a Vulnerability

If you discover a security vulnerability, please report it responsibly:

1. **DO NOT** open a public GitHub issue.
2. Use [GitHub private vulnerability reporting](https://github.com/xiqing85/mibee-eye-notebook/security/advisories/new).
3. Include: description, reproduction steps, affected versions, potential impact.
4. You will receive an acknowledgment within 72 hours.

## Security Features

mibee-eye-notebook is designed with security first:

- **TLS everywhere**: All web UI and API traffic is HTTPS (rustls). Self-signed dev certs auto-generated; hot-reload on file change.
- **Authentication**: Session-based auth with bcrypt password hashing. 24h session cookies with 5-min cleanup task.
- **Cookie security**: `HttpOnly`, `Secure`, `SameSite=Strict`.
- **Rate limiting**: Per-IP fixed window throttling on authentication endpoints (default 20 requests / 60 seconds). Resets on successful login.
- **Login failure lockout**: Exponential backoff per-user after 5 failed attempts (60s → 120s → 240s → ...). Prevents brute-force attacks.
- **CSRF protection**: Double-submit cookie pattern. CSRF token issued on login as a non-HttpOnly cookie, verified via `X-CSRF-Token` header on every POST/PUT/DELETE/PATCH request.
- **CSP header**: Strict Content-Security-Policy (`default-src 'self'; script-src 'self' 'unsafe-inline'; ...`).
- **HSTS**: `Strict-Transport-Security` header enforced.
- **Body size limits**: 10KB on auth routes, 1MB default on other routes — prevents oversized payload attacks.
- **Crypto RNG hardened**: `OsRng` used in all cryptographic code paths (session tokens, CSRF tokens).
- **Non-poisoning mutexes**: `parking_lot::Mutex` used for shared request-handler state (rate limiter, login failure map) to prevent crash-on-panic cascade.
- **No anonymous access**: All stream and control surfaces require authentication. Only `/health` and `/metrics` are public.

## Hardening Checklist (Production)

- [ ] Replace self-signed TLS certificate with CA-signed certificate (`tls/cert.pem` + `tls/key.pem`).
- [ ] Configure reverse proxy (nginx/caddy) with HSTS and certificate management.
- [ ] Set strong admin password (≥12 characters).
- [ ] Restrict network exposure (firewall, VPN, or reverse proxy ACLs).
- [ ] Monitor `/metrics` endpoint for anomalies.
- [ ] Configure `[observability.logs]` for remote log shipping to a Loki-compatible backend.
- [ ] Review and tighten rate limiting and body size limits for your threat model.
