# API Reference

## Overview

The mibee-rec REST API provides programmatic access to camera management, streaming control, and system configuration. All API endpoints use TLS encryption and require proper authentication.

### Base URL
```
https://localhost:8443
```

### Content Type
All requests and responses use JSON:
- Request: `Content-Type: application/json`
- Response: `Content-Type: application/json`

**Note:** Binary responses (snapshot, live preview) use appropriate content types (e.g., `image/jpeg`, `multipart/x-mixed-replace`).

### Authentication
The API uses cookie-based session authentication. After successful login, the API returns a session token in the `Set-Cookie` header which must be included in subsequent requests:

```bash
Cookie: session=$SESSION_TOKEN
```

Additionally, a CSRF token is issued as a non-HttpOnly cookie and returned in the response body. All state-changing requests (POST/PUT/DELETE/PATCH) must include the CSRF token in the `X-CSRF-Token` header.

### TLS Requirement
All API communication requires TLS/SSL. The server generates a self-signed certificate on first run if none exists.

---

## Authentication

### First-Run Setup

**Endpoint:** `POST /api/auth/setup`

**Description:** Create the initial admin user. This endpoint is only accessible when no users exist (first run).

**Request Body:**
```json
{
  "username": "string",
  "password": "string"
}
```

**Requirements:**
- username cannot be empty
- password must be at least 8 characters

**Response (200 OK):**
```json
{
  "status": "ok"
}
```

**Response (400 Bad Request):**
```json
{
  "error": "already configured",
  "code": 400
}
```

**Example:**
```bash
curl -X POST https://localhost:8443/api/auth/setup \
  -H "Content-Type: application/json" \
  -d '{"username": "admin", "password": "securepass123"}'
```

---

### Login

**Endpoint:** `POST /api/auth/login`

**Description:** Authenticate and create a session. On success, sets a session cookie (HttpOnly) and a CSRF cookie (non-HttpOnly), and returns a JSON response with the CSRF token.

**Request Body:**
```json
{
  "username": "string",
  "password": "string"
}
```

**Response (200 OK):**
- **Headers:**
  - `Set-Cookie: session=<token>; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=86400`
  - `Set-Cookie: csrf-token=<token>; SameSite=Strict; Path=/; Max-Age=86400`
- **Body:**
```json
{
  "status": "ok",
  "csrf_token": "<token>"
}
```

**Response (401 Unauthorized):**
```json
{
  "error": "invalid credentials"
}
```

**Response (429 Too Many Requests):**
```json
{
  "error": "account locked, try again in N seconds"
}
```

**Example:**
```bash
curl -X POST https://localhost:8443/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username": "admin", "password": "securepass123"}'
```

---

### Logout

**Endpoint:** `POST /api/auth/logout`

**Description:** Invalidate the current session. Clears the session cookie. No authentication required (clears whatever session exists).

**Response (200 OK):**
- **Headers:**
  - `Set-Cookie: session=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0`
- **Body:**
```json
{
  "status": "ok"
}
```

**Example:**
```bash
curl -X POST https://localhost:8443/api/auth/logout
```

---

### Password Reset

**Endpoint:** `POST /api/auth/reset`

**Description:** Change your password. This requires authentication with a valid session token.

**Request Body:**
```json
{
  "old_password": "string",
  "new_password": "string"
}
```

**Response (200 OK):**
```json
{
  "status": "ok"
}
```

**Response (401 Unauthorized):**
```json
{
  "error": "incorrect password"
}
```

**Example:**
```bash
curl -X POST https://localhost:8443/api/auth/reset \
  -H "Content-Type: application/json" \
  -H "Cookie: session=$SESSION_TOKEN" \
  -d '{"old_password": "currentpass", "new_password": "newpass123"}'
```

---

## Public Endpoints

### Health Check

**Endpoint:** `GET /health`

**Description:** Server health status and uptime. Available before setup.

**Response (200 OK):**
```json
{
  "status": "ok",
  "uptime": 3600
}
```

**Example:**
```bash
curl -X GET https://localhost:8443/health
```

### Metrics

**Endpoint:** `GET /metrics`

**Description:** Prometheus metrics in text format. Available before setup.

**Response (200 OK):**
```
# HELP mibee_rec_system_seconds System uptime in seconds
# TYPE mibee_rec_system_seconds counter
mibee_rec_system_seconds 3600
```

