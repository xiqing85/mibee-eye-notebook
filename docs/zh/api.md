# API 参考文档

## 概览

mibee-rec REST API 提供对摄像头管理、流媒体控制和系统配置的编程接口。所有 API 端点都使用 TLS 加密并需要适当的身份验证。

### 基础 URL
```
https://localhost:8443
```

### 内容类型
所有请求和响应都使用 JSON：
- 请求：`Content-Type: application/json`
- 响应：`Content-Type: application/json`

### 身份验证
API 使用基于 cookie 的会话身份验证。成功设置后，API 在 `Set-Cookie` 头中返回会话令牌，必须在后续请求中包含：

```bash
Cookie: session=$SESSION_TOKEN
```

### TLS 要求
所有 API 通信都需要 TLS/SSL。服务器在首次运行时如果不存在证书，会生成自签名证书。

---

## 身份验证

### 首次运行设置

**端点：** `POST /api/auth/setup`

**描述：** 创建初始管理员用户。此端点仅在不存在用户时可用（首次运行）。

**请求体：**
```json
{
  "username": "string",
  "password": "string"
}
```

**要求：**
- 用户名不能为空
- 密码必须至少 8 个字符

**响应（200 OK）：**
```json
{
  "status": "ok"
}
```

**响应（400 Bad Request）：**
```json
{
  "error": "already configured",
  "code": 400
}
```

**示例：**
```bash
curl -X POST https://localhost:8443/api/auth/setup \
  -H "Content-Type: application/json" \
  -d '{"username": "admin", "password": "securepass123"}'
```

### 会话管理

**注意：** 传统的登录/注销端点（`POST /api/auth/login` 和 `/POST /api/auth/logout`）尚未实现，返回 501。请使用密码重置端点替代。

### 密码重置

**端点：** `POST /api/auth/reset`

**描述：**更改您的密码。这需要使用有效的会话令牌进行身份验证。

**请求体：**
```json
{
  "old_password": "string",
  "new_password": "string"
}
```

**响应（200 OK）：**
```json
{
  "status": "ok"
}
```

**响应（401 Unauthorized）：**
```json
{
  "error": "incorrect password"
}
```

**示例：**
```bash
curl -X POST https://localhost:8443/api/auth/reset \
  -H "Content-Type: application/json" \
  -H "Cookie: session=$SESSION_TOKEN" \
  -d '{"old_password": "currentpass", "new_password": "newpass123"}'
```

---

## 公开端点

### 健康检查

**端点：** `GET /health`

**描述：**服务器健康状态和运行时间。在设置之前可用。

**响应（200 OK）：**
```json
{
  "status": "ok",
  "uptime": 3600
}
```

**示例：**
```bash
curl -X GET https://localhost:8443/health
```

### 指标

**端点：** `GET /metrics`

**描述：**Prometheus 指标（文本格式）。在设置之前可用。

**响应（200 OK）：**
```
# HELP mibee_rec_system_seconds System uptime in seconds
# TYPE mibee_rec_system_seconds counter
mibee_rec_system_seconds 3600
```

**示例：**
```bash
curl -X GET https://localhost:8443/metrics
```

---

## 摄像头端点

### 列出摄像头

**端点：** `GET /api/cameras`

**描述：**获取所有配置的摄像头。

**响应（200 OK）：**
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

**响应（500 Internal Server Error）：**
```json
{
  "error": "failed to list cameras",
  "code": 500
}
```

**示例：**
```bash
curl -X GET https://localhost:8443/api/cameras \
  -H "Cookie: session=$SESSION_TOKEN"
```

### 创建摄像头

**端点：** `POST /api/cameras`

**描述：**添加新的摄像头配置。

**请求体：**
```json
{
  "name": "string",
  "camera_type": "string",
  "config": {
    "url": "rtsp://192.168.1.100:554/stream1"
  }
}
```

**摄像头类型：**
- `usb` - 本地 USB 网络摄像头
- `rtsp` - RTSP 流媒体摄像头
- `onvif` - ONVIF 网络摄像头
- `gb28181` - GB/T 28181 兼容摄像头
- `rtmp` - RTMP 推送摄像头

**响应（201 Created）：**
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

**响应（400 Bad Request）：**
```json
{
  "error": "failed to create camera",
  "code": 400
}
```

**示例：**
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

### 获取摄像头

**端点：** `GET /api/cameras/{id}`

**描述：**根据 ID 获取特定摄像头。

**响应（200 OK）：**
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

**响应（404 Not Found）：**
```json
{
  "error": "camera not found",
  "code": 404
}
```

**响应（500 Internal Server Error）：**
```json
{
  "error": "failed to get camera",
  "code": 500
}
```

**示例：**
```bash
curl -X GET https://localhost:8443/api/cameras/550e8400-e29b-41d4-a716-446655440000 \
  -H "Cookie: session=$SESSION_TOKEN"
```

### 更新摄像头

**端点：** `PUT /api/cameras/{id}`

**描述：**更新摄像头配置。支持部分更新。

**请求体：**
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

**响应（200 OK）：**
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

**响应（404 Not Found）：**
```json
{
  "error": "camera not found",
  "code": 404
}
```

**示例：**
```bash
curl -X PUT https://localhost:8443/api/cameras/550e8400-e29b-41d4-a716-446655440000 \
  -H "Content-Type: application/json" \
  -H "Cookie: session=$SESSION_TOKEN" \
  -d '{
    "name": "Updated Camera Name",
    "status": "stopped"
  }'
```

### 删除摄像头

**端点：** `DELETE /api/cameras/{id}`

**描述：**删除摄像头配置。

