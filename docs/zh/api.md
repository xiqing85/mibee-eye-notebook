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

**注意：** 二进制响应（快照、实时预览）使用适当的内容类型（例如 `image/jpeg`、`multipart/x-mixed-replace`）。

### 身份验证
API 使用基于 cookie 的会话身份验证。成功登录后，API 在 `Set-Cookie` 头中返回会话令牌，必须在后续请求中包含：

```bash
Cookie: session=$SESSION_TOKEN
```

此外，还会作为非 HttpOnly cookie 颁发 CSRF 令牌并在响应体中返回。所有状态变更请求（POST/PUT/DELETE/PATCH）必须在 `X-CSRF-Token` 头中包含 CSRF 令牌。

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

---

### 登录

**端点：** `POST /api/auth/login`

**描述：** 认证并创建会话。成功后，设置会话 cookie（HttpOnly）和 CSRF cookie（非 HttpOnly），并在 JSON 响应中返回 CSRF 令牌。

**请求体：**
```json
{
  "username": "string",
  "password": "string"
}
```

**响应（200 OK）：**
- **头部：**
  - `Set-Cookie: session=<token>; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=86400`
  - `Set-Cookie: csrf-token=<token>; SameSite=Strict; Path=/; Max-Age=86400`
- **主体：**
```json
{
  "status": "ok",
  "csrf_token": "<token>"
}
```

**响应（401 Unauthorized）：**
```json
{
  "error": "invalid credentials"
}
```

**响应（429 Too Many Requests）：**
```json
{
  "error": "account locked, try again in N seconds"
}
```

**示例：**
```bash
curl -X POST https://localhost:8443/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username": "admin", "password": "securepass123"}'
```

---

### 注销

**端点：** `POST /api/auth/logout`

**描述：** 使当前会话失效。清除会话 cookie。无需身份验证（清除任何存在的会话）。

**响应（200 OK）：**
- **头部：**
  - `Set-Cookie: session=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0`
- **主体：**
```json
{
  "status": "ok"
}
```

**示例：**
```bash
curl -X POST https://localhost:8443/api/auth/logout
```

---

### 密码重置

**端点：** `POST /api/auth/reset`

**描述：** 更改您的密码。这需要使用有效的会话令牌进行身份验证。

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

**描述：** 服务器健康状态和运行时间。在设置之前可用。

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

**描述：** Prometheus 指标（文本格式）。在设置之前可用。

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

**描述：** 获取所有配置的摄像头。

**响应（200 OK）：**
```json
[
  {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "name": "前门",
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

**描述：** 添加新的摄像头配置。

**请求体：**
```json
{
  "name": "string",
  "camera_type": "string",
  "config": {
    "device_index": 0
  }
}
```

**摄像头类型：**
- `usb` - 本地 USB 网络摄像头（此产品的主要类型）
- `rtsp` - RTSP 流媒体摄像头
- `onvif` - ONVIF 网络摄像头
- `gb28181` - GB/T 28181 兼容摄像头
- `rtmp` - RTMP 推送摄像头

**注意：** 此产品专为**本地捕获**设计 - 它从物理连接的设备（通过 V4L2 的 USB 网络摄像头）捕获。虽然数据库允许其他摄像头类型，但主要用例是通过 `usb` 类型进行本地网络摄像头捕获。

**响应（201 Created）：**
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "前门",
  "camera_type": "usb",
  "config": {
    "device_index": 0
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
  -H "X-CSRF-Token: $CSRF_TOKEN" \
  -d '{
    "name": "网络摄像头",
    "camera_type": "usb",
    "config": {
      "device_index": 0
    }
  }'
```

### 获取摄像头

**端点：** `GET /api/cameras/{id}`

**描述：** 根据 ID 获取特定摄像头。

**响应（200 OK）：**
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "前门",
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

**描述：** 更新摄像头配置。支持部分更新。

**请求体：**
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

**响应（200 OK）：**
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "更新名称",
  "camera_type": "usb",
  "config": {
    "device_index": 1
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
  -H "X-CSRF-Token: $CSRF_TOKEN" \
  -d '{
    "name": "更新摄像头名称",
    "status": "stopped"
  }'
```

### 删除摄像头

**端点：** `DELETE /api/cameras/{id}`

**描述：** 删除摄像头配置。

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
  -H "Cookie: session=$SESSION_TOKEN" \
  -H "X-CSRF-Token: $CSRF_TOKEN"
```

### 启动流媒体

**端点：** `POST /api/cameras/{id}/start`

**描述：** 开始从摄像头捕获帧。如果已在运行，返回 409。