**Example:**
```bash
curl -X GET https://localhost:8443/metrics
```

---

## Camera Endpoints

### List Cameras

**Endpoint:** `GET /api/cameras`

**Description:** Retrieve all configured cameras.

**Response (200 OK):**
```json
[
  {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "name": "Front Door",
    "camera_type": "usb",
    "config": {
      "device_index": 0
    },
    "status": "stopped",
    "created_at": "1672531200",
    "updated_at": "1672531200",
    "rtsp_url": "rtsp://localhost:8554/live/550e8400-e29b-41d4-a716-446655440000"
  }
]
```

**Response (500 Internal Server Error):**
```json
{
  "error": "failed to list cameras",
  "code": 500
}
```

**Example:**
```bash
curl -X GET https://localhost:8443/api/cameras \
  -H "Cookie: session=$SESSION_TOKEN"
```

### Create Camera

**Endpoint:** `POST /api/cameras`

**Description:** Add a new camera configuration.

**Request Body:**
```json
{
  "name": "string",
  "camera_type": "string",
  "config": {
    "device_index": 0
  }
}
```

**Camera Types:**
- `usb` - Local USB webcam (primary type for this product)
- `rtsp` - RTSP streaming camera
- `onvif` - ONVIF network camera
- `gb28181` - GB/T 28181 compliant camera
- `rtmp` - RTMP ingest camera

**Note:** This product is designed for **local capture only** - it captures from physically-attached devices (USB webcams via V4L2). While the database allows other camera types, the primary use case is local webcam capture via the `usb` type.

**Response (201 Created):**
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "Front Door",
  "camera_type": "usb",
  "config": {
    "device_index": 0
  },
  "status": "stopped",
  "created_at": "1672531200",
  "updated_at": "1672531200"
}
```

**Response (400 Bad Request):**
```json
{
  "error": "failed to create camera",
  "code": 400
}
```

**Example:**
```bash
curl -X POST https://localhost:8443/api/cameras \
  -H "Content-Type: application/json" \
  -H "Cookie: session=$SESSION_TOKEN" \
  -H "X-CSRF-Token: $CSRF_TOKEN" \
  -d '{
    "name": "Webcam",
    "camera_type": "usb",
    "config": {
      "device_index": 0
    }
  }'
```

### Get Camera

**Endpoint:** `GET /api/cameras/{id}`

**Description:** Retrieve a specific camera by ID.

**Response (200 OK):**
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "Front Door",
  "camera_type": "usb",
  "config": {
    "device_index": 0
  },
  "status": "running",
  "created_at": "1672531200",
  "updated_at": "1672534800",
  "rtsp_url": "rtsp://localhost:8554/live/550e8400-e29b-41d4-a716-446655440000"
}
```

**Response (404 Not Found):**
```json
{
  "error": "camera not found",
  "code": 404
}
```

**Response (500 Internal Server Error):**
```json
{
  "error": "failed to get camera",
  "code": 500
}
```

**Example:**
```bash
curl -X GET https://localhost:8443/api/cameras/550e8400-e29b-41d4-a716-446655440000 \
  -H "Cookie: session=$SESSION_TOKEN"
```

### Update Camera

**Endpoint:** `PUT /api/cameras/{id}`

**Description:** Update camera configuration. Partial updates are supported.

**Request Body:**
```json
{
  "name": "Updated Name",
  "camera_type": "string",
  "config": {
    "device_index": 1
  },
  "status": "running"
}
```

**Response (200 OK):**
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "Updated Name",
  "camera_type": "usb",
  "config": {
    "device_index": 1
  },
  "status": "running",
  "created_at": "1672531200",
  "updated_at": "1672534800"
}
```

**Response (404 Not Found):**
```json
{
  "error": "camera not found",
  "code": 404
}
```

**Example:**
```bash
curl -X PUT https://localhost:8443/api/cameras/550e8400-e29b-41d4-a716-446655440000 \
  -H "Content-Type: application/json" \
  -H "Cookie: session=$SESSION_TOKEN" \
  -H "X-CSRF-Token: $CSRF_TOKEN" \
  -d '{
    "name": "Updated Camera Name",
    "status": "stopped"
  }'