**响应（200 OK）：**
```json
{
  "status": "ok"
}
```

**响应（404 Not Found）：**
```json
{
  "error": "camera not found",
  "code": 404
}
```

**响应（500 Internal Server Error）：**
```json
{
  "error": "failed to delete camera",
  "code": 500
}
```

**示例：**
```bash
curl -X DELETE https://localhost:8443/api/cameras/550e8400-e29b-41d4-a716-446655440000 \
  -H "Cookie: session=$SESSION_TOKEN"
```

### 启动流媒体

**端点：** `POST /api/cameras/{id}/start`

**描述：**开始从摄像头捕获帧。如果已在运行，返回 409。

**响应（200 OK）：**
```json
{
  "status": "ok",
  "camera_id": "550e8400-e29b-41d4-a716-446655440000"
}
```

**响应（404 Not Found）：**
```json
{
  "error": "camera not found",
  "code": 404
}
```

**响应（409 Conflict）：**
```json
{
  "error": "stream already running",
  "code": 409
}
```

**响应（500 Internal Server Error）：**
```json
{
  "error": "failed to start stream",
  "code": 500
}
```

**示例：**
```bash
curl -X POST https://localhost:8443/api/cameras/550e8400-e29b-41d4-a716-446655440000/start \
  -H "Cookie: session=$SESSION_TOKEN"
```

### 停止流媒体

**端点：** `POST /api/cameras/{id}/stop`

**描述：**停止从摄像头捕获帧。

**响应（200 OK）：**
```json
{
  "status": "ok",
  "camera_id": "550e8400-e29b-41d4-a716-446655440000"
}
```

**响应（404 Not Found）：**
```json
{
  "error": "camera not found",
  "code": 404
}
```

**响应（500 Internal Server Error）：**
```json
{
  "error": "failed to stop stream",
  "code": 500
}
```

**示例：**
```bash
curl -X POST https://localhost:8443/api/cameras/550e8400-e29b-41d4-a716-446655440000/stop \
  -H "Cookie: session=$SESSION_TOKEN"
```

### 捕获快照

**端点：** `GET /api/cameras/{id}/snapshot`

**描述：**从摄像头捕获单个 JPEG 帧。

**状态：**尚未实现 - 返回 501。

**响应（501 Not Implemented）：**
```json
{
  "error": "snapshot capture not yet implemented",
  "code": 501
}
```

**示例：**
```bash
curl -X GET https://localhost:8443/api/cameras/550e8400-e29b-41d4-a716-446655440000/snapshot \
  -H "Cookie: session=$SESSION_TOKEN"
```

---

## 设置端点

### 获取设置

**端点：** `GET /api/settings`

**描述：**检索所有系统设置作为键值对。

**响应（200 OK）：**
```json
{
  "theme": "dark",
  "language": "en-US",
  "notification_enabled": "true",
  "max_concurrent_streams": "16"
}
```

**响应（500 Internal Server Error）：**
```json
{
  "error": "failed to list settings",
  "code": 500
}
```

**示例：**
```bash
curl -X GET https://localhost:8443/api/settings \
  -H "Cookie: session=$SESSION_TOKEN"
```

### 更新设置

**端点：** `PUT /api/settings`

**描述：**更新一个或多个系统设置。

**请求体：**
```json
{
  "settings": {
    "theme": "light",
    "language": "zh-CN",
    "max_concurrent_streams": "8"
  }
}
```

**响应（200 OK）：**
```json
{
  "status": "ok"
}
```

**响应（500 Internal Server Error）：**
```json
{
  "error": "failed to update settings",
  "code": 500
}
```

**示例：**
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

## ONVIF 端点

### 发现 ONVIF 设备

**端点：** `GET /api/onvif/discover`

**描述：**使用 WS-Discovery 探测本地网络中的 ONVIF 摄像头（UDP 多播端口 3702）。使用 5 秒超时。

**响应（200 OK）：**
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

**响应（500 Internal Server Error）：**
```json
{
  "error": "ONVIF discovery failed",
  "code": 500
}
```

**示例：**
```bash
curl -X GET https://localhost:8443/api/onvif/discover \
  -H "Cookie: session=$SESSION_TOKEN"
```

---

## 错误响应

所有错误响应都遵循一致的格式：

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

## 设置流程

1. **首次访问：** 大多数端点响应 503
2. **设置：** 调用 `POST /api/auth/setup` 创建管理员用户
3. **访问：** 使用返回的会话令牌访问认证端点
4. **配置：** 添加摄像头、设置和发现设备

## 示例会话流程

```bash
# 1. 首次运行 - 设置管理员用户
curl -X POST https://localhost:8443/api/auth/setup \
  -H "Content-Type: application/json" \
  -d '{"username": "admin", "password": "securepass123"}'

# 2. 创建摄像头
curl -X POST https://localhost:8443/api/cameras \
  -H "Content-Type: application/json" \
  -H "Cookie: session=returned_session_token" \
  -d '{"name": "Test Camera", "camera_type": "rtsp", "config": {"url": "rtsp://192.168.1.100:554/stream"}}'

# 3. 启动流媒体
curl -X POST https://localhost:8443/api/cameras/camera_id/start \
  -H "Cookie: session=returned_session_token"

# 4. 检查设置
curl -X GET https://localhost:8443/api/settings \
  -H "Cookie: session=returned_session_token"
```

## 速率限制

登录端点受速率限制，每 IP 地址每 60 秒最多 20 个请求，以防止暴力攻击。

## 会话管理

- 会话令牌在 24 小时后过期
- 重置密码时所有会话均失效
- 会话清理自动进行