**响应（200 OK）：**
```json
{
  "status": "running",
  "rtsp_url": "rtsp://localhost:8554/live/550e8400-e29b-41d4-a716-446655440000",
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
  -H "Cookie: session=$SESSION_TOKEN" \
  -H "X-CSRF-Token: $CSRF_TOKEN"
```

### 停止流媒体

**端点：** `POST /api/cameras/{id}/stop`

**描述：** 停止从摄像头捕获帧。

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
  -H "Cookie: session=$SESSION_TOKEN" \
  -H "X-CSRF-Token: $CSRF_TOKEN"
```

### 捕获快照

**端点：** `GET /api/cameras/{id}/snapshot`

**描述：** 使用 ffmpeg 从摄像头捕获单个 JPEG 帧。摄像头流必须处于活动状态。

**要求：**
- 摄像头的流必须处于活动状态（否则返回 409）
- 必须安装 ffmpeg 并在 PATH 中可用

**响应（200 OK）：**
- **头部：**
  - `Content-Type: image/jpeg`
  - `Content-Length: <size>`
- **主体：** JPEG 二进制图像数据

**响应（404 Not Found）：**
摄像头不存在

**响应（409 Conflict）：**
流未活动 - 请先启动流

**响应（504 Gateway Timeout）：**
捕获时间超过 30 秒

**示例：**
```bash
# 捕获快照并保存到文件
curl -X GET https://localhost:8443/api/cameras/550e8400-e29b-41d4-a716-446655440000/snapshot \
  -H "Cookie: session=$SESSION_TOKEN" \
  -o snapshot.jpg
```

---

### 实时预览

**端点：** `GET /api/cameras/{id}/live`

**描述：** 使用 MJPEG 多部分格式的浏览器实时预览流。适合直接在 `<img src=...>` 标签中使用。摄像头流必须处于活动状态。

**要求：**
- 摄像头的流必须处于活动状态（否则返回 409）
- 必须安装 ffmpeg 并在 PATH 中可用

**响应（200 OK）：**
- **头部：**
  - `Content-Type: multipart/x-mixed-replace; boundary=ffmpeg`
  - `Cache-Control: no-store, no-cache, must-revalidate`
- **主体：** 连续的 MJPEG 流（由边界分隔的 JPEG 帧）

**响应（404 Not Found）：**
摄像头不存在

**响应（409 Conflict）：**
流未活动 - 请先启动流

**示例：**
```bash
# 在 HTML 中：
# <img src="/api/cameras/{id}/live">
#
# 浏览器自动处理 MJPEG 流并显示实时视频。
```

---

## 设置端点

### 获取设置

**端点：** `GET /api/settings`

**描述：** 检索所有系统设置作为键值对。

**响应（200 OK）：**
```json
{
  "theme": "dark",
  "language": "en-US",
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

**描述：** 更新一个或多个系统设置。

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
  -H "X-CSRF-Token: $CSRF_TOKEN" \
  -d '{
    "settings": {
      "theme": "light",
      "language": "en-US"
    }
  }'
```

---

## 协议配置端点

### 获取协议配置

**端点：** `GET /api/protocols/{onvif|gb28181|rtmp}`

**描述：** 检索指定协议的当前配置。

**响应（200 OK）：**
```json
{
  "enabled": true,
  // ... 协议特定字段
}
```

**示例：**
```bash
curl -X GET https://localhost:8443/api/protocols/onvif \
  -H "Cookie: session=$SESSION_TOKEN"
```

---

### 更新协议配置

**端点：** `PUT /api/protocols/{onvif|gb28181|rtmp}`

**描述：** 更新指定协议的配置。持久化到 SQLite 并根据 `enabled` 标志热切换协议运行时（无需服务器重启）。

**请求体：**
```json
{
  "enabled": true,
  // ... 协议特定字段
}
```

**响应（200 OK）：**
```json
{
  "status": "ok"
}
```

**示例：**
```bash
curl -X PUT https://localhost:8443/api/protocols/onvif \
  -H "Content-Type: application/json" \
  -H "Cookie: session=$SESSION_TOKEN" \
  -H "X-CSRF-Token: $CSRF_TOKEN" \
  -d '{"enabled": true, "device_name": "FrontDoor"}'
```

---

### 获取协议运行时状态

**端点：** `GET /api/protocols/runtime-status`

**描述：** 获取所有协议的运行/停止状态。

**响应（200 OK）：**
```json
{
  "onvif": "running",
  "gb28181": "stopped",
  "rtmp": "running"
}
```

**示例：**
```bash
curl -X GET https://localhost:8443/api/protocols/runtime-status \
  -H "Cookie: session=$SESSION_TOKEN"
