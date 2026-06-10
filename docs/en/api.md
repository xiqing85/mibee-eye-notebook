# API Reference

## Overview

The notebook-cam REST API provides programmatic access to camera management, streaming control, and system configuration. All API endpoints use TLS encryption and require proper authentication.

### Base URL
```
https://localhost:8443
```

### Content Type
All requests and responses use JSON:
- Request: `Content-Type: application/json`
- Response: `Content-Type: application/json`

### Authentication
The API uses cookie-based session authentication. After successful setup, the API returns a session token in the `Set-Cookie` header which must be included in subsequent requests:

```bash
Cookie: session=$SESSION_TOKEN
```

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

### Session Management

**Note:** Traditional login/logout endpoints (`POST /api/auth/login` and `/POST /api/auth/logout`) are not yet implemented and return 501. Use the password reset endpoint instead.

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
# HELP notebook_cam_system_seconds System uptime in seconds
# TYPE notebook_cam_system_seconds counter
notebook_cam_system_seconds 3600
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
    "camera_type": "rtsp",
    "config": {
      "url": "rtsp://192.168.1.100:554/stream1"
    },
    "status": "stopped",
    "created_at": "1672531200",
    "updated_at": "1672531200"
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
    "url": "rtsp://192.168.1.100:554/stream1"
  }
}
```

**Camera Types:**
- `usb` - Local USB webcam
- `rtsp` - RTSP streaming camera
- `onvif` - ONVIF network camera
- `gb28181` - GB/T 28181 compliant camera
- `rtmp` - RTMP ingest camera

**Response (201 Created):**
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "Front Door",
  "camera_type": "rtsp",
  "config": {
    "url": "rtsp://192.168.1.100:554/stream1"
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
  -d '{
    "name": "Backyard Camera",
    "camera_type": "onvif",
    "config": {
      "host": "192.168.1.200",
      "port": 80,
      "username": "admin",
      "password": "password"
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
  "camera_type": "rtsp",
  "config": {
    "url": "rtsp://192.168.1.100:554/stream1"
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
    "url": "rtsp://updated-url:554/stream"
  },
  "status": "running"
}
```

**Response (200 OK):**
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "Updated Name",
  "camera_type": "rtsp",
  "config": {
    "url": "rtsp://updated-url:554/stream"
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
  -H "Cookie: session=$SESSION_TOKEN"
```

### Start Stream

**Endpoint:** `POST /api/cameras/{id}/start`

**Description:** Start capturing frames from a camera. Returns 409 if already running.

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
  -H "Cookie: session=$SESSION_TOKEN"
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
  -H "Cookie: session=$SESSION_TOKEN"
```

### Capture Snapshot

**Endpoint:** `GET /api/cameras/{id}/snapshot`

**Description:** Capture a single JPEG frame from a camera.

**Status:** Not yet implemented - returns 501.

**Response (501 Not Implemented):**
```json
{
  "error": "snapshot capture not yet implemented",
  "code": 501
}
```

**Example:**
```bash
curl -X GET https://localhost:8443/api/cameras/550e8400-e29b-41d4-a716-446655440000/snapshot \
  -H "Cookie: session=$SESSION_TOKEN"
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
  "notification_enabled": "true",
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
  -d '{
    "settings": {
      "theme": "light",
      "language": "en-US",
      "auto_discover": "true"
    }
  }'
```

---

## ONVIF Endpoints

### Discover ONVIF Devices

**Endpoint:** `GET /api/onvif/discover`

**Description:** Probe the local network for ONVIF cameras using WS-Discovery (UDP multicast on port 3702). Uses a 5-second timeout.

**Response (200 OK):**
```json
[
  {
    "xaddrs": [
      "http://192.168.1.100:80/onvif/device_service"
    ],
    "scopes": [
      "onvif://www.onvif.org/Profile/Streaming",
      "onvif://www.onvif.org/type/video"
    ],
    "types": ["dn:Device"],
    "endpoint": "soap-udp://192.168.1.100:3702"
  }
]
```

**Response (500 Internal Server Error):**
```json
{
  "error": "ONVIF discovery failed",
  "code": 500
}
```

**Example:**
```bash
curl -X GET https://localhost:8443/api/onvif/discover \
  -H "Cookie: session=$SESSION_TOKEN"
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
  "error": "rate limit exceeded"
}
```

### 503 Service Unavailable
```json
{
  "error": "SETUP_REQUIRED",
  "code": 503
}
```

### 501 Not Implemented
```json
{
  "error": "not implemented"
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
3. **Access:** Use returned session token for authenticated endpoints
4. **Configuration:** Add cameras, settings, and discover devices

## Example Session Flow

```bash
# 1. First run - setup admin user
curl -X POST https://localhost:8443/api/auth/setup \
  -H "Content-Type: application/json" \
  -d '{"username": "admin", "password": "securepass123"}'

# 2. Create a camera
curl -X POST https://localhost:8443/api/cameras \
  -H "Content-Type: application/json" \
  -H "Cookie: session=returned_session_token" \
  -d '{"name": "Test Camera", "camera_type": "rtsp", "config": {"url": "rtsp://192.168.1.100:554/stream"}}'

# 3. Start streaming
curl -X POST https://localhost:8443/api/cameras/camera_id/start \
  -H "Cookie: session=returned_session_token"

# 4. Check settings
curl -X GET https://localhost:8443/api/settings \
  -H "Cookie: session=returned_session_token"
```

## Rate Limiting

Login endpoints are rate-limited to 20 requests per 60 seconds per IP address to prevent brute force attacks.

## Session Management

- Session tokens expire after 24 hours
- All sessions are invalidated when password is reset
- Session cleanup happens automatically