```

### Delete Camera

**Endpoint:** `DELETE /api/cameras/{id}`

**Description:** Remove a camera configuration.

**Response (200 OK):**
```json
{
  "status": "ok"
}
```

**Response (404 Not Found):**
```json
{
  "error": "camera not found",
  "code": 404
}
```

**Response (500 Internal Server Error):**
```json
{
  "error": "failed to delete camera",
  "code": 500
}
```

**Example:**
```bash
curl -X DELETE https://localhost:8443/api/cameras/550e8400-e29b-41d4-a716-446655440000 \
  -H "Cookie: session=$SESSION_TOKEN" \
  -H "X-CSRF-Token: $CSRF_TOKEN"
```

### Start Stream

**Endpoint:** `POST /api/cameras/{id}/start`

**Description:** Start capturing frames from a camera. Returns 409 if already running.

**Response (200 OK):**
```json
{
  "status": "running",
  "rtsp_url": "rtsp://localhost:8554/live/550e8400-e29b-41d4-a716-446655440000",
  "camera_id": "550e8400-e29b-41d4-a716-446655440000"
}
```

**Response (404 Not Found):**
```json
{
  "error": "camera not found",
  "code": 404
}
```

**Response (409 Conflict):**
```json
{
  "error": "stream already running",
  "code": 409
}
```

**Response (500 Internal Server Error):**
```json
{
  "error": "failed to start stream",
  "code": 500
}
```

**Example:**
```bash
curl -X POST https://localhost:8443/api/cameras/550e8400-e29b-41d4-a716-446655440000/start \
  -H "Cookie: session=$SESSION_TOKEN" \
  -H "X-CSRF-Token: $CSRF_TOKEN"
```

### Stop Stream

**Endpoint:** `POST /api/cameras/{id}/stop`

**Description:** Stop capturing frames from a camera.

**Response (200 OK):**
```json
{
  "status": "ok",
  "camera_id": "550e8400-e29b-41d4-a716-446655440000"
}
```

**Response (404 Not Found):**
```json
{
  "error": "camera not found",
  "code": 404
}
```

**Response (500 Internal Server Error):**
```json
{
  "error": "failed to stop stream",
  "code": 500
}
```

**Example:**
```bash
curl -X POST https://localhost:8443/api/cameras/550e8400-e29b-41d4-a716-446655440000/stop \
  -H "Cookie: session=$SESSION_TOKEN" \
  -H "X-CSRF-Token: $CSRF_TOKEN"
```

### Capture Snapshot

**Endpoint:** `GET /api/cameras/{id}/snapshot`

**Description:** Capture a single JPEG frame from a camera using ffmpeg. The camera stream must be active.

**Requirements:**
- Stream must be running for the camera (returns 409 otherwise)
- ffmpeg must be installed and available on PATH

**Response (200 OK):**
- **Headers:**
  - `Content-Type: image/jpeg`
  - `Content-Length: <size>`
- **Body:** JPEG binary image data

**Response (404 Not Found):**
Camera does not exist

**Response (409 Conflict):**
Stream is not active - start the stream first

**Response (504 Gateway Timeout):**
Capture took longer than 30 seconds

**Example:**
```bash
# Capture snapshot and save to file
curl -X GET https://localhost:8443/api/cameras/550e8400-e29b-41d4-a716-446655440000/snapshot \
  -H "Cookie: session=$SESSION_TOKEN" \
  -o snapshot.jpg
```

---

### Live Preview

**Endpoint:** `GET /api/cameras/{id}/live`

**Description:** Browser live preview stream using MJPEG multipart format. Suitable for direct use in `<img src=...>` tags. The camera stream must be active.

**Requirements:**
- Stream must be running for the camera (returns 409 otherwise)
- ffmpeg must be installed and available on PATH

**Response (200 OK):**
- **Headers:**
  - `Content-Type: multipart/x-mixed-replace; boundary=ffmpeg`
  - `Cache-Control: no-store, no-cache, must-revalidate`
- **Body:** Continuous MJPEG stream (JPEG frames separated by boundaries)

**Response (404 Not Found):**
Camera does not exist

**Response (409 Conflict):**
Stream is not active - start the stream first

**Example:**
```bash
# In HTML:
# <img src="/api/cameras/{id}/live">
#
# The browser automatically handles the MJPEG stream and displays live video.
```

---

## Settings Endpoints

### Get Settings

**Endpoint:** `GET /api/settings`

**Description:** Retrieve all system settings as key-value pairs.

**Response (200 OK):**
```json
{
  "theme": "dark",
  "language": "en-US",
  "max_concurrent_streams": "16"
}
```

**Response (500 Internal Server Error):**
```json
{
  "error": "failed to list settings",
  "code": 500
}
```

**Example:**
```bash
curl -X GET https://localhost:8443/api/settings \
  -H "Cookie: session=$SESSION_TOKEN"