```

---

## 设备枚举端点

### 列出视频设备

**端点：** `GET /api/devices/video`

**描述：** 使用 V4L2 枚举所有可用的本地视频捕获设备（网络摄像头）。

**响应（200 OK）：**
```json
[
  {
    "index": 0,
    "name": "/dev/video0",
    "formats": ["YUYV 640x480", "MJPEG 1280x720"]
  }
]
```

**示例：**
```bash
curl -X GET https://localhost:8443/api/devices/video \
  -H "Cookie: session=$SESSION_TOKEN"
```

---

### 列出音频设备

**端点：** `GET /api/devices/audio`

**描述：** 使用 ALSA 枚举所有可用的本地音频输入设备。

**响应（200 OK）：**
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

**示例：**
```bash
curl -X GET https://localhost:8443/api/devices/audio \
  -H "Cookie: session=$SESSION_TOKEN"
```

---

## 服务器发送事件（SSE）

### 摄像头事件流

**端点：** `GET /api/events`

**描述：** 用于实时摄像头热插拔事件的服务器发送事件流。当摄像头插入或拔出时接收 `camera_added` 和 `camera_offlined` 事件。

**事件类型：**
- `camera_added` - 发现新摄像头
- `camera_offlined` - 摄像头离线（已拔出）

**事件格式：**
```
event: camera_added
data: {"camera_id":"...","device_index":0,"name":"..."}

event: camera_offlined
data: {"camera_id":"...","device_index":0}

```

**示例：**
```bash
# SSE 端点返回连续事件流
curl -N https://localhost:8443/api/events \
  -H "Cookie: session=$SESSION_TOKEN"
```

---

## 安全功能

### CSRF 保护

所有状态变更请求（POST、PUT、DELETE、PATCH）都需要 CSRF 令牌以防止跨站请求伪造攻击。

**令牌颁发：**
- 成功登录后，CSRF 令牌作为名为 `csrf-token` 的非 HttpOnly cookie 颁发
- 同样的令牌也在 JSON 响应体中作为 `csrf_token` 返回

**令牌使用：**
- 在所有 POST/PUT/DELETE/PATCH 请求的 `X-CSRF-Token` 头中包含令牌
- 令牌值必须匹配 `csrf-token` cookie 值（双重提交模式）

**示例：**
```bash
curl -X POST https://localhost:8443/api/cameras \
  -H "Content-Type: application/json" \
  -H "Cookie: session=$SESSION_TOKEN; csrf-token=<token>" \
  -H "X-CSRF-Token: <token>" \
  -d '{"name":"Test","camera_type":"usb"}'
```

**失败：** 令牌缺失或不匹配返回 403 Forbidden。

---

### 速率限制

登录端点受速率限制以防止暴力攻击。

**默认限制：**
- 每个 IP 地址每 60 秒 20 个请求
- 成功登录后重置速率限制计数器

**账户锁定：**
- 5 次登录失败后，账户被锁定
- 锁定时间随着每次额外的失败而加倍：60s → 120s → 240s → ...
- 成功登录重置失败计数器

**速率限制响应：**
```json
{
  "error": "account locked, try again in 60 seconds"
}
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

## 设置流程

1. **首次访问：** 大多数端点响应 503
2. **设置：** 调用 `POST /api/auth/setup` 创建管理员用户
3. **访问：** 使用 `POST /api/auth/login` 登录以获取会话令牌和 CSRF 令牌
4. **配置：** 添加摄像头、设置和发现设备

## 示例会话流程

```bash
# 1. 首次运行 - 设置管理员用户
curl -X POST https://localhost:8443/api/auth/setup \
  -H "Content-Type: application/json" \
  -d '{"username": "admin", "password": "securepass123"}'

# 2. 登录以获取会话和 CSRF 令牌
LOGIN_RESPONSE=$(curl -X POST https://localhost:8443/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username": "admin", "password": "securepass123"}' \
  -c cookies.txt)

CSRF_TOKEN=$(echo $LOGIN_RESPONSE | jq -r '.csrf_token')

# 3. 创建摄像头（需要 CSRF 令牌）
curl -X POST https://localhost:8443/api/cameras \
  -H "Content-Type: application/json" \
  -b cookies.txt \
  -H "X-CSRF-Token: $CSRF_TOKEN" \
  -d '{"name": "网络摄像头", "camera_type": "usb", "config": {"device_index": 0}}'

# 4. 启动流媒体（需要 CSRF 令牌）
curl -X POST https://localhost:8443/api/cameras/<camera_id>/start \
  -b cookies.txt \
  -H "X-CSRF-Token: $CSRF_TOKEN"

# 5. 检查设置
curl -X GET https://localhost:8443/api/settings \
  -b cookies.txt
```

---

## 会话管理

- 会话令牌在 24 小时后过期
- 重置密码时所有会话均失效
- 会话清理自动进行（每 5 分钟）
- 成功登录后重置速率限制计数器