```

### Update Settings

**Endpoint:** `PUT /api/settings`

**Description:** Update one or more system settings.

**Request Body:**
```json
{
  "settings": {
    "theme": "light",
    "language": "zh-CN",
    "max_concurrent_streams": "8"
  }
}
```

**Response (200 OK):**
```json
{
  "status": "ok"
}
```

**Response (500 Internal Server Error):**
```json
{
  "error": "failed to update settings",
  "code": 500
}
```

**Example:**
```bash
curl -X PUT https://localhost:8443/api/settings \
  -H "Content-Type: application/json" \
  -H "Cookie: session=$SESSION_TOKEN" \
  -H "X-CSRF-Token: $CSRF_TOKEN" \
  -d '{
    "settings": {
      "theme": "light",
      "language": "en-US"
    }
  }'
```

---

## Protocol Configuration Endpoints

### Get Protocol Config

**Endpoint:** `GET /api/protocols/{onvif|gb28181|rtmp}`

**Description:** Retrieve the current configuration for the specified protocol.

**Response (200 OK):**
```json
{
  "enabled": true,
  // ... protocol-specific fields
}
```

**Example:**
```bash
curl -X GET https://localhost:8443/api/protocols/onvif \
  -H "Cookie: session=$SESSION_TOKEN"
```

---

### Update Protocol Config

**Endpoint:** `PUT /api/protocols/{onvif|gb28181|rtmp}`

**Description:** Update the configuration for the specified protocol. Persists to SQLite and hot-toggles the protocol runtime based on the `enabled` flag (no server restart required).

**Request Body:**
```json
{
  "enabled": true,
  // ... protocol-specific fields
}
```

**Response (200 OK):**
```json
{
  "status": "ok"
}
```

**Example:**
```bash
curl -X PUT https://localhost:8443/api/protocols/onvif \
  -H "Content-Type: application/json" \
  -H "Cookie: session=$SESSION_TOKEN" \
  -H "X-CSRF-Token: $CSRF_TOKEN" \
  -d '{"enabled": true, "device_name": "FrontDoor"}'
```

---

### Get Protocol Runtime Status

**Endpoint:** `GET /api/protocols/runtime-status`

**Description:** Get the running/stopped status of all protocols.

**Response (200 OK):**
```json
{
  "onvif": "running",
  "gb28181": "stopped",
  "rtmp": "running"
}
```

**Example:**
```bash
curl -X GET https://localhost:8443/api/protocols/runtime-status \
  -H "Cookie: session=$SESSION_TOKEN"
```

---

## Device Enumeration Endpoints

### List Video Devices

**Endpoint:** `GET /api/devices/video`

**Description:** Enumerate all available local video capture devices (webcams) using V4L2.

**Response (200 OK):**
```json
[
  {
    "index": 0,
    "name": "/dev/video0",
    "formats": ["YUYV 640x480", "MJPEG 1280x720"]
  }
]
```

**Example:**
```bash
curl -X GET https://localhost:8443/api/devices/video \
  -H "Cookie: session=$SESSION_TOKEN"
```

---

### List Audio Devices

**Endpoint:** `GET /api/devices/audio`

**Description:** Enumerate all available local audio input devices using ALSA.

**Response (200 OK):**
```json
[
  {
    "name": "default",
    "supported_configs": [
      {
        "channels": 2,
        "min_sample_rate": 44100.0,
        "max_sample_rate": 48000.0,
        "sample_format": "S16LE"
      }
    ]
  }
]
```

**Example:**
```bash
curl -X GET https://localhost:8443/api/devices/audio \
  -H "Cookie: session=$SESSION_TOKEN"
```

---

## Server-Sent Events (SSE)

### Camera Events Stream

**Endpoint:** `GET /api/events`

**Description:** Server-Sent Events stream for real-time camera hot-plug events. Receives `camera_added` and `camera_offlined` events as cameras are plugged in or unplugged.

**Event Types:**
- `camera_added` - New camera discovered
- `camera_offlined` - Camera went offline (unplugged)

**Event Format:**
```
event: camera_added
data: {"camera_id":"...","device_index":0,"name":"..."}

event: camera_offlined
data: {"camera_id":"...","device_index":0}

```

**Example:**
```bash
# SSE endpoint returns continuous event stream
curl -N https://localhost:8443/api/events \
  -H "Cookie: session=$SESSION_TOKEN"
```

---

## Security Features

### CSRF Protection

All state-changing requests (POST, PUT, DELETE, PATCH) require a CSRF token to prevent Cross-Site Request Forgery attacks.

**Token Issuance:**
- On successful login, a CSRF token is issued as a non-HttpOnly cookie named `csrf-token`
- The same token is also returned in the JSON response body as `csrf_token`

**Token Usage:**
- Include the token in the `X-CSRF-Token` header on all POST/PUT/DELETE/PATCH requests
- The token value must match the `csrf-token` cookie value (double-submit pattern)

**Example:**
```bash
curl -X POST https://localhost:8443/api/cameras \
  -H "Content-Type: application/json" \
  -H "Cookie: session=$SESSION_TOKEN; csrf-token=<token>" \
  -H "X-CSRF-Token: <token>" \
  -d '{"name":"Test","camera_type":"usb"}'
```

**Failure:** Missing or mismatched token returns 403 Forbidden.

---

### Rate Limiting

Login endpoints are rate-limited to prevent brute force attacks.

**Default Limits:**
- 20 requests per 60 seconds per IP address on auth endpoints
- Rate limit counter resets on successful login

**Account Lockout:**
- After 5 failed login attempts, the account is locked
- Lockout duration doubles with each additional failure: 60s → 120s → 240s → ...
- Successful login resets the failure counter

**Rate Limit Response:**
```json
{
  "error": "account locked, try again in 60 seconds"
}
```

---

## Error Responses

All error responses follow a consistent format:

### 400 Bad Request
```json
{
  "error": "descriptive error message",
  "code": 400
}
```

### 401 Unauthorized
```json
{
  "error": "unauthorized"
}
```

### 403 Forbidden
```json
{
  "error": "CSRF token missing or invalid"
}
```

### 404 Not Found
```json
{
  "error": "camera not found",
  "code": 404
}
```

### 409 Conflict
```json
{
  "error": "stream already running",
  "code": 409
}
```

### 429 Too Many Requests
```json
{
  "error": "account locked, try again in N seconds"
}
```

### 503 Service Unavailable
```json
{
  "error": "SETUP_REQUIRED",
  "code": 503
}
```

### 504 Gateway Timeout
```json
{
  "error": "snapshot capture timed out"
}
```

### 500 Internal Server Error
```json
{
  "error": "descriptive error message",
  "code": 500
}
```

---

## Setup Flow

1. **First Access:** Server responds with 503 for most endpoints
2. **Setup:** Call `POST /api/auth/setup` to create admin user
3. **Access:** Login with `POST /api/auth/login` to get session token and CSRF token
4. **Configuration:** Add cameras, settings, and discover devices

## Example Session Flow

```bash
# 1. First run - setup admin user
curl -X POST https://localhost:8443/api/auth/setup \
  -H "Content-Type: application/json" \
  -d '{"username": "admin", "password": "securepass123"}'

# 2. Login to get session and CSRF tokens
LOGIN_RESPONSE=$(curl -X POST https://localhost:8443/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username": "admin", "password": "securepass123"}' \
  -c cookies.txt)

CSRF_TOKEN=$(echo $LOGIN_RESPONSE | jq -r '.csrf_token')

# 3. Create a camera (requires CSRF token)
curl -X POST https://localhost:8443/api/cameras \
  -H "Content-Type: application/json" \
  -b cookies.txt \
  -H "X-CSRF-Token: $CSRF_TOKEN" \
  -d '{"name": "Webcam", "camera_type": "usb", "config": {"device_index": 0}}'

# 4. Start streaming (requires CSRF token)
curl -X POST https://localhost:8443/api/cameras/<camera_id>/start \
  -b cookies.txt \
  -H "X-CSRF-Token: $CSRF_TOKEN"

# 5. Check settings
curl -X GET https://localhost:8443/api/settings \
  -b cookies.txt
```

---

## Session Management

- Session tokens expire after 24 hours
- All sessions are invalidated when password is reset
- Session cleanup happens automatically (every 5 minutes)
- Rate limiting resets on successful login